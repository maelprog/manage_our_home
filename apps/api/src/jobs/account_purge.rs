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
use chrono::{DateTime, Utc};
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

    // `delete_account` refuses an owner, but an account can still become
    // one during its grace period — by creating a group, or by being handed
    // one. Its membership goes like any other; a group it owned passes to
    // the member `successor` picks, and a group it was alone in stays with
    // no member, like the rest of the content shared under it.
    let memberships = sqlx::query!(
        r#"DELETE FROM group_members WHERE user_id = $1
           RETURNING group_id, role::text AS "role!""#,
        user_id
    )
    .fetch_all(&mut *tx)
    .await?;
    for owned in memberships.iter().filter(|m| m.role == "owner") {
        let remaining: Vec<RemainingMember> = sqlx::query!(
            r#"SELECT user_id, role::text = 'admin' AS "is_admin!", joined_at
               FROM group_members WHERE group_id = $1 FOR UPDATE"#,
            owned.group_id
        )
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|r| RemainingMember {
            user_id: r.user_id,
            is_admin: r.is_admin,
            joined_at: r.joined_at,
        })
        .collect();
        let Some(new_owner_id) = successor(&remaining) else {
            continue;
        };
        sqlx::query!(
            "UPDATE group_members SET role = 'owner' WHERE group_id = $1 AND user_id = $2",
            owned.group_id,
            new_owner_id
        )
        .execute(&mut *tx)
        .await?;
        // `transfer_ownership`'s entry, with no actor: the purge made it.
        crate::audit::record(
            &mut tx,
            None,
            "ownership_transferred",
            "group",
            &owned.group_id.to_string(),
            json!({ "new_owner_id": new_owner_id, "reason": "account_purged" }),
        )
        .await?;
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

/// A remaining member of a group whose owner is being purged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemainingMember {
    pub user_id: Uuid,
    pub is_admin: bool,
    pub joined_at: DateTime<Utc>,
}

/// The member who inherits a purged owner's group: the longest-standing
/// admin, otherwise the longest-standing member; `None` when nobody is
/// left. Equal `joined_at` falls back to `user_id`, so the choice never
/// depends on the order the rows come back in.
pub fn successor(remaining: &[RemainingMember]) -> Option<Uuid> {
    remaining
        .iter()
        .min_by_key(|m| (!m.is_admin, m.joined_at, m.user_id))
        .map(|m| m.user_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn member(id: u128, is_admin: bool, day: u32) -> RemainingMember {
        RemainingMember {
            user_id: Uuid::from_u128(id),
            is_admin,
            joined_at: Utc.with_ymd_and_hms(2026, 1, day, 12, 0, 0).unwrap(),
        }
    }

    #[test]
    fn the_longest_standing_admin_inherits_even_behind_older_members() {
        let remaining = [
            member(1, false, 1),
            member(2, true, 5),
            member(3, true, 3),
            member(4, false, 2),
        ];
        assert_eq!(successor(&remaining), Some(Uuid::from_u128(3)));
    }

    #[test]
    fn without_an_admin_the_longest_standing_member_inherits() {
        let remaining = [
            member(1, false, 4),
            member(2, false, 2),
            member(3, false, 9),
        ];
        assert_eq!(successor(&remaining), Some(Uuid::from_u128(2)));
    }

    #[test]
    fn a_group_left_with_no_member_has_no_successor() {
        assert_eq!(successor(&[]), None);
    }

    #[test]
    fn a_tie_on_joined_at_is_broken_by_user_id_whatever_the_row_order() {
        let a = [member(7, true, 3), member(5, true, 3)];
        let b = [member(5, true, 3), member(7, true, 3)];
        assert_eq!(successor(&a), Some(Uuid::from_u128(5)));
        assert_eq!(successor(&b), Some(Uuid::from_u128(5)));
    }
}
