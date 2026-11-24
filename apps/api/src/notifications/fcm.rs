//! Firebase Cloud Messaging, for the Android app (#1002, #1003): the
//! device's registration token, the server's FCM configuration, and the
//! push itself.
//!
//! The app is a Capacitor WebView with no Web Push; Google's FCM pushes it
//! instead, and knows the device by a **registration token** the app gets
//! from the Firebase SDK. The token is stored in `push_subscriptions`
//! (`platform = 'fcm'`, migration 0071) in place of a browser's endpoint.
//!
//! **The push** ([`FcmSender`], #1003) goes to FCM's HTTP v1 API, as the
//! service account of the server's Firebase project: an assertion signed
//! with its key ([`ServiceAccount::assertion`], RS256) is exchanged at
//! Google's token endpoint for an access token, kept until shortly before
//! it expires. The message is data only ([`message_body`]): the kind and
//! the opaque ids of the entries of the notification centre it stands for,
//! as a web push names them (#862) — nothing about the event, the expense
//! or the item. Unlike a web push it is not encrypted for the device:
//! Google reads those values. Only the kinds [`sends`] admits go by FCM:
//! not the reminders, which the app schedules on the phone (#668).
//!
//! **A token is an opaque string the server never requests.** Unlike a
//! browser's endpoint (`push::validate_endpoint`), it is not a URL: the
//! server hands it to FCM's API in a JSON body (#1003). It is still
//! user input stored per account and shown in the art. 15 export, so
//! [`validate_token`] bounds its length and its characters, and checks
//! nothing else — FCM documents no format.
//!
//! **The two platforms cannot share a row.** `endpoint` is unique across
//! both; a token admits neither `/` nor `.`, which every endpoint
//! `push::validate_endpoint` admits holds (`https://<host>/…`), so no
//! token is ever a browser's endpoint, and no device moves from one
//! platform to the other on registering again.

use std::path::{Path, PathBuf};

use aws_lc_rs::signature::RsaKeyPair;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::push::{PushKind, PushOutcome};

/// Longest token taken. Real ones run to about 160 characters (an
/// instance id, `:`, then `APA91b…`); the bound is that of a browser's
/// endpoint (`push::MAX_ENDPOINT_LEN`), the same column.
pub const MAX_TOKEN_LEN: usize = 1024;

/// Why a token is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    Empty,
    TooLong,
    /// A character outside the tokens' alphabet: ASCII letters and
    /// digits, `-`, `_` and `:`.
    UnexpectedCharacter,
}

/// The token, if it may be one: 1 to [`MAX_TOKEN_LEN`] characters, each an
/// ASCII letter or digit, `-`, `_` (base64url) or `:` (between the
/// instance id and the rest).
pub fn validate_token(raw: &str) -> Result<&str, TokenError> {
    if raw.is_empty() {
        return Err(TokenError::Empty);
    }
    if raw.len() > MAX_TOKEN_LEN {
        return Err(TokenError::TooLong);
    }
    if !raw
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':'))
    {
        return Err(TokenError::UnexpectedCharacter);
    }
    Ok(raw)
}

/// Environment variable holding the Firebase project's id.
pub const PROJECT_ID_VAR: &str = "FCM_PROJECT_ID";
/// Environment variable holding the path of the service account's JSON
/// key, the credential FCM's API is called with.
pub const SERVICE_ACCOUNT_FILE_VAR: &str = "FCM_SERVICE_ACCOUNT_FILE";

/// The server's FCM configuration, as the environment gives it: checked
/// for its form, the file not read yet ([`FcmSender::load`] reads it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FcmConfig {
    project_id: String,
    service_account_file: PathBuf,
}

/// Why a configuration is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// One of the two variables set without the other.
    Incomplete,
    /// Not a Google Cloud project id: 6 to 30 lowercase ASCII letters,
    /// digits or hyphens, starting with a letter, not ending with a
    /// hyphen. It goes into the path of FCM's API (#1003).
    BadProjectId,
    /// The service account's file is not there.
    MissingServiceAccountFile,
}

