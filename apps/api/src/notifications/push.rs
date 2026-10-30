//! Web Push (RFC 8030) with VAPID (RFC 8292), for the reminders sent as a
//! notification (#306) and the budget alerts (#536).
//!
//! **No payload.** A push message carries no body at all: the service
//! worker (`apps/web/src/sw.js`) asks the site which kind of push its
//! device got ([`Pending`]) and shows that kind's fixed text — « Rappel
//! d'un événement à venir », « Alerte de budget », a neutral one when it
//! cannot tell —, and the event's title is read in the
//! application, after the click. So nothing about the event — not
//! its title, not an identifier — ever reaches the push service (Mozilla,
//! Google, Apple, Microsoft), and there is no payload to encrypt
//! (RFC 8291): the subscription's `p256dh`/`auth` keys are not even
//! stored. What the push service learns is that this server sent this
//! device a message at that time.
//!
//! **The endpoint is user input that the server then POSTs to.** Left
//! open, `POST /account/push-subscriptions` would make the reminder worker
//! request any URL a member typed — an internal address, the metadata
//! service of the host. [`validate_endpoint`] admits HTTPS on the default
//! port to the hosts of the browsers' push services, and nothing else; the
//! client follows no redirect ([`client`]).

use std::time::Duration as StdDuration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use reqwest::Url;

/// Push services whose host is matched exactly.
const EXACT_HOSTS: &[&str] = &[
    // Chrome, and the Chromium browsers that keep Google's service
    // (Opera, Brave, Samsung Internet).
    "fcm.googleapis.com",
    // Firefox.
    "updates.push.services.mozilla.com",
];

/// Push services matched on a domain and any of its subdomains.
const HOST_SUFFIXES: &[&str] = &[
    // Safari (macOS 13+, iOS/iPadOS 16.4+ from the home screen):
    // `web.push.apple.com` today.
    "push.apple.com",
    // Edge: `<region>.notify.windows.com`.
    "notify.windows.com",
];

/// Longest endpoint taken. Real ones run to a few hundred characters
/// (FCM's are the longest, around 200).
pub const MAX_ENDPOINT_LEN: usize = 1024;

/// Why an endpoint is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointError {
    TooLong,
    NotAUrl,
    NotHttps,
    /// User info, an explicit port or an IP address: no push service's
    /// endpoint has any of them.
    UnexpectedPart,
    UnknownPushService,
}

/// The endpoint parsed, if it is a push service's: `https`, default port,
/// no user info, a host in [`EXACT_HOSTS`] or under one of
/// [`HOST_SUFFIXES`].
pub fn validate_endpoint(raw: &str) -> Result<Url, EndpointError> {
    if raw.len() > MAX_ENDPOINT_LEN {
        return Err(EndpointError::TooLong);
    }
    let url = Url::parse(raw).map_err(|_| EndpointError::NotAUrl)?;
    if url.scheme() != "https" {
        return Err(EndpointError::NotHttps);
    }
    // `port()` is `None` for the scheme's default, written out or not.
    if !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return Err(EndpointError::UnexpectedPart);
    }
    // `domain()` is `None` for an IP address host, and for no host at all.
    let Some(host) = url.domain() else {
        return Err(match url.host_str() {
            Some(_) => EndpointError::UnexpectedPart,
            None => EndpointError::NotAUrl,
        });
    };
    let known = EXACT_HOSTS.contains(&host)
        || HOST_SUFFIXES.iter().any(|suffix| {
            host == *suffix
                || host
                    .strip_suffix(suffix)
                    .is_some_and(|label| label.ends_with('.'))
        });
    if !known {
        return Err(EndpointError::UnknownPushService);
    }
    Ok(url)
}

/// The `aud` claim of the VAPID token for `endpoint`: its origin.
pub fn audience(endpoint: &Url) -> String {
    endpoint.origin().ascii_serialization()
}

/// What a push service's answer means for the subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    /// Accepted (201, or any 2xx).
    Delivered,
    /// 404 or 410: the subscription expired or was withdrawn by the
    /// browser. It is deleted, never retried.
    Gone,
    /// Refused by the push service (any other status), or never sent:
    /// worth another attempt. The device never gets this push.
    Failed(String),
    /// No answer from the push service: a timeout, a connection that
    /// failed or broke off. Worth another attempt, like [`Failed`]
    /// (`PushOutcome::Failed`) — but the push service may have stored the
    /// message before the connection was lost, so the device may still
    /// get it ([`pending_after_unanswered`]).
    Unanswered(String),
}

/// Classifies a push service's HTTP status.
pub fn classify(status: u16) -> PushOutcome {
    match status {
        200..=299 => PushOutcome::Delivered,
        404 | 410 => PushOutcome::Gone,
        other => PushOutcome::Failed(format!("push service answered {other}")),
    }
}

