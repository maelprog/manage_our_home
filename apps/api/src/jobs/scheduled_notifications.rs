use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use futures::channel::mpsc;
use futures::stream::{FuturesOrdered, FuturesUnordered};
use futures::{SinkExt, StreamExt};
use manage_our_home_shared::validation::rgpd::sanitize_email_line;
use sqlx::PgPool;
use tokio::time::interval;
use uuid::Uuid;

use crate::agenda::reminders::{refill_notifications, EventTimes};
use crate::email::EmailSender;
use crate::notifications::{push, ReminderChannel};

const SEND_POLL_INTERVAL_SECS: u64 = 60;
const REFILL_POLL_INTERVAL_SECS: u64 = 3600;
const MAX_SEND_ATTEMPTS: i32 = 5;
/// How many due notifications are pushed at once (#321). Each one waits on
/// its member's push services, up to `push::client`'s timeout per device:
/// in a single queue, one slow service or one account full of devices held
/// back every reminder behind it.
pub const MAX_CONCURRENT_REMINDERS: usize = 8;
/// How many devices of one member are pushed at once (#321). With
/// [`MAX_CONCURRENT_REMINDERS`], a pass has at most 8 × 10 push requests in
/// flight.
pub const MAX_CONCURRENT_DEVICES: usize = 10;
/// `last_error` of a notification retired because its recipient's account
/// is deactivated or purged.
const RECIPIENT_GONE: &str = "recipient account deactivated or purged";

/// Persisted job-queue worker (architecture.md correction #4): reminders
/// must survive restarts/deploys, so this polls `scheduled_notifications`
/// rather than relying on an in-process scheduler. Runs two independent
/// loops on their own tickers: sending due notifications frequently, and
/// refilling the rolling window for recurring events' reminders less
/// often (occurrences further than `NOTIFICATION_WINDOW_DAYS` out don't
/// need to exist yet).
///
/// `pool` must be the `BYPASSRLS` admin pool (`AppState.admin_db`): both
/// loops read across every family, from tables under forced RLS policies,
/// and each pass refuses any other role (#293).
///
/// `push` is the server's VAPID identity, `None` when notifications are
/// off (no `VAPID_PRIVATE_KEY`): a push is then a failure, retried and
/// finally retired like any other.
pub async fn run(pool: PgPool, email: EmailSender, push: Option<Arc<push::Vapid>>) {
    let pool_for_refill = pool.clone();
    tokio::spawn(async move {
        let mut ticker = interval(StdDuration::from_secs(REFILL_POLL_INTERVAL_SECS));
        loop {
            ticker.tick().await;
            if let Err(e) = refill_recurring_reminders(&pool_for_refill).await {
                tracing::error!(error = ?e, "scheduled_notifications refill failed");
            }
        }
    });

    let client = push::client();
    let mut ticker = interval(StdDuration::from_secs(SEND_POLL_INTERVAL_SECS));
    loop {
        ticker.tick().await;
        let send = |to: String, subject: String, body: String| {
            let email = email.clone();
            async move { email.send(&to, &subject, body).await }
        };
        let send_push = |endpoint: String, ttl_secs: i64| {
            let client = client.clone();
            let vapid = push.clone();
            async move {
                match vapid {
                    Some(vapid) => push::send(&client, &vapid, &endpoint, ttl_secs).await,
                    None => push::PushOutcome::Failed(
                        "notifications are not configured on this server".into(),
                    ),
                }
            }
        };
        if let Err(e) = send_due_notifications(&pool, send, send_push).await {
            tracing::error!(error = ?e, "scheduled_notifications send failed");
        }
    }
}

