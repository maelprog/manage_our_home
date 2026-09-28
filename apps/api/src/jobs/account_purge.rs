//! Account purge (RGPD art. 17): once the 30-day grace period after
//! `POST /account/delete` has elapsed, erase the account.
//!
//! The `users` row is anonymised, not deleted: the content the account
//! shared with its groups keeps a `created_by` pointing at it. Because the
//! row survives, no `ON DELETE CASCADE` towards `users(id)` ever fires, so
//! every personal row is deleted here by name (#139, #255). The split
//! below is the controller's decision of 2026-09-19:
//!
//! - deleted — the tables the schema already meant to delete with the
//!   account (`ON DELETE CASCADE`): `oauth_identities`, `sessions`,
//!   `email_verification_tokens`, `password_reset_tokens`,
//!   `group_members`, `message_read_state`, `event_assignees`; and the
//!   personal rows among the others: the account's own `audit_log`
//!   entries, the `invitations` it sent (with the third-party addresses
//!   they hold) and its `calendar_imports` (with their `feed_url`, a
//!   bearer credential to an external calendar);
//! - kept, attributed to the anonymised row — the content shared with the
//!   group: `events`, `event_attachments` and their objects,
//!   `event_occurrence_completions`, `messages`, `stock_items`, `recipes`,
//!   `meal_history`, `grocery_items`, `budget_entries`, `groups`. That
//!   survival is written into the terms of service.

use std::time::Duration as StdDuration;

use anyhow::Context;
use serde_json::json;
use sqlx::PgPool;
use tokio::time::{interval, MissedTickBehavior};
use uuid::Uuid;

use crate::attachment_reconcile::ensure_bypasses_rls;

const PURGE_GRACE_DAYS: i64 = 30;
const POLL_INTERVAL_SECS: u64 = 3600;

/// Polling worker. `pool` must be the `BYPASSRLS` admin pool
/// (`AppState.admin_db`): five of the tables it deletes from are under
/// forced RLS policies, and on any other role the pass refuses rather than
/// stamp an account purged with its memberships left in place.
pub async fn run(pool: PgPool) {
    let mut ticker = interval(StdDuration::from_secs(POLL_INTERVAL_SECS));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if let Err(e) = purge_due_accounts(&pool).await {
            tracing::error!(error = ?e, "account purge job failed");
        }
    }
}

/// One pass: every account past its grace period, each in its own
/// transaction.
pub async fn purge_due_accounts(pool: &PgPool) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    ensure_bypasses_rls(&mut conn).await.context(
        "account purge refusing to run: `group_members`, `message_read_state`, \
         `event_assignees`, `invitations` and `calendar_imports` are FORCE ROW LEVEL \
         SECURITY, so on a role that does not bypass it their DELETEs match no row and \
         the account would be stamped purged with its memberships, invitations and \
         calendar feeds left in place. Point ADMIN_DATABASE_URL at the BYPASSRLS \
         admin_role (see apps/api/README.md).",
    )?;
    drop(conn);

    let due = sqlx::query_scalar!(
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

    for user_id in due {
        if let Err(e) = purge_account(pool, user_id).await {
            tracing::error!(error = ?e, %user_id, "account purge failed for one account");
        }
    }

    Ok(())
}

async fn purge_account(pool: &PgPool, user_id: Uuid) -> anyhow::Result<()> {
    let mut tx = crate::db::begin(pool).await?;

    // First, so that the owner check reads the rows it deletes, under
    // their lock. `delete_account` refuses an owner, but an account can
    // still become one during its grace period — by creating a group, or
    // by being handed one. Dropping that membership would leave the group
    // without an owner, so the account waits — rolled back, retried next
    // pass — until ownership moves on or the group is deleted.
    let roles = sqlx::query_scalar!(
        r#"DELETE FROM group_members WHERE user_id = $1 RETURNING role::text AS "role!""#,
        user_id
    )
    .fetch_all(&mut *tx)
    .await?;
    if roles.iter().any(|role| role == "owner") {
        tx.rollback().await?;
        tracing::warn!(
            %user_id,
            "account purge deferred: the account owns a group; ownership must be transferred first"
        );
        return Ok(());
    }

    sqlx::query!("DELETE FROM oauth_identities WHERE user_id = $1", user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!("DELETE FROM sessions WHERE user_id = $1", user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!(
        "DELETE FROM email_verification_tokens WHERE user_id = $1",
        user_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM password_reset_tokens WHERE user_id = $1",
        user_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM message_read_state WHERE user_id = $1", user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!("DELETE FROM event_assignees WHERE user_id = $1", user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!("DELETE FROM audit_log WHERE actor_user_id = $1", user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!("DELETE FROM invitations WHERE created_by = $1", user_id)
        .execute(&mut *tx)
        .await?;
    // `calendar_import_events` goes with it (ON DELETE CASCADE); the
    // imported `events` stay, as when a member deletes an import.
    sqlx::query!(
        "DELETE FROM calendar_imports WHERE created_by = $1",
        user_id
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        r#"
        UPDATE users
        SET email = 'deleted-' || id || '@deleted.invalid',
            password_hash = NULL,
            display_name = 'Utilisateur supprimé',
            age_declared_at = NULL,
            deleted_at = now()
        WHERE id = $1
        "#,
        user_id
    )
    .execute(&mut *tx)
    .await?;
    crate::audit::record(
        &mut tx,
        None,
        "account_purged",
        "user",
        &user_id.to_string(),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