/// A device whose push service never answers 404 or 410 but keeps failing
/// (an endpoint on a host that no longer resolves, a service that refuses
/// it for good) is forgotten once **both** hold, with no delivery in
/// between: at least [`MAX_CONSECUTIVE_FAILURES`] failed pushes in a row,
/// and the first of them at least [`MIN_FAILING_DAYS`] days old.
///
/// The count alone would not do: several reminders falling due in the same
/// pass, each attempted 5 times a minute apart, reach 20 failures within a
/// five-minute outage. The duration is what tells a dead endpoint from an
/// outage: a push service down for a week on end is not a case to plan for.
/// The count keeps a device that was pushed only once or twice that week
/// from going on so little evidence. A device forgotten wrongly costs
/// little: the member gets the « no device » warning, and the page
/// registers the device again on its next visit, the permission being
/// still granted.
pub const MAX_CONSECUTIVE_FAILURES: i32 = 20;

/// See [`MAX_CONSECUTIVE_FAILURES`].
pub const MIN_FAILING_DAYS: i64 = 7;

/// What becomes of a device once `outcome` is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceAfter {
    /// Deleted: gone (404/410), or failing past both bounds.
    Forget,
    /// Kept, with these failure bookkeeping values: `(0, None)` after a
    /// delivery.
    Keep {
        consecutive_failures: i32,
        failing_since: Option<DateTime<Utc>>,
    },
}

/// The device's fate after `outcome`, from its failures in a row so far and
/// the time of the first of them (`None` when its last push went through).
pub fn device_after(
    outcome: &PushOutcome,
    consecutive_failures: i32,
    failing_since: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> DeviceAfter {
    match outcome {
        PushOutcome::Delivered => DeviceAfter::Keep {
            consecutive_failures: 0,
            failing_since: None,
        },
        PushOutcome::Gone => DeviceAfter::Forget,
        PushOutcome::Failed(_) | PushOutcome::Unanswered(_) => {
            let failures = consecutive_failures.saturating_add(1);
            let since = failing_since.unwrap_or(now);
            if failures >= MAX_CONSECUTIVE_FAILURES
                && now - since >= chrono::Duration::days(MIN_FAILING_DAYS)
            {
                DeviceAfter::Forget
            } else {
                DeviceAfter::Keep {
                    consecutive_failures: failures,
                    failing_since: Some(since),
                }
            }
        }
    }
}

/// Shortest TTL asked for, in seconds.
pub const MIN_TTL_SECS: i64 = 60;
/// Longest TTL asked for: four weeks, what the push services keep at most.
pub const MAX_TTL_SECS: i64 = 4 * 7 * 24 * 3600;

/// How long the push service may hold the message for a device that is
/// offline: until the occurrence starts. A reminder delivered after the
/// event has begun is noise. Bounded by [`MIN_TTL_SECS`] (a reminder sent
/// late, or at the very start, still gets a minute) and [`MAX_TTL_SECS`].
pub fn ttl_secs(now: DateTime<Utc>, occurrence_at: DateTime<Utc>) -> i64 {
    (occurrence_at - now)
        .num_seconds()
        .clamp(MIN_TTL_SECS, MAX_TTL_SECS)
}

/// A kind of notification pushed to the devices, under its name in
/// `notifications.kind`. The service worker shows a fixed text for each
/// (`apps/web/src/sw.js`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushKind {
    /// An agenda reminder come due (#306).
    AgendaReminder,
    /// A budget cap alert (#536).
    BudgetAlert,
}

impl PushKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PushKind::AgendaReminder => "agenda_reminder",
            PushKind::BudgetAlert => "budget_alert",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        [PushKind::AgendaReminder, PushKind::BudgetAlert]
            .into_iter()
            .find(|kind| kind.as_str() == raw)
    }
}

/// The kind of the pushes a device has pending: one kind, or several.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    Only(PushKind),
    Mixed,
}

impl PendingKind {
    /// The value of `push_subscriptions.pending_push_kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            PendingKind::Only(kind) => kind.as_str(),
            PendingKind::Mixed => "mixed",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "mixed" => Some(PendingKind::Mixed),
            _ => PushKind::parse(raw).map(PendingKind::Only),
        }
    }
}

/// The pushes sent to a device that it has not shown yet
/// (`push_subscriptions.pending_*`, migration 0037), so that the service
/// worker shows the text of the push it got, and not that of another.
///
/// A push carries nothing: the worker asks on each one which kind it is
/// ([`pending_on_receipt`]). Each push is counted before it is sent
/// ([`pending_after_push`]) and each receipt takes one off. While the
/// pushes pending are all of one kind, a receipt can only be one of them;
/// once two kinds are pending — an alert, and a reminder sent again by a
/// retry — none can tell which it is, and the worker shows the neutral
/// text until they are all received. Never another kind's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pending {
    /// `None` with nothing pending.
    pub kind: Option<PendingKind>,
    pub count: i32,
    /// When the last of them expires at the push service (its TTL): past
    /// it, none can arrive any more.
    pub until: Option<DateTime<Utc>>,
}