/// Re-runs `refill_notifications` for every reminder attached to a
/// recurring event, materializing any newly-in-range occurrences. Cheap at
/// household scale; revisit with a `last_refilled_at` cursor if the number
/// of recurring reminders ever grows large enough to matter.
async fn refill_recurring_reminders(pool: &PgPool) -> anyhow::Result<()> {
    ensure_admin_pool(pool, "refilling recurring reminders").await?;
    let reminders = sqlx::query!(
        r#"
        SELECT r.id as reminder_id, r.offset_minutes, e.id as event_id, e.starts_at, e.ends_at, e.all_day, e.rrule
        FROM event_reminders r
        JOIN events e ON e.id = r.event_id
        WHERE e.rrule IS NOT NULL
        "#
    )
    .fetch_all(pool)
    .await?;

    for row in reminders {
        let mut tx = crate::db::begin(pool).await?;
        if let Err(e) = refill_notifications(
            &mut tx,
            row.reminder_id,
            row.event_id,
            EventTimes {
                starts_at: row.starts_at,
                ends_at: row.ends_at,
                all_day: row.all_day,
                rrule: row.rrule.as_deref(),
            },
            row.offset_minutes,
        )
        .await
        {
            tracing::error!(error = ?e, reminder_id = %row.reminder_id, "failed to refill notifications");
            tx.rollback().await.ok();
            continue;
        }
        tx.commit().await?;
    }

    Ok(())
}

