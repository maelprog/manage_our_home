use std::time::Duration as StdDuration;

use sqlx::PgPool;
use tokio::time::interval;
use uuid::Uuid;

use crate::agenda::reminders::refill_notifications;
use crate::email::EmailSender;

const SEND_POLL_INTERVAL_SECS: u64 = 60;
const REFILL_POLL_INTERVAL_SECS: u64 = 3600;
const MAX_SEND_ATTEMPTS: i32 = 5;

/// Persisted job-queue worker (architecture.md correction #4): reminders
/// must survive restarts/deploys, so this polls `scheduled_notifications`
/// rather than relying on an in-process scheduler. Runs two independent
/// loops on their own tickers: sending due notifications frequently, and
/// refilling the rolling window for recurring events' reminders less
/// often (occurrences further than `NOTIFICATION_WINDOW_DAYS` out don't
/// need to exist yet).
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
        if let Err(e) = send_due_notifications(&pool, &email).await {
            tracing::error!(error = ?e, "scheduled_notifications send failed");
        }
    }
}

/// Re-runs `refill_notifications` for every reminder attached to a
/// recurring event, materializing any newly-in-range occurrences. Cheap at
/// household scale; revisit with a `last_refilled_at` cursor if the number
/// of recurring reminders ever grows large enough to matter.
async fn refill_recurring_reminders(pool: &PgPool) -> anyhow::Result<()> {
    let reminders = sqlx::query!(
        r#"
        SELECT r.id as reminder_id, r.offset_minutes, e.id as event_id, e.starts_at, e.rrule
        FROM event_reminders r
        JOIN events e ON e.id = r.event_id
        WHERE e.rrule IS NOT NULL
        "#
    )
    .fetch_all(pool)
    .await?;

    for row in reminders {
        let mut tx = pool.begin().await?;
        if let Err(e) = refill_notifications(
            &mut tx,
            row.reminder_id,
            row.event_id,
            row.starts_at,
            row.rrule.as_deref(),
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

pub async fn send_due_notifications(pool: &PgPool, email: &EmailSender) -> anyhow::Result<()> {
    let due = sqlx::query!(
        r#"
        SELECT sn.id, sn.occurrence_at, sn.attempts, e.title, u.email
        FROM scheduled_notifications sn
        JOIN events e ON e.id = sn.event_id
        JOIN users u ON u.id = e.created_by
        WHERE sn.status = 'pending' AND sn.fire_at <= now()
        "#
    )
    .fetch_all(pool)
    .await?;

    for row in due {
        let subject = format!("Rappel : {}", row.title);
        let body = format!(
            "Rappel pour « {} » prévu le {}.",
            row.title,
            row.occurrence_at.format("%d/%m/%Y %H:%M")
        );

        match email.send(&row.email, &subject, body).await {
            Ok(()) => mark_sent(pool, row.id).await?,
            Err(e) => mark_failed(pool, row.id, row.attempts, &e.to_string()).await?,
        }
    }

    Ok(())
}

async fn mark_sent(pool: &PgPool, id: Uuid) -> anyhow::Result<()> {
    sqlx::query!("UPDATE scheduled_notifications SET status = 'sent' WHERE id = $1", id)
        .execute(pool)
        .await?;
    Ok(())
}

async fn mark_failed(pool: &PgPool, id: Uuid, attempts: i32, error: &str) -> anyhow::Result<()> {
    let attempts = attempts + 1;
    let status = if attempts >= MAX_SEND_ATTEMPTS { "failed" } else { "pending" };
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