impl Pending {
    /// From the columns of `push_subscriptions`.
    pub fn from_columns(kind: Option<&str>, count: i32, until: Option<DateTime<Utc>>) -> Self {
        if count <= 0 {
            return Pending::default();
        }
        Pending {
            kind: Some(
                kind.and_then(PendingKind::parse)
                    .unwrap_or(PendingKind::Mixed),
            ),
            count,
            until,
        }
    }

    /// Pushes may still arrive.
    fn live(&self, now: DateTime<Utc>) -> bool {
        self.count > 0 && self.until.is_some_and(|until| until > now)
    }
}

/// `pending` once a push of `kind`, held `ttl_secs` by the push service,
/// is about to be sent at `now`.
pub fn pending_after_push(
    pending: Pending,
    kind: PushKind,
    now: DateTime<Utc>,
    ttl_secs: i64,
) -> Pending {
    let until = now + chrono::Duration::seconds(ttl_secs);
    if !pending.live(now) {
        return Pending {
            kind: Some(PendingKind::Only(kind)),
            count: 1,
            until: Some(until),
        };
    }
    let kind = match pending.kind {
        Some(PendingKind::Only(pending_kind)) if pending_kind == kind => PendingKind::Only(kind),
        _ => PendingKind::Mixed,
    };
    Pending {
        kind: Some(kind),
        count: pending.count.saturating_add(1),
        until: pending.until.max(Some(until)),
    }
}

/// `pending` once a push counted was refused by the push service, with an
/// HTTP answer ([`PushOutcome::Failed`], [`PushOutcome::Gone`]), or never
/// sent: it never arrives. Its kind is not taken back: which of the others
/// are left is not known. Not for a push that got no answer — a timeout,
/// a connection lost —: that one may have been stored and arrive
/// ([`pending_after_unanswered`]).
pub fn pending_after_failure(pending: Pending) -> Pending {
    let count = pending.count.saturating_sub(1).max(0);
    if count == 0 {
        return Pending::default();
    }
    Pending { count, ..pending }
}

/// `pending` once a push counted got no answer from the push service
/// ([`PushOutcome::Unanswered`]): it may have been stored and arrive, or
/// not. It stays counted, and the kind becomes mixed: whether a receipt is
/// this one or another is no longer known, so every push received shows
/// the neutral text until those pending are all received, or past their
/// TTL.
pub fn pending_after_unanswered(pending: Pending) -> Pending {
    if pending.count <= 0 {
        return Pending::default();
    }
    Pending {
        kind: Some(PendingKind::Mixed),
        ..pending
    }
}

/// What a device shows on a push received at `now`: the kind of the pushes
/// pending if they are all of one, `None` — the neutral text — otherwise,
/// or with none pending. And `pending` with this one taken off.
pub fn pending_on_receipt(pending: Pending, now: DateTime<Utc>) -> (Option<PushKind>, Pending) {
    if !pending.live(now) {
        return (None, Pending::default());
    }
    let shown = match pending.kind {
        Some(PendingKind::Only(kind)) => Some(kind),
        _ => None,
    };
    let count = pending.count - 1;
    let after = if count == 0 {
        Pending::default()
    } else {
        Pending { count, ..pending }
    };
    (shown, after)
}

/// How long a VAPID token is valid. RFC 8292 caps it at 24 hours.
const TOKEN_LIFETIME_SECS: i64 = 12 * 3600;

/// Why the VAPID configuration is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VapidError {
    /// Not 32 bytes of unpadded base64url, or not a valid P-256 scalar.
    BadPrivateKey,
    /// Not a `mailto:` or `https:` URI (RFC 8292 §2.1).
    BadSubject,
}

/// The server's VAPID identity: the P-256 key every push is signed with,
/// and the contact the push services may write to.
#[derive(Clone)]
pub struct Vapid {
    key: SigningKey,
    subject: String,
    public_key: String,
}

impl std::fmt::Debug for Vapid {
    // Never the private key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vapid")
            .field("subject", &self.subject)
            .field("public_key", &self.public_key)
            .finish_non_exhaustive()
    }
}

/// Environment variable holding the private key: the raw 32-byte P-256
/// scalar, unpadded base64url (apps/api/README.md says how to make one).
pub const PRIVATE_KEY_VAR: &str = "VAPID_PRIVATE_KEY";
/// Environment variable holding the contact URI (`mailto:…`).
pub const SUBJECT_VAR: &str = "VAPID_SUBJECT";