impl FcmConfig {
    /// From the project id and the service account's file path, both
    /// already read: checks the id's form, not the file.
    pub fn new(project_id: &str, service_account_file: &str) -> Result<Self, ConfigError> {
        let (project_id, file) = (project_id.trim(), service_account_file.trim());
        if project_id.is_empty() || file.is_empty() {
            return Err(ConfigError::Incomplete);
        }
        if !is_project_id(project_id) {
            return Err(ConfigError::BadProjectId);
        }
        Ok(FcmConfig {
            project_id: project_id.to_string(),
            service_account_file: PathBuf::from(file),
        })
    }

    /// From the values of [`PROJECT_ID_VAR`] and
    /// [`SERVICE_ACCOUNT_FILE_VAR`]: `Ok(None)` when both are empty — the
    /// Android app's notifications are then off on this server, and a
    /// token is refused with `fcm_not_configured` —, an error when one is
    /// set without the other, when the id is malformed or when the file is
    /// not there.
    pub fn from_vars(
        project_id: &str,
        service_account_file: &str,
    ) -> Result<Option<Self>, ConfigError> {
        if project_id.trim().is_empty() && service_account_file.trim().is_empty() {
            return Ok(None);
        }
        let config = FcmConfig::new(project_id, service_account_file)?;
        if !config.service_account_file.is_file() {
            return Err(ConfigError::MissingServiceAccountFile);
        }
        Ok(Some(config))
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn service_account_file(&self) -> &Path {
        &self.service_account_file
    }
}

fn is_project_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (6..=30).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[bytes.len() - 1] != b'-'
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

// -- sending (#1003) ---------------------------------------------------------

/// Whether a kind of push goes to the Android app by FCM: the agenda
/// changes, the budget alerts and the expiry recap. Not the reminders: the
/// app schedules them on the phone itself (#668), a push would show each
/// one twice.
pub fn sends(kind: PushKind) -> bool {
    match kind {
        PushKind::AgendaChange | PushKind::BudgetAlert | PushKind::StockExpiry => true,
        PushKind::AgendaReminder => false,
    }
}

/// What one FCM push carries: the kind, and the entries of the
/// notification centre it stands for (#862), with the TTL of the kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub kind: PushKind,
    pub entries: Vec<Uuid>,
    pub ttl_secs: i64,
}

/// The body of FCM's `messages:send` for `token` (HTTP v1): a data
/// message, which the app shows itself — no `notification` block. Its
/// data are what a web push names (`push::payload`, #862), plus the kind,
/// which a web device asks the site for and the app is told: `kind`
/// (`PushKind::as_str`) and `entries`, the entries' ids separated by
/// commas — left out with no entry, or with more than
/// `push::MAX_PAYLOAD_ENTRIES`. Nothing else: no title, no author, no
/// amount. FCM does not encrypt them for the device, unlike a web push
/// (RFC 8291): Google reads them. High priority, so that a phone in Doze
/// shows it now; FCM keeps it `ttl_secs` for a phone offline.
pub fn message_body(token: &str, message: &Message) -> serde_json::Value {
    let mut data = serde_json::Map::new();
    data.insert("kind".into(), message.kind.as_str().into());
    if (1..=super::push::MAX_PAYLOAD_ENTRIES).contains(&message.entries.len()) {
        let entries: Vec<String> = message.entries.iter().map(Uuid::to_string).collect();
        data.insert("entries".into(), entries.join(",").into());
    }
    serde_json::json!({
        "message": {
            "token": token,
            "data": data,
            "android": {
                "priority": "HIGH",
                "ttl": format!("{}s", message.ttl_secs),
            },
        }
    })
}

/// Classifies FCM's answer to a `messages:send`: 2xx delivered; the
/// error code `UNREGISTERED` (the app uninstalled, the token expired or
/// withdrawn), or a 404, gone — the device is forgotten, as a browser's on
/// a 404 or 410; anything else a failure worth another attempt, which
/// keeps the status and the error code only, nothing of the body's text.
pub fn classify(status: u16, body: &str) -> PushOutcome {
    let code = error_code(body);
    match status {
        200..=299 => PushOutcome::Delivered,
        _ if code.as_deref() == Some("UNREGISTERED") => PushOutcome::Gone,
        404 => PushOutcome::Gone,
        other => PushOutcome::Failed(match code {
            Some(code) => format!("FCM answered {other} {code}"),
            None => format!("FCM answered {other}"),
        }),
    }
}

