use std::time::Duration as StdDuration;

use anyhow::Context;
use sqlx::PgPool;
use tokio::time::interval;
use uuid::Uuid;

use crate::agenda::reminders::{refill_notifications, EventTimes};
use crate::attachment_reconcile::ensure_bypasses_rls;
use crate::email::EmailSender;

const SEND_POLL_INTERVAL_SECS: u64 = 60;
const REFILL_POLL_INTERVAL_SECS: u64 = 3600;
const MAX_SEND_ATTEMPTS: i32 = 5;
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
pub async fn run(pool: PgPool, email: EmailSender) {
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

    let mut ticker = interval(StdDuration::from_secs(SEND_POLL_INTERVAL_SECS));
    loop {
        ticker.tick().await;
        let send = |to: String, subject: String, body: String| {
            let email = email.clone();
            async move { email.send(&to, &subject, body).await }
        };
        if let Err(e) = send_due_notifications(&pool, send).await {
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

/// One pass: every due, pending notification is sent through `send(to,
/// subject, body)` to the event's creator, then marked sent, or failed
/// once `MAX_SEND_ATTEMPTS` sends were refused.
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
/// mailer, still gets that one.
pub async fn send_due_notifications<F, Fut>(pool: &PgPool, send: F) -> anyhow::Result<()>
where
    F: Fn(String, String, String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    ensure_admin_pool(pool, "sending due reminders").await?;

    let due = sqlx::query!(
        r#"
        SELECT sn.id, sn.occurrence_at, sn.attempts, e.title, e.created_by
        FROM scheduled_notifications sn
        JOIN events e ON e.id = sn.event_id
        WHERE sn.status = 'pending' AND sn.fire_at <= now()
        "#
    )
    .fetch_all(pool)
    .await?;

    for row in due {
        let Some(to) = sqlx::query_scalar!(
            r#"
            SELECT email FROM users
            WHERE id = $1 AND deactivated_at IS NULL AND deleted_at IS NULL
            "#,
            row.created_by,
        )
        .fetch_optional(pool)
        .await?
        else {
            mark_recipient_gone(pool, row.id).await?;
            continue;
        };

        let subject = format!("Rappel : {}", row.title);
        let body = format!(
            "Rappel pour « {} » prévu le {}.",
            row.title,
            row.occurrence_at.format("%d/%m/%Y %H:%M")
        );

        match send(to, subject, body).await {
            Ok(()) => mark_sent(pool, row.id).await?,
            Err(e) => mark_failed(pool, row.id, row.attempts, &e.to_string()).await?,
        }
    }

    Ok(())
}

/// Refuses a pool whose role does not bypass RLS, naming the pass and the
/// hazard, like the other workers on the admin pool (#215, #139, #138).
async fn ensure_admin_pool(pool: &PgPool, pass: &str) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    ensure_bypasses_rls(&mut conn).await.with_context(|| {
        format!(
            "scheduled notifications refusing to run ({pass}): `scheduled_notifications` \
             and `events` are FORCE ROW LEVEL SECURITY, so on a role that does not bypass \
             it no reminder is ever found due and none is sent. Point ADMIN_DATABASE_URL at \
             the BYPASSRLS admin_role (see apps/api/README.md)."
        )
    })
}

async fn mark_recipient_gone(pool: &PgPool, id: Uuid) -> anyhow::Result<()> {
    sqlx::query!(
        "UPDATE scheduled_notifications SET status = 'failed', last_error = $2 WHERE id = $1",
        id,
        RECIPIENT_GONE,
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