impl Vapid {
    /// From the private scalar (unpadded base64url) and the subject.
    pub fn new(private_key: &str, subject: &str) -> Result<Self, VapidError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(private_key.trim())
            .map_err(|_| VapidError::BadPrivateKey)?;
        if bytes.len() != 32 {
            return Err(VapidError::BadPrivateKey);
        }
        let key = SigningKey::from_slice(&bytes).map_err(|_| VapidError::BadPrivateKey)?;
        let subject_ok = subject
            .strip_prefix("mailto:")
            .or_else(|| subject.strip_prefix("https://"))
            .is_some_and(|rest| !rest.is_empty());
        if !subject_ok {
            return Err(VapidError::BadSubject);
        }
        let public_key =
            URL_SAFE_NO_PAD.encode(key.verifying_key().to_sec1_point(false).as_bytes());
        Ok(Vapid {
            key,
            subject: subject.to_string(),
            public_key,
        })
    }

    /// From [`PRIVATE_KEY_VAR`] and [`SUBJECT_VAR`]: `Ok(None)` when the
    /// key is unset or empty — notifications are then off on this server,
    /// and every member is told so where they would subscribe — and an
    /// error when it is set but unusable, so a typo stops the start
    /// instead of silently turning them off.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let key = std::env::var(PRIVATE_KEY_VAR).unwrap_or_default();
        if key.trim().is_empty() {
            return Ok(None);
        }
        let subject = std::env::var(SUBJECT_VAR).unwrap_or_default();
        Vapid::new(&key, &subject)
            .map(Some)
            .map_err(|e| anyhow::anyhow!("{PRIVATE_KEY_VAR} / {SUBJECT_VAR}: {e:?}"))
    }

    /// The public key, uncompressed point (65 bytes) in unpadded
    /// base64url: the `applicationServerKey` a browser subscribes with.
    pub fn public_key(&self) -> &str {
        &self.public_key
    }

    /// The `Authorization` header for a push to `audience`, valid
    /// [`TOKEN_LIFETIME_SECS`] from `now` (RFC 8292 §3).
    pub fn authorization(&self, audience: &str, now: DateTime<Utc>) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = serde_json::json!({
            "aud": audience,
            "exp": now.timestamp() + TOKEN_LIFETIME_SECS,
            "sub": self.subject,
        });
        let claims = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signing_input = format!("{header}.{claims}");
        // ES256's signature is r || s, 64 bytes (RFC 7518 §3.4), which is
        // what `Signature::to_bytes` gives — not the DER form.
        let signature: Signature = self.key.sign(signing_input.as_bytes());
        let signature = URL_SAFE_NO_PAD.encode(signature.to_bytes());
        format!("vapid t={signing_input}.{signature}, k={}", self.public_key)
    }
}

/// The client every push goes through: no redirect is followed (an
/// endpoint that answers 3xx gets a failure, not a request somewhere
/// else), and a push service that does not answer in 10 s is a failure.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(StdDuration::from_secs(10))
        .build()
        .expect("static reqwest configuration")
}