/// One pass: every due, pending notification goes to the event's creator
/// on the channel their account chose (`users.reminder_channel`, #306) —
/// by email through `send(to, subject, body)`, by notification through
/// `send_push(endpoint, ttl_secs)` once per subscribed device — and is
/// then settled by [`settle`]: sent, retried (failed once
/// `MAX_SEND_ATTEMPTS` attempts were refused), or retired at once.
///
/// A push service answering 404 or 410 has its subscription deleted on the
/// spot: the browser expired or withdrew it. A member who chose
/// notifications and is left without any device gets nothing, and no
/// email instead (#306); the application warns them where they set
/// reminders.
///
/// `pool` must be the `BYPASSRLS` admin pool (`AppState.admin_db`): the
/// pass reads `scheduled_notifications` and `events` across every family,
/// and both are under forced RLS policies. On any other role it would find
/// nothing due and report success; it refuses instead (#293).
///
/// The creator's account is read again right before each send, not once
/// for the whole pass. One found deactivated (`deactivated_at`) or purged
/// (`deleted_at`, its address rewritten to `deleted-<id>@deleted.invalid`)
/// at that point is not written to (#291): the notification is marked
/// failed without an attempt, so a reactivation does not send it late and
/// a purged creator's are not read again on every pass. Those not yet due
/// stay pending, and go if the account is reactivated by then. An account
/// deactivated after that read, while its email is being handed to the
/// mailer or its devices are being pushed, still gets that one.
///
/// Notifications are read and emailed one after the other, but pushed
/// concurrently (#321): up to [`MAX_CONCURRENT_REMINDERS`] notifications
/// at once, and up to [`MAX_CONCURRENT_DEVICES`] devices of each, every
/// device keeping `push::client`'s own timeout. A push service that hangs
/// holds back its own member's reminder, and the reading of the next ones
/// goes on meanwhile — while fewer than [`MAX_CONCURRENT_REMINDERS`]
/// reminders hang. Once that many do, no other is pushed until one of them
/// answers or times out, and the reading stops too as soon as the channel
/// between both sides is full: `MAX_CONCURRENT_REMINDERS + 1` reminders
/// read and waiting (its bound plus the reading side's own slot, per
/// `futures::channel::mpsc::channel`).
///
/// A notification that cannot be seen through (a database read or write
/// failing) is logged and left pending for the next pass, without stopping
/// the others; the pass then reports how many there were.
pub async fn send_due_notifications<F, Fut, P, PFut>(
    pool: &PgPool,
    send: F,
    send_push: P,
) -> anyhow::Result<()>
where
    F: Fn(String, String, String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
    P: Fn(String, i64) -> PFut,
    PFut: std::future::Future<Output = push::PushOutcome>,
{
    ensure_admin_pool(pool, "sending due reminders").await?;

    let due: Vec<DueRow> = sqlx::query!(
        r#"
        SELECT sn.id, sn.occurrence_at, sn.attempts, e.title, e.created_by
        FROM scheduled_notifications sn
        JOIN events e ON e.id = sn.event_id
        WHERE sn.status = 'pending' AND sn.fire_at <= now()
        "#
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| DueRow {
        id: r.id,
        occurrence_at: r.occurrence_at,
        attempts: r.attempts,
        title: r.title,
        created_by: r.created_by,
    })
    .collect();

    // The reading side hands each notification over once its account was
    // read and its email sent; the pushing side takes up to
    // `MAX_CONCURRENT_REMINDERS` at a time. Both run in this one task, so
    // neither waits for the other beyond the channel's bound.
    let (handed, taken) = mpsc::channel::<Reached>(MAX_CONCURRENT_REMINDERS);
    let (unread, unpushed) = futures::join!(
        read_all(pool, &send, due, handed),
        push_all(pool, &send_push, taken),
    );

    match unread + unpushed {
        0 => Ok(()),
        n => anyhow::bail!("{n} due reminder(s) could not be sent or settled, see above"),
    }
}

/// Logs a notification the pass could not see through. It stays pending,
/// and is taken again on the next pass.
fn unsent(id: Uuid, error: &anyhow::Error) {
    tracing::error!(error = ?error, notification_id = %id, "failed to send a due reminder");
}

/// The reading side: [`reach`] each due notification in turn, and hand the
/// ones still to push and settle over to [`push_all`]. Returns how many
/// failed.
async fn read_all<F, Fut>(
    pool: &PgPool,
    send: &F,
    due: Vec<DueRow>,
    mut handed: mpsc::Sender<Reached>,
) -> usize
where
    F: Fn(String, String, String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let mut failed = 0;
    for row in due {
        let id = row.id;
        match reach(pool, send, row).await {
            Ok(Some(reached)) => {
                // `push_all` takes until this side hangs up.
                if handed.send(reached).await.is_err() {
                    break;
                }
            }
            Ok(None) => {}
            Err(e) => {
                unsent(id, &e);
                failed += 1;
            }
        }
    }
    failed
}

/// The pushing side: [`push_and_settle`] what [`read_all`] hands over, up
/// to [`MAX_CONCURRENT_REMINDERS`] at once. Returns how many failed.
async fn push_all<P, PFut>(
    pool: &PgPool,
    send_push: &P,
    mut taken: mpsc::Receiver<Reached>,
) -> usize
where
    P: Fn(String, i64) -> PFut,
    PFut: std::future::Future<Output = push::PushOutcome>,
{
    let mut in_flight = FuturesUnordered::new();
    let mut open = true;
    let mut failed = 0;
    loop {
        tokio::select! {
            next = taken.next(), if open && in_flight.len() < MAX_CONCURRENT_REMINDERS => {
                match next {
                    Some(reached) => in_flight.push(push_and_settle(pool, send_push, reached)),
                    None => open = false,
                }
            }
            Some((id, settled)) = in_flight.next() => {
                if let Err(e) = settled {
                    unsent(id, &e);
                    failed += 1;
                }
            }
            else => break,
        }
    }
    failed
}

/// A due, pending notification, as the pass reads it.
struct DueRow {
    id: Uuid,
    occurrence_at: DateTime<Utc>,
    attempts: i32,
    title: String,
    created_by: Uuid,
}

/// A due notification whose member was found active and emailed if their
/// channel includes email, waiting for its pushes and its settlement.
struct Reached {
    id: Uuid,
    attempts: i32,
    created_by: Uuid,
    occurrence_at: DateTime<Utc>,
    push: bool,
    email: Option<Result<(), String>>,
}

/// Reads the creator's account right before sending, retires the
/// notification if it is gone, and sends the email if their channel
/// includes it. `None` once retired.
async fn reach<F, Fut>(pool: &PgPool, send: &F, row: DueRow) -> anyhow::Result<Option<Reached>>
where
    F: Fn(String, String, String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let Some(recipient) = sqlx::query!(
        r#"
            SELECT email, reminder_channel FROM users
            WHERE id = $1 AND deactivated_at IS NULL AND deleted_at IS NULL
            "#,
        row.created_by,
    )
    .fetch_optional(pool)
    .await?
    else {
        retire(pool, row.id, RECIPIENT_GONE).await?;
        return Ok(None);
    };
    // The column's CHECK (0020) admits nothing else.
    let channel = ReminderChannel::parse(&recipient.reminder_channel).ok_or_else(|| {
        anyhow::anyhow!("unknown reminder_channel {:?}", recipient.reminder_channel)
    })?;

    let email = if channel.includes_email() {
        let body = reminder_email_body(&row.title, row.occurrence_at);
        Some(
            send(recipient.email, REMINDER_SUBJECT.to_owned(), body)
                .await
                .map_err(|e| e.to_string()),
        )
    } else {
        None
    };

    Ok(Some(Reached {
        id: row.id,
        attempts: row.attempts,
        created_by: row.created_by,
        occurrence_at: row.occurrence_at,
        push: channel.includes_push(),
        email,
    }))
}

/// Pushes every device of the member if their channel includes push, then
/// settles the notification. Returns its id with the outcome.
async fn push_and_settle<P, PFut>(
    pool: &PgPool,
    send_push: &P,
    reached: Reached,
) -> (Uuid, anyhow::Result<()>)
where
    P: Fn(String, i64) -> PFut,
    PFut: std::future::Future<Output = push::PushOutcome>,
{
    let id = reached.id;
    let settled = async {
        let mut pushed = Vec::new();
        if reached.push {
            pushed =
                push_devices(pool, send_push, reached.created_by, reached.occurrence_at).await?;
        }
        match settle(reached.email, &pushed) {
            Settlement::Sent => mark_sent(pool, id).await,
            Settlement::Retry(error) => mark_failed(pool, id, reached.attempts, &error).await,
            Settlement::Retire(reason) => retire(pool, id, reason).await,
        }
    };
    (id, settled.await)
}

/// Pushes every device of `user`, up to [`MAX_CONCURRENT_DEVICES`] at
/// once, and records each one's answer. Every device is pushed and
/// recorded even when one fails to be; the first such error is returned
/// once they all answered.
async fn push_devices<P, PFut>(
    pool: &PgPool,
    send_push: &P,
    user: Uuid,
    occurrence_at: DateTime<Utc>,
) -> anyhow::Result<Vec<push::PushOutcome>>
where
    P: Fn(String, i64) -> PFut,
    PFut: std::future::Future<Output = push::PushOutcome>,
{
    let devices = sqlx::query!(
        r#"SELECT id, endpoint, consecutive_failures, failing_since FROM push_subscriptions
                   WHERE user_id = $1 ORDER BY created_at"#,
        user,
    )
    .fetch_all(pool)
    .await?;
    let ttl = push::ttl_secs(Utc::now(), occurrence_at);

    let mut in_flight = FuturesOrdered::new();
    let mut answers = Vec::with_capacity(devices.len());
    for device in devices {
        if in_flight.len() == MAX_CONCURRENT_DEVICES {
            answers.extend(in_flight.next().await);
        }
        in_flight.push_back(push_device(
            pool,
            send_push,
            device.id,
            device.endpoint,
            device.consecutive_failures,
            device.failing_since,
            ttl,
        ));
    }
    while let Some(answer) = in_flight.next().await {
        answers.push(answer);
    }
    answers.into_iter().collect()
}

/// Pushes one device and records its answer.
async fn push_device<P, PFut>(
    pool: &PgPool,
    send_push: &P,
    id: Uuid,
    endpoint: String,
    consecutive_failures: i32,
    failing_since: Option<DateTime<Utc>>,
    ttl_secs: i64,
) -> anyhow::Result<push::PushOutcome>
where
    P: Fn(String, i64) -> PFut,
    PFut: std::future::Future<Output = push::PushOutcome>,
{
    let outcome = send_push(endpoint, ttl_secs).await;
    record_device(pool, id, &outcome, consecutive_failures, failing_since).await?;
    Ok(outcome)
}

/// Forgets a device or records its answer, per [`push::device_after`].
async fn record_device(
    pool: &PgPool,
    id: Uuid,
    outcome: &push::PushOutcome,
    consecutive_failures: i32,
    failing_since: Option<DateTime<Utc>>,
) -> anyhow::Result<()> {
    let delivered = *outcome == push::PushOutcome::Delivered;
    match push::device_after(outcome, consecutive_failures, failing_since, Utc::now()) {
        // Gone, or failing past both bounds.
        push::DeviceAfter::Forget => {
            sqlx::query!("DELETE FROM push_subscriptions WHERE id = $1", id)
                .execute(pool)
                .await?;
        }
        push::DeviceAfter::Keep {
            consecutive_failures,
            failing_since,
        } => {
            sqlx::query!(
                r#"UPDATE push_subscriptions
                               SET consecutive_failures = $2, failing_since = $3,
                                   last_success_at = CASE WHEN $4 THEN now() ELSE last_success_at END
                               WHERE id = $1"#,
                id,
                consecutive_failures,
                failing_since,
                delivered
            )
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

/// Subject of every reminder email. It names no event (#146): a title can
/// be sensitive (« IRM », « rendez-vous Dr X »), and the subject is the part
/// of an email that travels furthest and is protected least — it shows in
/// the recipient's message list and is indexed by their mail provider. The
/// title goes in the body only; `docs/privacy-policy.md` records the choice.
pub const REMINDER_SUBJECT: &str = "Rappel d'un événement à venir";

/// Body of a reminder email. The title is user input travelling into a
/// `text/plain` email, so it goes through `sanitize_email_line` first: no
/// line break, no invisible character, and a bounded length (#269).
fn reminder_email_body(title: &str, occurrence_at: DateTime<Utc>) -> String {
    format!(
        "Rappel pour « {} » prévu le {}.",
        sanitize_email_line(title),
        occurrence_at.format("%d/%m/%Y %H:%M")
    )
}

/// Refuses a pool whose role does not bypass RLS, like the other workers
/// on the admin pool (#215, #139, #138). The check is the one
/// `attachment_reconcile::ensure_bypasses_rls` makes, but not its message:
/// that one names `event_attachments` and the bucket, which would send
/// whoever reads this pass's ERROR line to the wrong hazard.
async fn ensure_admin_pool(pool: &PgPool, pass: &str) -> anyhow::Result<()> {
    let bypasses = sqlx::query_scalar!(
        "SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname = current_user"
    )
    .fetch_optional(pool)
    .await?
    .flatten()
    .unwrap_or(false);

    if !bypasses {
        anyhow::bail!(
            "scheduled notifications refusing to run ({pass}): the database connection does \
             not bypass RLS, and `scheduled_notifications` and `events` are FORCE ROW LEVEL \
             SECURITY, so no reminder would ever be found due and none would be sent. Point \
             ADMIN_DATABASE_URL at the BYPASSRLS admin_role (see apps/api/README.md)."
        );
    }
    Ok(())
}

/// What becomes of one due notification once its channels have answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settlement {
    /// At least one channel reached the member.
    Sent,
    /// Nothing reached them, but something may on the next pass: one
    /// attempt is counted ([`MAX_SEND_ATTEMPTS`] in all).
    Retry(String),
    /// Nothing reached them and nothing will: retired at once, without an
    /// attempt, with this reason.
    Retire(&'static str),
}

/// `last_error` of a notification retired because its recipient chose
/// notifications and has no device subscribed — or every one they had
/// turned out expired. No fallback to email (#306).
pub const NO_PUSH_SUBSCRIPTION: &str = "no active push subscription";

/// Settles one notification from what each channel answered: `email` is
/// `None` when the member's channel does not include email, and `push`
/// holds one answer per subscribed device (empty when the channel does not
/// include push, or when they have none).
///
/// Sent as soon as one channel reached the member — the other is not
/// retried, so a member on « both » whose email went through does not get
/// it twice. Otherwise a failure that may pass next time (an email
/// refused, a push service down) counts an attempt; and a member left with
/// no way to reach them (notifications only, no device, or only expired
/// ones) has the notification retired without one.
pub fn settle(email: Option<Result<(), String>>, push: &[push::PushOutcome]) -> Settlement {
    let pushed = push.contains(&push::PushOutcome::Delivered);
    if matches!(email, Some(Ok(()))) || pushed {
        return Settlement::Sent;
    }
    let errors: Vec<String> = email
        .and_then(Result::err)
        .into_iter()
        .chain(push.iter().filter_map(|o| match o {
            push::PushOutcome::Failed(e) => Some(e.clone()),
            _ => None,
        }))
        .collect();
    if errors.is_empty() {
        Settlement::Retire(NO_PUSH_SUBSCRIPTION)
    } else {
        Settlement::Retry(errors.join("; "))
    }
}

/// Marks a notification failed with `reason`, without counting an attempt.
async fn retire(pool: &PgPool, id: Uuid, reason: &str) -> anyhow::Result<()> {
    sqlx::query!(
        "UPDATE scheduled_notifications SET status = 'failed', last_error = $2 WHERE id = $1",
        id,
        reason,
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn mark_sent(pool: &PgPool, id: Uuid) -> anyhow::Result<()> {
    sqlx::query!(
        "UPDATE scheduled_notifications SET status = 'sent' WHERE id = $1",
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn mark_failed(pool: &PgPool, id: Uuid, attempts: i32, error: &str) -> anyhow::Result<()> {
    let attempts = attempts + 1;
    let status = if attempts >= MAX_SEND_ATTEMPTS {
        "failed"
    } else {
        "pending"
    };
    sqlx::query!(
        "UPDATE scheduled_notifications SET attempts = $2, last_error = $3, status = $4 WHERE id = $1",
        id,
        attempts,
        error,
        status,
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 3, 14, 30, 0).unwrap()
    }

    #[test]
    fn subject_names_no_event() {
        assert_eq!(REMINDER_SUBJECT, "Rappel d'un événement à venir");
    }

    #[test]
    fn body_carries_title_and_date() {
        assert_eq!(
            reminder_email_body("Dîner", at()),
            "Rappel pour « Dîner » prévu le 03/10/2026 14:30."
        );
    }

    #[test]
    fn body_title_cannot_open_a_line() {
        let body = reminder_email_body("IRM\r\nBcc: x@example.test\n\nfaux paragraphe", at());
        assert!(!body.contains(['\n', '\r']), "{body:?}");
        assert_eq!(
            body,
            "Rappel pour « IRM Bcc: x@example.test faux paragraphe » prévu le 03/10/2026 14:30."
        );
    }

    #[test]
    fn body_title_loses_invisible_characters() {
        assert_eq!(
            reminder_email_body("Pis\u{202e}cine\u{200b}", at()),
            "Rappel pour « Piscine » prévu le 03/10/2026 14:30."
        );
    }

    // -- settling a notification across its channels (#306) ----------------

    use push::PushOutcome::{Delivered, Failed, Gone};

    fn failed() -> push::PushOutcome {
        Failed("push service answered 503".into())
    }

    #[test]
    fn an_email_that_went_is_sent() {
        assert_eq!(settle(Some(Ok(())), &[]), Settlement::Sent);
    }

    #[test]
    fn a_refused_email_is_retried_with_its_error() {
        assert_eq!(
            settle(Some(Err("smtp down".into())), &[]),
            Settlement::Retry("smtp down".into())
        );
    }

    #[test]
    fn one_device_reached_is_enough() {
        assert_eq!(settle(None, &[Delivered]), Settlement::Sent);
        assert_eq!(settle(None, &[Gone, failed(), Delivered]), Settlement::Sent);
    }

    #[test]
    fn notifications_only_without_any_device_is_retired_without_falling_back() {
        assert_eq!(settle(None, &[]), Settlement::Retire(NO_PUSH_SUBSCRIPTION));
    }

    #[test]
    fn notifications_only_with_every_device_expired_is_retired() {
        assert_eq!(
            settle(None, &[Gone, Gone]),
            Settlement::Retire(NO_PUSH_SUBSCRIPTION)
        );
    }

    #[test]
    fn a_push_service_down_is_retried_with_its_error() {
        assert_eq!(
            settle(None, &[Gone, failed()]),
            Settlement::Retry("push service answered 503".into())
        );
    }

    #[test]
    fn both_channels_send_once_either_went() {
        assert_eq!(settle(Some(Ok(())), &[failed()]), Settlement::Sent);
        assert_eq!(settle(Some(Ok(())), &[]), Settlement::Sent);
        assert_eq!(
            settle(Some(Err("smtp down".into())), &[Delivered]),
            Settlement::Sent
        );
    }

    #[test]
    fn both_channels_failing_are_retried_with_both_errors() {
        assert_eq!(
            settle(Some(Err("smtp down".into())), &[failed()]),
            Settlement::Retry("smtp down; push service answered 503".into())
        );
        // No device on « both »: the email's failure alone is retried.
        assert_eq!(
            settle(Some(Err("smtp down".into())), &[Gone]),
            Settlement::Retry("smtp down".into())
        );
    }

    #[test]
    fn body_title_is_bounded() {
        let long = "a".repeat(10_000);
        let expected_title = format!("{}…", "a".repeat(80));
        assert_eq!(
            reminder_email_body(&long, at()),
            format!("Rappel pour « {expected_title} » prévu le 03/10/2026 14:30.")
        );
    }
}