/// The first `errorCode` of an FCM error's `details`, if it looks like
/// one (`UNREGISTERED`, `QUOTA_EXCEEDED`…): upper case and `_` only.
fn error_code(body: &str) -> Option<String> {
    let body: serde_json::Value = serde_json::from_str(body).ok()?;
    body["error"]["details"]
        .as_array()?
        .iter()
        .filter_map(|detail| detail["errorCode"].as_str())
        .find(|code| !code.is_empty() && code.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'))
        .map(str::to_string)
}

/// The OAuth 2.0 scope FCM's API is called with.
pub const SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
/// Google's token endpoint: where the assertion is exchanged, and its
/// audience.
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// FCM's API.
pub const SEND_BASE_URL: &str = "https://fcm.googleapis.com";
/// How long an assertion is valid: Google's maximum, an hour.
pub const ASSERTION_LIFETIME_SECS: i64 = 3600;
/// An access token is renewed this long before it expires.
pub const TOKEN_MARGIN_SECS: i64 = 60;

/// The service account FCM's API is called as: its address, and its
/// private key, which signs the assertion exchanged for an access token
/// (RFC 7523).
pub struct ServiceAccount {
    client_email: String,
    key: RsaKeyPair,
}

impl std::fmt::Debug for ServiceAccount {
    // Never the key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceAccount")
            .field("client_email", &self.client_email)
            .finish_non_exhaustive()
    }
}

/// Why a service account's file is refused. None names its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceAccountError {
    NotJson,
    /// `type` is not `service_account`.
    NotAServiceAccount,
    MissingClientEmail,
    /// No `private_key`, or not an RSA key in PKCS#8 PEM.
    BadPrivateKey,
}

impl ServiceAccount {
    /// From the JSON key file Google's console gives: `type`
    /// `service_account`, `client_email`, and `private_key`, an RSA key in
    /// PKCS#8 PEM. Its other fields are not read: the token endpoint is
    /// Google's ([`TOKEN_URL`]), whatever `token_uri` says.
    pub fn parse(json: &str) -> Result<Self, ServiceAccountError> {
        let json: serde_json::Value =
            serde_json::from_str(json).map_err(|_| ServiceAccountError::NotJson)?;
        if json["type"] != "service_account" {
            return Err(ServiceAccountError::NotAServiceAccount);
        }
        let client_email = json["client_email"]
            .as_str()
            .map(str::trim)
            .filter(|email| !email.is_empty())
            .ok_or(ServiceAccountError::MissingClientEmail)?;
        let der = json["private_key"]
            .as_str()
            .and_then(pkcs8_der)
            .ok_or(ServiceAccountError::BadPrivateKey)?;
        let key = RsaKeyPair::from_pkcs8(&der).map_err(|_| ServiceAccountError::BadPrivateKey)?;
        Ok(ServiceAccount {
            client_email: client_email.to_string(),
            key,
        })
    }

    pub fn client_email(&self) -> &str {
        &self.client_email
    }

    /// The assertion exchanged for an access token: a JWT of
    /// [`assertion_claims`], signed RS256 with the account's key.
    pub fn assertion(&self, now: DateTime<Utc>) -> Result<String, aws_lc_rs::error::Unspecified> {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let claims = URL_SAFE_NO_PAD.encode(assertion_claims(&self.client_email, now).to_string());
        let signing_input = format!("{header}.{claims}");
        let mut signature = vec![0; self.key.public_modulus_len()];
        self.key.sign(
            &aws_lc_rs::signature::RSA_PKCS1_SHA256,
            &aws_lc_rs::rand::SystemRandom::new(),
            signing_input.as_bytes(),
            &mut signature,
        )?;
        Ok(format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature)
        ))
    }
}

/// The DER inside a `PRIVATE KEY` PEM block.
fn pkcs8_der(pem: &str) -> Option<Vec<u8>> {
    let body = pem
        .split_once("-----BEGIN PRIVATE KEY-----")?
        .1
        .split_once("-----END PRIVATE KEY-----")?
        .0;
    let body: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    STANDARD.decode(body).ok()
}

/// The claims of the assertion of `client_email` at `now`: FCM's scope,
/// Google's token endpoint as audience, valid
/// [`ASSERTION_LIFETIME_SECS`].
pub fn assertion_claims(client_email: &str, now: DateTime<Utc>) -> serde_json::Value {
    serde_json::json!({
        "iss": client_email,
        "scope": SCOPE,
        "aud": TOKEN_URL,
        "iat": now.timestamp(),
        "exp": now.timestamp() + ASSERTION_LIFETIME_SECS,
    })
}