/// Sends one push without payload to `endpoint`. The endpoint is checked
/// again here, whatever was stored: only a push service is ever requested.
pub async fn send(
    client: &reqwest::Client,
    vapid: &Vapid,
    endpoint: &str,
    ttl_secs: i64,
) -> PushOutcome {
    let url = match validate_endpoint(endpoint) {
        Ok(url) => url,
        // Not a push service's: nothing will ever be sent there.
        Err(_) => return PushOutcome::Gone,
    };
    let authorization = vapid.authorization(&audience(&url), Utc::now());
    let response = client
        .post(url)
        .header("TTL", ttl_secs.to_string())
        .header("Urgency", "normal")
        .header("Authorization", authorization)
        .header("Content-Length", "0")
        .send()
        .await;
    match response {
        Ok(response) => classify(response.status().as_u16()),
        // The error's text can carry the endpoint; keep the kind only.
        Err(e) if e.is_timeout() => PushOutcome::Unanswered("push service timed out".into()),
        Err(_) => PushOutcome::Unanswered("push service unreachable".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::VerifyingKey;

    // -- endpoints ----------------------------------------------------------

    #[test]
    fn the_browsers_push_services_are_accepted() {
        for endpoint in [
            "https://fcm.googleapis.com/fcm/send/dVq3mJ8:APA91bH",
            "https://updates.push.services.mozilla.com/wpush/v2/gAAAAABk",
            "https://web.push.apple.com/QGuQyavXutnMA",
            "https://wns2-par02p.notify.windows.com/w/?token=BQYAAAB",
        ] {
            let url = validate_endpoint(endpoint).unwrap_or_else(|e| panic!("{endpoint}: {e:?}"));
            assert_eq!(url.as_str(), endpoint);
        }
    }

    #[test]
    fn any_other_host_is_refused() {
        for endpoint in [
            "https://example.org/push",
            "https://internal/push",
            // Look-alikes: a suffix only counts on a label boundary.
            "https://evilpush.apple.com/x",
            "https://fcm.googleapis.com.evil.test/x",
            "https://notfcm.googleapis.com/x",
            "https://push.apple.com.evil.test/x",
        ] {
            assert_eq!(
                validate_endpoint(endpoint),
                Err(EndpointError::UnknownPushService),
                "{endpoint}"
            );
        }
    }

    #[test]
    fn a_subdomain_of_a_suffix_is_accepted_and_the_suffix_itself_too() {
        assert!(validate_endpoint("https://push.apple.com/x").is_ok());
        assert!(validate_endpoint("https://a.b.notify.windows.com/x").is_ok());
    }

    #[test]
    fn only_https_is_accepted() {
        for endpoint in [
            "http://fcm.googleapis.com/fcm/send/x",
            "ftp://fcm.googleapis.com/x",
            "file:///etc/passwd",
        ] {
            assert_eq!(
                validate_endpoint(endpoint),
                Err(EndpointError::NotHttps),
                "{endpoint}"
            );
        }
    }

    #[test]
    fn a_port_user_info_or_ip_address_is_refused() {
        for endpoint in [
            "https://fcm.googleapis.com:8443/fcm/send/x",
            "https://user:pw@fcm.googleapis.com/fcm/send/x",
            "https://user@fcm.googleapis.com/fcm/send/x",
            "https://127.0.0.1/x",
            "https://[::1]/x",
            "https://169.254.169.254/latest/meta-data",
        ] {
            assert_eq!(
                validate_endpoint(endpoint),
                Err(EndpointError::UnexpectedPart),
                "{endpoint}"
            );
        }
        // The default port written out is the default port.
        assert!(validate_endpoint("https://fcm.googleapis.com:443/fcm/send/x").is_ok());
    }

    #[test]
    fn the_host_is_compared_case_insensitively() {
        // The URL parser lowercases hosts: one written in capitals is the
        // same service, and nothing else.
        assert!(validate_endpoint("https://FCM.GoogleAPIs.com/fcm/send/x").is_ok());
    }

    #[test]
    fn garbage_and_overlong_endpoints_are_refused() {
        assert_eq!(validate_endpoint(""), Err(EndpointError::NotAUrl));
        assert_eq!(validate_endpoint("not a url"), Err(EndpointError::NotAUrl));
        let long = format!(
            "https://fcm.googleapis.com/{}",
            "a".repeat(MAX_ENDPOINT_LEN)
        );
        assert_eq!(validate_endpoint(&long), Err(EndpointError::TooLong));
    }

    #[test]
    fn the_audience_is_the_endpoint_origin() {
        let url = validate_endpoint("https://web.push.apple.com/QGuQyavXutnMA?x=1").unwrap();
        assert_eq!(audience(&url), "https://web.push.apple.com");
    }

    // -- push service answers -----------------------------------------------

    #[test]
    fn a_2xx_is_delivered() {
        for status in [200, 201, 202] {
            assert_eq!(classify(status), PushOutcome::Delivered, "{status}");
        }
    }

    #[test]
    fn a_404_or_410_means_the_subscription_is_gone() {
        assert_eq!(classify(404), PushOutcome::Gone);
        assert_eq!(classify(410), PushOutcome::Gone);
    }

    #[test]
    fn anything_else_is_a_failure_to_retry() {
        for status in [301, 400, 401, 403, 413, 429, 500, 503] {
            assert!(
                matches!(classify(status), PushOutcome::Failed(ref m) if m.contains(&status.to_string())),
                "{status}"
            );
        }
    }

    // -- consecutive failures -------------------------------------------------

    fn failed() -> PushOutcome {
        PushOutcome::Failed("push service answered 503".into())
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 20, 9, 0, 0).unwrap()
    }

    fn keep(n: i32, since: Option<DateTime<Utc>>) -> DeviceAfter {
        DeviceAfter::Keep {
            consecutive_failures: n,
            failing_since: since,
        }
    }

    #[test]
    fn a_delivery_resets_the_failures() {
        let long_ago = Some(now() - Duration::days(30));
        assert_eq!(
            device_after(&PushOutcome::Delivered, 0, None, now()),
            keep(0, None)
        );
        assert_eq!(
            device_after(&PushOutcome::Delivered, 99, long_ago, now()),
            keep(0, None)
        );
    }

    #[test]
    fn a_first_failure_starts_the_clock() {
        assert_eq!(
            device_after(&failed(), 0, None, now()),
            keep(1, Some(now()))
        );
    }

    /// No answer from the push service is a failure as well (#536).
    #[test]
    fn a_push_unanswered_counts_as_a_failure() {
        let since = Some(now() - Duration::hours(3));
        let timed_out = PushOutcome::Unanswered("push service timed out".into());
        assert_eq!(
            device_after(&timed_out, 0, None, now()),
            keep(1, Some(now()))
        );
        assert_eq!(device_after(&timed_out, 7, since, now()), keep(8, since));
    }

    #[test]
    fn a_later_failure_counts_one_more_and_keeps_the_start() {
        let since = Some(now() - Duration::hours(3));
        assert_eq!(device_after(&failed(), 7, since, now()), keep(8, since));
    }

    #[test]
    fn many_failures_within_the_week_keep_the_device() {
        // Several reminders due in one pass, 5 attempts each: dozens of
        // failures inside an outage of minutes.
        let since = Some(now() - Duration::minutes(5));
        assert_eq!(device_after(&failed(), 99, since, now()), keep(100, since));
        let since = Some(now() - Duration::days(MIN_FAILING_DAYS) + Duration::seconds(1));
        assert_eq!(
            device_after(&failed(), MAX_CONSECUTIVE_FAILURES, since, now()),
            keep(MAX_CONSECUTIVE_FAILURES + 1, since)
        );
    }

    #[test]
    fn a_week_of_failures_with_too_few_of_them_keeps_the_device() {
        let since = Some(now() - Duration::days(30));
        assert_eq!(
            device_after(&failed(), MAX_CONSECUTIVE_FAILURES - 2, since, now()),
            keep(MAX_CONSECUTIVE_FAILURES - 1, since)
        );
    }

    #[test]
    fn enough_failures_over_a_week_forget_the_device() {
        let since = Some(now() - Duration::days(MIN_FAILING_DAYS));
        assert_eq!(
            device_after(&failed(), MAX_CONSECUTIVE_FAILURES - 1, since, now()),
            DeviceAfter::Forget
        );
        let since = Some(now() - Duration::days(40));
        assert_eq!(
            device_after(&failed(), MAX_CONSECUTIVE_FAILURES + 5, since, now()),
            DeviceAfter::Forget
        );
    }

    #[test]
    fn a_gone_device_is_forgotten_whatever_its_record() {
        assert_eq!(
            device_after(&PushOutcome::Gone, 0, None, now()),
            DeviceAfter::Forget
        );
        assert_eq!(
            device_after(&PushOutcome::Gone, 3, Some(now()), now()),
            DeviceAfter::Forget
        );
    }

    // -- TTL ----------------------------------------------------------------

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 3, h, m, 0).unwrap()
    }

    #[test]
    fn the_message_lives_until_the_occurrence_starts() {
        assert_eq!(ttl_secs(at(14, 0), at(14, 30)), 30 * 60);
    }

    #[test]
    fn a_late_or_immediate_reminder_still_gets_a_minute() {
        assert_eq!(ttl_secs(at(14, 30), at(14, 30)), MIN_TTL_SECS);
        assert_eq!(ttl_secs(at(15, 0), at(14, 30)), MIN_TTL_SECS);
    }

    #[test]
    fn a_far_occurrence_is_capped_at_four_weeks() {
        assert_eq!(
            ttl_secs(at(14, 0), at(14, 0) + Duration::days(60)),
            MAX_TTL_SECS
        );
    }

    // -- VAPID --------------------------------------------------------------

    /// A fixed P-256 scalar, for the tests only.
    const KEY: &str = "Yh8ZQ5n5k3t3c0mKXJ6tJd0w9p1n8uQ2xB4vV7rL0aE";

    fn vapid() -> Vapid {
        Vapid::new(KEY, "mailto:admin@example.test").unwrap()
    }

    #[test]
    fn the_public_key_is_the_uncompressed_point_in_base64url() {
        let public = URL_SAFE_NO_PAD.decode(vapid().public_key()).unwrap();
        assert_eq!(public.len(), 65);
        assert_eq!(public[0], 0x04);
        let expected = SigningKey::from_slice(&URL_SAFE_NO_PAD.decode(KEY).unwrap())
            .unwrap()
            .verifying_key()
            .to_sec1_point(false);
        assert_eq!(public, expected.as_bytes());
    }

    #[test]
    fn a_malformed_private_key_is_refused() {
        for key in [
            "",
            "not base64url!",
            // 31 bytes.
            "Yh8ZQ5n5k3t3c0mKXJ6tJd0w9p1n8uQ2xB4vV7rL0Q",
            // Zero is not a valid scalar.
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            assert_eq!(
                Vapid::new(key, "mailto:a@example.test").unwrap_err(),
                VapidError::BadPrivateKey,
                "{key:?}"
            );
        }
    }

    #[test]
    fn the_subject_must_be_a_mailto_or_https_uri() {
        assert!(Vapid::new(KEY, "https://maison.example.org").is_ok());
        for subject in [
            "",
            "admin@example.test",
            "http://maison.example.org",
            "mailto:",
        ] {
            assert_eq!(
                Vapid::new(KEY, subject).unwrap_err(),
                VapidError::BadSubject,
                "{subject:?}"
            );
        }
    }

    #[test]
    fn the_debug_output_never_shows_the_private_key() {
        let shown = format!("{:?}", vapid());
        assert!(!shown.contains(KEY), "{shown}");
    }

    /// Splits `vapid t=<jwt>, k=<key>` and checks the JWT's signature with
    /// the advertised key; returns the decoded header and claims.
    fn read_authorization(header: &str) -> (serde_json::Value, serde_json::Value, String) {
        let rest = header.strip_prefix("vapid t=").expect(header);
        let (token, key) = rest.split_once(", k=").expect(header);
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "{token}");
        let decode = |p: &str| -> serde_json::Value {
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p).unwrap()).unwrap()
        };
        let verifying =
            VerifyingKey::from_sec1_bytes(&URL_SAFE_NO_PAD.decode(key).unwrap()).unwrap();
        let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        verifying
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .expect("the signature checks against the advertised key");
        (decode(parts[0]), decode(parts[1]), key.to_string())
    }

    #[test]
    fn the_authorization_is_an_es256_jwt_signed_with_the_advertised_key() {
        let v = vapid();
        let now = at(14, 0);
        let (header, claims, key) =
            read_authorization(&v.authorization("https://fcm.googleapis.com", now));
        assert_eq!(header, serde_json::json!({"typ": "JWT", "alg": "ES256"}));
        assert_eq!(key, v.public_key());
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["sub"], "mailto:admin@example.test");
        assert_eq!(claims["exp"], now.timestamp() + 12 * 3600);
    }

    #[test]
    fn the_token_names_its_own_audience() {
        let v = vapid();
        let (_, a, _) =
            read_authorization(&v.authorization("https://web.push.apple.com", at(9, 0)));
        let (_, b, _) = read_authorization(
            &v.authorization("https://updates.push.services.mozilla.com", at(9, 0)),
        );
        assert_eq!(a["aud"], "https://web.push.apple.com");
        assert_eq!(b["aud"], "https://updates.push.services.mozilla.com");
    }

    // -- send ---------------------------------------------------------------

    /// What is stored is not trusted: a row that never went through
    /// `subscribe` (written by hand, or before the allowlist changed) must
    /// not make the worker request it. Here the endpoint is a live local
    /// listener that would accept the push: `send` must neither connect to
    /// it nor report a delivery, and must report the subscription gone.
    #[tokio::test]
    async fn send_never_requests_an_endpoint_that_is_not_a_push_service() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let connections = Arc::new(AtomicUsize::new(0));
        let seen = connections.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                seen.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let _ = socket
                    .write_all(b"HTTP/1.1 201 Created\r\ncontent-length: 0\r\n\r\n")
                    .await;
            }
        });

        let outcome = send(
            &client(),
            &vapid(),
            &format!("http://127.0.0.1:{port}/push"),
            60,
        )
        .await;

        assert_eq!(outcome, PushOutcome::Gone);
        assert_eq!(
            connections.load(Ordering::SeqCst),
            0,
            "the endpoint was requested"
        );
    }

    // -- the kind a device shows ----------------------------------------------

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 30, 12, 0, 0).unwrap()
    }

    const R: PushKind = PushKind::AgendaReminder;
    const A: PushKind = PushKind::BudgetAlert;

    fn pushed(kinds: &[PushKind]) -> Pending {
        kinds.iter().fold(Pending::default(), |p, &k| {
            pending_after_push(p, k, t0(), 3600)
        })
    }

    /// Each receipt at `now`, in turn: the kind each device shows.
    fn shown(mut pending: Pending, receipts: usize, now: DateTime<Utc>) -> Vec<Option<PushKind>> {
        (0..receipts)
            .map(|_| {
                let (kind, after) = pending_on_receipt(pending, now);
                pending = after;
                kind
            })
            .collect()
    }

    #[test]
    fn the_kinds_have_the_names_of_the_notifications() {
        assert_eq!(R.as_str(), "agenda_reminder");
        assert_eq!(A.as_str(), "budget_alert");
        for kind in [R, A] {
            assert_eq!(PushKind::parse(kind.as_str()), Some(kind));
            let only = PendingKind::Only(kind);
            assert_eq!(PendingKind::parse(only.as_str()), Some(only));
        }
        assert_eq!(PendingKind::Mixed.as_str(), "mixed");
        assert_eq!(PendingKind::parse("mixed"), Some(PendingKind::Mixed));
        assert_eq!(PushKind::parse("mixed"), None);
        assert_eq!(PushKind::parse("message"), None);
        assert_eq!(PendingKind::parse("message"), None);
    }

    /// A push shows its own kind, once; nothing is left after.
    #[test]
    fn a_push_shows_its_kind() {
        let (kind, after) = pending_on_receipt(pushed(&[A]), t0());
        assert_eq!(kind, Some(A));
        assert_eq!(after, Pending::default());
        assert_eq!(shown(pushed(&[R, R]), 3, t0()), [Some(R), Some(R), None]);
    }

    /// A reminder pushed again (a retry) while an alert is pending: no
    /// device can tell which one it got, each shows the neutral text —
    /// never the other one's. Once all shown, an alert shows its own again.
    #[test]
    fn pushes_of_two_kinds_pending_show_neither() {
        let mixed = pushed(&[R, A, R]);
        assert_eq!(shown(mixed, 3, t0()), [None, None, None]);
        let drained = (0..3).fold(mixed, |p, _| pending_on_receipt(p, t0()).1);
        assert_eq!(drained, Pending::default());
        let again = pending_after_push(drained, A, t0(), 3600);
        assert_eq!(shown(again, 1, t0()), [Some(A)]);
    }

    /// Read back from its columns. A kind the code does not know (the
    /// column's CHECK admits none) counts as mixed: the neutral text.
    #[test]
    fn pending_is_read_back_from_its_columns() {
        let alert = pushed(&[A, A]);
        let kind = alert.kind.map(PendingKind::as_str);
        assert_eq!(Pending::from_columns(kind, alert.count, alert.until), alert);
        let mixed = pushed(&[A, R]);
        assert_eq!(
            Pending::from_columns(Some("mixed"), mixed.count, mixed.until),
            mixed
        );
        let unknown = Pending::from_columns(Some("message"), 1, alert.until);
        assert_eq!(unknown.kind, Some(PendingKind::Mixed));
        assert_eq!(shown(unknown, 1, t0()), [None]);
        assert_eq!(Pending::from_columns(None, 0, None), Pending::default());
    }

    /// A receipt with nothing pending: the neutral text.
    #[test]
    fn nothing_pending_shows_nothing() {
        assert_eq!(shown(Pending::default(), 1, t0()), [None]);
    }

    /// Past the longest TTL of the pushes pending, none of them can still
    /// arrive: the record starts over, and a push of the other kind shows
    /// its own.
    #[test]
    fn pushes_past_their_ttl_are_forgotten() {
        let alert = pending_after_push(Pending::default(), A, t0(), 60);
        let later = pending_after_push(alert, R, t0(), 3600);
        assert_eq!(later.until, Some(t0() + Duration::seconds(3600)));
        assert_eq!(later.kind, Some(PendingKind::Mixed));
        assert_eq!(later.count, 2);

        let past = t0() + Duration::seconds(61);
        assert_eq!(shown(alert, 1, past), [None]);
        let reminder = pending_after_push(alert, R, past, 60);
        assert_eq!(reminder.count, 1);
        assert_eq!(shown(reminder, 1, past), [Some(R)]);
    }

    /// A push the push service refused never arrives: it is taken off the
    /// count. Its kind stays mixed in though: the neutral text, never a
    /// wrong one.
    #[test]
    fn a_push_refused_is_not_counted() {
        assert_eq!(pending_after_failure(pushed(&[A])), Pending::default());
        let one_refused = pending_after_failure(pushed(&[A, R]));
        assert_eq!(one_refused.count, 1);
        assert_eq!(shown(one_refused, 1, t0()), [None]);
        assert_eq!(
            pending_after_failure(Pending::default()),
            Pending::default()
        );
    }

    /// A push the push service did not answer may still arrive: it stays
    /// counted, but which kind arrives is no longer known — the neutral
    /// text until the pushes pending are all received, or past their TTL.
    #[test]
    fn a_push_unanswered_may_still_arrive_and_shows_neither_kind() {
        let unanswered = pending_after_unanswered(pushed(&[R]));
        assert_eq!(unanswered.count, 1);
        assert_eq!(unanswered.kind, Some(PendingKind::Mixed));
        assert_eq!(unanswered.until, pushed(&[R]).until);

        let then_an_alert = pending_after_push(unanswered, A, t0(), 3600);
        assert_eq!(shown(then_an_alert, 3, t0()), [None, None, None]);
        let drained = (0..2).fold(then_an_alert, |p, _| pending_on_receipt(p, t0()).1);
        assert_eq!(drained, Pending::default());
        let past = t0() + Duration::seconds(3601);
        let reminder = pending_after_push(unanswered, R, past, 60);
        assert_eq!(shown(reminder, 1, past), [Some(R)]);

        assert_eq!(
            pending_after_unanswered(Pending::default()),
            Pending::default()
        );
    }
}
