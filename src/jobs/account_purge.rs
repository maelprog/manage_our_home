use std::time::Duration as StdDuration;

use serde_json::json;
use sqlx::PgPool;
use tokio::time::interval;

const PURGE_GRACE_DAYS: i64 = 30;
const POLL_INTERVAL_SECS: u64 = 3600;

/// Polling worker (architecture.md's `scheduled_notifications` pattern):
/// finds accounts whose 30-day grace period has elapsed and purges their
/// PII, matching the RGPD Art. 17 erasure right (AC #6). Content the user
/// created stays in place for future epics to attribute to
/// "Utilisateur supprimé" — only `users`/`oauth_identities`/`sessions`
/// rows for this account are removed here.
pub async fn run(pool: PgPool) {
    let mut ticker = interval(StdDuration::from_secs(POLL_INTERVAL_SECS));
    loop {
        ticker.tick().await;
        if let Err(e) = purge_due_accounts(&pool).await {
            tracing::error!(error = ?e, "account purge job failed");
        }
    }
}

pub async fn purge_due_accounts(pool: &PgPool) -> Result<(), sqlx::Error> {
    let due = sqlx::query!(
        r#"
        SELECT id FROM users
        WHERE deletion_requested_at IS NOT NULL
          AND deletion_requested_at < now() - ($1 || ' days')::interval
          AND deleted_at IS NULL
        "#,
        PURGE_GRACE_DAYS.to_string()
    )
    .fetch_all(pool)
    .await?;

    for row in due {
        let mut tx = pool.begin().await?;
        sqlx::query!("DELETE FROM oauth_identities WHERE user_id = $1", row.id)
            .execute(&mut *tx)
            .await?;
        sqlx::query!("DELETE FROM sessions WHERE user_id = $1", row.id)
            .execute(&mut *tx)
            .await?;
        sqlx::query!(
            r#"
            UPDATE users
            SET email = 'deleted-' || id || '@deleted.invalid',
                password_hash = NULL,
                display_name = 'Utilisateur supprimé',
                deleted_at = now()
            WHERE id = $1
            "#,
            row.id
        )
        .execute(&mut *tx)
        .await?;
        crate::audit::record(
            &mut tx,
            None,
            "account_purged",
            "user",
            &row.id.to_string(),
            json!({}),
        )
        .await?;
        tx.commit().await?;
    }

    Ok(())
}