/// An access token, and when it expires.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken {
    value: String,
    expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for AccessToken {
    // Never the token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessToken")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl AccessToken {
    /// From the token endpoint's answer, received at `now`: its
    /// `access_token`, valid `expires_in` seconds.
    pub fn from_response(body: &str, now: DateTime<Utc>) -> Option<Self> {
        let body: serde_json::Value = serde_json::from_str(body).ok()?;
        let value = body["access_token"].as_str().filter(|v| !v.is_empty())?;
        let expires_in = body["expires_in"].as_i64().filter(|secs| *secs > 0)?;
        Some(AccessToken {
            value: value.to_string(),
            expires_at: now + chrono::Duration::seconds(expires_in),
        })
    }

    /// Still usable at `now`: more than [`TOKEN_MARGIN_SECS`] to go.
    pub fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        now + chrono::Duration::seconds(TOKEN_MARGIN_SECS) < self.expires_at
    }
}

/// The push of `fcm` ([`FcmSender::send`]), as `push_entries` takes it.
pub fn sender(
    fcm: std::sync::Arc<FcmSender>,
) -> impl Fn(String, Message) -> futures::future::BoxFuture<'static, PushOutcome> + Send + Sync {
    move |token, message| {
        let fcm = fcm.clone();
        Box::pin(async move { fcm.send(&token, &message).await })
    }
}

/// The server's FCM sender: its project, its service account read at
/// start, and the access token in use, kept until shortly before it
/// expires ([`AccessToken::is_fresh`]) and shared by every push.
///
/// Every request goes through `outbound_http::oauth_client()`: its
/// timeouts, and no redirect followed — the bearer token is never sent
/// anywhere else.
pub struct FcmSender {
    project_id: String,
    account: ServiceAccount,
    token_url: String,
    send_url: String,
    token: tokio::sync::Mutex<Option<AccessToken>>,
}

impl std::fmt::Debug for FcmSender {
    // Neither the key nor the access token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FcmSender")
            .field("project_id", &self.project_id)
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl FcmSender {
    /// From [`PROJECT_ID_VAR`] and [`SERVICE_ACCOUNT_FILE_VAR`]
    /// ([`FcmConfig::from_vars`]), the file read and its key tried
    /// ([`FcmSender::load`]): `Ok(None)` when both are unset, an error —
    /// the server refuses to start — when the configuration or the file is
    /// unusable, so that a typo does not silently turn the Android app's
    /// notifications off.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let project_id = std::env::var(PROJECT_ID_VAR).unwrap_or_default();
        let file = std::env::var(SERVICE_ACCOUNT_FILE_VAR).unwrap_or_default();
        let config = FcmConfig::from_vars(&project_id, &file)
            .map_err(|e| anyhow::anyhow!("{PROJECT_ID_VAR} / {SERVICE_ACCOUNT_FILE_VAR}: {e:?}"))?;
        config.as_ref().map(Self::load).transpose()
    }

    /// Reads the service account's file of `config`, and signs an
    /// assertion with its key once, so that a key that cannot sign stops
    /// the start rather than every push. The errors name the file, never
    /// its content.
    pub fn load(config: &FcmConfig) -> anyhow::Result<Self> {
        let path = config.service_account_file();
        let json = std::fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!(
                "{SERVICE_ACCOUNT_FILE_VAR}: cannot read {}: {}",
                path.display(),
                e.kind()
            )
        })?;
        let account = ServiceAccount::parse(&json).map_err(|e| {
            anyhow::anyhow!("{SERVICE_ACCOUNT_FILE_VAR}: {}: {e:?}", path.display())
        })?;
        account.assertion(Utc::now()).map_err(|_| {
            anyhow::anyhow!(
                "{SERVICE_ACCOUNT_FILE_VAR}: {}: the private key cannot sign",
                path.display()
            )
        })?;
        Ok(FcmSender {
            project_id: config.project_id().to_string(),
            account,
            token_url: TOKEN_URL.into(),
            send_url: String::new(),
            token: tokio::sync::Mutex::new(None),
        }
        .with_send_base_url(SEND_BASE_URL))
    }

    /// Google's token endpoint and FCM's API replaced by `token_url` and
    /// `send_base_url`: for the tests, which stand a local server in for
    /// both. Never called by the server itself.
    pub fn with_endpoints(mut self, token_url: &str, send_base_url: &str) -> Self {
        self.token_url = token_url.to_string();
        *self.token.get_mut() = None;
        self.with_send_base_url(send_base_url)
    }

    fn with_send_base_url(mut self, base: &str) -> Self {
        self.send_url = format!("{base}/v1/projects/{}/messages:send", self.project_id);
        self
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// Sends `message` to the device of `token`. An access token is
    /// fetched first when none is fresh; a 401 forgets it, the next push
    /// fetches another. No access token — the endpoint refused the
    /// assertion, or did not answer — is a failure: nothing was sent.
    pub async fn send(&self, token: &str, message: &Message) -> PushOutcome {
        let access = match self.access_token().await {
            Ok(access) => access,
            Err(reason) => return PushOutcome::Failed(reason),
        };
        let response = crate::outbound_http::oauth_client()
            .post(&self.send_url)
            .bearer_auth(&access)
            .json(&message_body(token, message))
            .send()
            .await;
        match response {
            Ok(response) => {
                let status = response.status().as_u16();
                if status == 401 {
                    self.forget(&access).await;
                }
                let body = response.text().await.unwrap_or_default();
                classify(status, &body)
            }
            // The error's text can carry the URL; keep the kind only.
            Err(e) if e.is_timeout() => PushOutcome::Unanswered("FCM timed out".into()),
            Err(_) => PushOutcome::Unanswered("FCM unreachable".into()),
        }
    }

    /// The access token in use, or a new one. The lock is held while it is
    /// fetched: the pushes sent at once wait for one token, and do not
    /// each fetch their own.
    async fn access_token(&self) -> Result<String, String> {
        let mut cached = self.token.lock().await;
        let now = Utc::now();
        if let Some(token) = cached.as_ref().filter(|token| token.is_fresh(now)) {
            return Ok(token.value.clone());
        }
        let assertion = self
            .account
            .assertion(now)
            .map_err(|_| "FCM assertion could not be signed".to_string())?;
        let response = crate::outbound_http::oauth_client()
            .post(&self.token_url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|_| "FCM token endpoint unreachable".to_string())?;
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        let token = (status == 200)
            .then(|| AccessToken::from_response(&body, now))
            .flatten()
            .ok_or_else(|| format!("FCM token endpoint answered {status}"))?;
        let value = token.value.clone();
        *cached = Some(token);
        Ok(value)
    }

    /// Forgets the access token `refused`, unless another replaced it.
    async fn forget(&self, refused: &str) {
        let mut cached = self.token.lock().await;
        if cached.as_ref().is_some_and(|token| token.value == refused) {
            *cached = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of a real token: an instance id, `:`, then `APA91b` and
    /// base64url.
    const PLAUSIBLE: &str = "dVq3mJ8kTq2xQ9Yw1ZbC0e:APA91bHk3s-Lp_Q7vR2mXy8T0aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ-_0123456789abcdefghijklmnopqrs";

    #[test]
    fn a_plausible_token_is_taken_as_it_is() {
        assert_eq!(validate_token(PLAUSIBLE), Ok(PLAUSIBLE));
        assert_eq!(validate_token("a"), Ok("a"));
        let longest = "A".repeat(MAX_TOKEN_LEN);
        assert_eq!(validate_token(&longest), Ok(longest.as_str()));
    }

    #[test]
    fn an_empty_token_is_refused() {
        assert_eq!(validate_token(""), Err(TokenError::Empty));
    }

    #[test]
    fn a_token_too_long_is_refused() {
        let token = "A".repeat(MAX_TOKEN_LEN + 1);
        assert_eq!(validate_token(&token), Err(TokenError::TooLong));
    }

    #[test]
    fn a_character_outside_the_alphabet_is_refused() {
        for token in [
            " abc",
            "abc ",
            "ab c",
            "abc\n",
            "abc\0",
            "ab/c",
            "ab.c",
            "ab+c",
            "ab=c",
            "ab%2F",
            "ab\"c",
            "<script>",
            "abcé",
            "https://fcm.googleapis.com/fcm/send/abc",
        ] {
            assert_eq!(
                validate_token(token),
                Err(TokenError::UnexpectedCharacter),
                "{token:?}"
            );
        }
    }

    /// `endpoint` is unique across both platforms: no browser's endpoint
    /// is ever a token, so a registration never takes a row of the other
    /// platform over.
    #[test]
    fn no_browser_endpoint_is_a_token() {
        for endpoint in [
            "https://fcm.googleapis.com/fcm/send/dVq3mJ8:APA91bH",
            "https://updates.push.services.mozilla.com/wpush/v2/gAAAAABk",
            "https://web.push.apple.com/QGx",
            "https://wns2-par02p.notify.windows.com/w/?token=BQYAAAB",
        ] {
            assert!(
                super::super::push::validate_endpoint(endpoint).is_ok(),
                "{endpoint}"
            );
            assert!(validate_token(endpoint).is_err(), "{endpoint}");
        }
    }

    #[test]
    fn a_configuration_needs_both_values() {
        assert_eq!(
            FcmConfig::new("", "/run/secrets/fcm.json"),
            Err(ConfigError::Incomplete)
        );
        assert_eq!(
            FcmConfig::new("manage-our-home", "  "),
            Err(ConfigError::Incomplete)
        );
        let config = FcmConfig::new(" manage-our-home ", "/run/secrets/fcm.json").unwrap();
        assert_eq!(config.project_id(), "manage-our-home");
        assert_eq!(
            config.service_account_file(),
            Path::new("/run/secrets/fcm.json")
        );
    }

    #[test]
    fn the_project_id_is_a_google_cloud_one() {
        for id in ["abcdef", "manage-our-home", "home-2026", &"a".repeat(30)] {
            assert!(FcmConfig::new(id, "/k.json").is_ok(), "{id}");
        }
        for id in [
            "abcde",
            &"a".repeat(31),
            "1home-app",
            "-home-app",
            "home-app-",
            "Home-App",
            "home_app",
            "home.app",
            "home/app",
            "home app",
            "../../x",
        ] {
            assert_eq!(
                FcmConfig::new(id, "/k.json"),
                Err(ConfigError::BadProjectId),
                "{id}"
            );
        }
    }

    // -- the configuration's variables ----------------------------------------

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/fcm/service_account.json"
    );
    const PUBLIC_JWK: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/fcm/public_jwk.json"
    ));

    #[test]
    fn no_variable_set_is_no_configuration() {
        assert_eq!(FcmConfig::from_vars("", ""), Ok(None));
        assert_eq!(FcmConfig::from_vars("  ", "\t"), Ok(None));
    }

    /// One variable without the other stops the start: a typo does not
    /// silently turn the Android app's notifications off.
    #[test]
    fn one_variable_without_the_other_is_refused() {
        assert_eq!(
            FcmConfig::from_vars("manage-our-home", ""),
            Err(ConfigError::Incomplete)
        );
        assert_eq!(
            FcmConfig::from_vars("", FIXTURE),
            Err(ConfigError::Incomplete)
        );
    }

    #[test]
    fn a_service_account_file_not_there_is_refused() {
        assert_eq!(
            FcmConfig::from_vars("manage-our-home", "/nonexistent/fcm.json"),
            Err(ConfigError::MissingServiceAccountFile)
        );
        // A directory is not the file either.
        assert_eq!(
            FcmConfig::from_vars("manage-our-home", env!("CARGO_MANIFEST_DIR")),
            Err(ConfigError::MissingServiceAccountFile)
        );
        assert_eq!(
            FcmConfig::from_vars("Bad_Id", FIXTURE),
            Err(ConfigError::BadProjectId)
        );
        let config = FcmConfig::from_vars("manage-our-home", FIXTURE)
            .unwrap()
            .unwrap();
        assert_eq!(config.service_account_file(), Path::new(FIXTURE));
    }

    // -- what goes by FCM -----------------------------------------------------

    #[test]
    fn the_reminders_are_not_sent_by_fcm() {
        assert!(!sends(PushKind::AgendaReminder));
        for kind in [
            PushKind::AgendaChange,
            PushKind::BudgetAlert,
            PushKind::StockExpiry,
        ] {
            assert!(sends(kind), "{kind:?}");
        }
    }

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// Data only: no `notification` block, nothing in the clear but the
    /// kind and the entries' opaque ids; high priority, the kind's TTL.
    #[test]
    fn a_message_carries_the_kind_and_the_entries_only() {
        let message = Message {
            kind: PushKind::BudgetAlert,
            entries: vec![uuid(1), uuid(2)],
            ttl_secs: 86_400,
        };
        assert_eq!(
            message_body("tok:en", &message),
            serde_json::json!({
                "message": {
                    "token": "tok:en",
                    "data": {
                        "kind": "budget_alert",
                        "entries": "00000000-0000-0000-0000-000000000001,00000000-0000-0000-0000-000000000002",
                    },
                    "android": { "priority": "HIGH", "ttl": "86400s" },
                }
            })
        );
    }

    /// Without an entry, or with more than a web push names, the message
    /// names none: its tap marks nothing read.
    #[test]
    fn a_message_names_no_entry_beyond_the_web_bound() {
        let most = super::super::push::MAX_PAYLOAD_ENTRIES as u128;
        for entries in [vec![], (0..=most).map(uuid).collect()] {
            let body = message_body(
                "t",
                &Message {
                    kind: PushKind::StockExpiry,
                    entries,
                    ttl_secs: 60,
                },
            );
            assert_eq!(
                body["message"]["data"],
                serde_json::json!({ "kind": "stock_expiry" })
            );
        }
        let body = message_body(
            "t",
            &Message {
                kind: PushKind::AgendaChange,
                entries: (0..most).map(uuid).collect(),
                ttl_secs: 60,
            },
        );
        let named = body["message"]["data"]["entries"].as_str().unwrap();
        assert_eq!(named.split(',').count() as u128, most);
    }

    // -- FCM's answers --------------------------------------------------------

    const UNREGISTERED: &str = r#"{"error":{"code":404,"message":"Requested entity was not found.","status":"NOT_FOUND","details":[{"@type":"type.googleapis.com/google.firebase.fcm.v1.FcmError","errorCode":"UNREGISTERED"}]}}"#;

    #[test]
    fn fcm_s_answers_are_classified() {
        assert_eq!(
            classify(200, r#"{"name":"projects/p/messages/1"}"#),
            PushOutcome::Delivered
        );
        assert_eq!(classify(404, UNREGISTERED), PushOutcome::Gone);
        assert_eq!(classify(404, ""), PushOutcome::Gone);
        // UNREGISTERED whatever the status it comes with.
        let unregistered_400 = UNREGISTERED.replace("404", "400");
        assert_eq!(classify(400, &unregistered_400), PushOutcome::Gone);
        // Anything else is worth another attempt: the error code is kept,
        // nothing else of the body.
        let invalid = r#"{"error":{"code":400,"message":"The registration token is not a valid FCM registration token","status":"INVALID_ARGUMENT","details":[{"@type":"type.googleapis.com/google.firebase.fcm.v1.FcmError","errorCode":"INVALID_ARGUMENT"}]}}"#;
        assert_eq!(
            classify(400, invalid),
            PushOutcome::Failed("FCM answered 400 INVALID_ARGUMENT".into())
        );
        assert_eq!(
            classify(503, "<html>unavailable</html>"),
            PushOutcome::Failed("FCM answered 503".into())
        );
        assert_eq!(
            classify(401, r#"{"error":{"details":[{"errorCode":"secret <b>"}]}}"#),
            PushOutcome::Failed("FCM answered 401".into())
        );
    }

    // -- the service account --------------------------------------------------

    fn fixture() -> ServiceAccount {
        ServiceAccount::parse(&std::fs::read_to_string(FIXTURE).unwrap()).unwrap()
    }

    #[test]
    fn a_service_account_file_is_read() {
        assert_eq!(
            fixture().client_email(),
            "fcm-sender@manage-our-home-test.iam.gserviceaccount.com"
        );
    }

    #[test]
    fn a_file_that_is_no_service_account_is_refused() {
        let good: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(FIXTURE).unwrap()).unwrap();
        let with = |field: &str, value: serde_json::Value| {
            let mut json = good.clone();
            json[field] = value;
            json.to_string()
        };
        let without = |field: &str| {
            let mut json = good.clone();
            json.as_object_mut().unwrap().remove(field);
            json.to_string()
        };
        let refused = |json: &str| ServiceAccount::parse(json).err();
        assert_eq!(refused("not json"), Some(ServiceAccountError::NotJson));
        assert_eq!(
            refused(&with("type", "authorized_user".into())),
            Some(ServiceAccountError::NotAServiceAccount)
        );
        assert_eq!(
            refused(&without("type")),
            Some(ServiceAccountError::NotAServiceAccount)
        );
        assert_eq!(
            refused(&without("client_email")),
            Some(ServiceAccountError::MissingClientEmail)
        );
        assert_eq!(
            refused(&with("client_email", " ".into())),
            Some(ServiceAccountError::MissingClientEmail)
        );
        assert_eq!(
            refused(&without("private_key")),
            Some(ServiceAccountError::BadPrivateKey)
        );
        assert_eq!(
            refused(&with(
                "private_key",
                "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n".into()
            )),
            Some(ServiceAccountError::BadPrivateKey)
        );
    }

    /// Neither the key nor the access token in `Debug`.
    #[test]
    fn the_secrets_are_never_shown() {
        let shown = format!("{:?}", fixture());
        assert!(shown.contains("fcm-sender@"), "{shown}");
        assert!(!shown.contains("PRIVATE"), "{shown}");
        assert!(!shown.contains("MII"), "{shown}");
        let token = AccessToken::from_response(
            r#"{"access_token":"ya29.secret","expires_in":3599,"token_type":"Bearer"}"#,
            Utc::now(),
        )
        .unwrap();
        assert!(!format!("{token:?}").contains("ya29"));
    }

    #[test]
    fn the_assertion_s_claims_are_google_s() {
        let now = DateTime::from_timestamp(1_790_000_000, 0).unwrap();
        assert_eq!(
            assertion_claims("a@b.iam.gserviceaccount.com", now),
            serde_json::json!({
                "iss": "a@b.iam.gserviceaccount.com",
                "scope": SCOPE,
                "aud": TOKEN_URL,
                "iat": 1_790_000_000i64,
                "exp": 1_790_000_000i64 + ASSERTION_LIFETIME_SECS,
            })
        );
    }

    /// Signed RS256 with the account's key: its public half verifies it.
    #[test]
    fn the_assertion_is_signed_with_the_account_s_key() {
        let now = Utc::now();
        let assertion = fixture().assertion(now).unwrap();
        let header = jsonwebtoken::decode_header(&assertion).unwrap();
        assert_eq!(header.alg, jsonwebtoken::Algorithm::RS256);
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
        validation.set_audience(&[TOKEN_URL]);
        let jwk: serde_json::Value = serde_json::from_str(PUBLIC_JWK).unwrap();
        let public = jsonwebtoken::DecodingKey::from_rsa_components(
            jwk["n"].as_str().unwrap(),
            jwk["e"].as_str().unwrap(),
        )
        .unwrap();
        let claims = jsonwebtoken::decode::<serde_json::Value>(&assertion, &public, &validation)
            .unwrap()
            .claims;
        assert_eq!(claims, assertion_claims(fixture().client_email(), now));
    }

    // -- the access token -----------------------------------------------------

    #[test]
    fn an_access_token_is_kept_until_shortly_before_it_expires() {
        let now = DateTime::from_timestamp(1_790_000_000, 0).unwrap();
        let token = AccessToken::from_response(
            r#"{"access_token":"ya29.a","expires_in":3599,"token_type":"Bearer"}"#,
            now,
        )
        .unwrap();
        let at = |secs| now + chrono::Duration::seconds(secs);
        assert!(token.is_fresh(now));
        assert!(token.is_fresh(at(3599 - TOKEN_MARGIN_SECS - 1)));
        assert!(!token.is_fresh(at(3599 - TOKEN_MARGIN_SECS)));
        assert!(!token.is_fresh(at(3599)));
        assert!(!token.is_fresh(at(7200)));
    }

    #[test]
    fn a_token_answer_without_a_token_is_none() {
        let now = Utc::now();
        for body in [
            "",
            "{}",
            r#"{"expires_in":3599}"#,
            r#"{"access_token":"","expires_in":3599}"#,
            r#"{"access_token":"ya29.a"}"#,
            r#"{"access_token":"ya29.a","expires_in":-5}"#,
            r#"{"error":"invalid_grant"}"#,
        ] {
            assert!(AccessToken::from_response(body, now).is_none(), "{body}");
        }
    }
}
