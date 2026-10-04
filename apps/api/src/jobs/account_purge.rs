//! Account purge (RGPD art. 17 and 5(1)(e)): erase an account once the
//! 30-day grace period after `POST /account/delete` has elapsed — whether
//! or not support deactivated it since (#139) — or once it has stayed
//! deactivated by the superadmin for 2 years, its holder warned by email
//! 30 days before (#256, controller's decision of 2026-09-28). The rules
//! are the pure [`purge_due`] and [`deactivation_notice_due`].
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
//!   `group_members`, `message_read_state`, `event_assignees`,
//!   `account_reactivation_requests` (#289), `push_subscriptions` (#306); and the
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
use chrono::{DateTime, Duration, Months, Utc};
use serde_json::json;
use sqlx::PgPool;
use tokio::time::{interval, MissedTickBehavior};
use uuid::Uuid;

use crate::attachment_reconcile::ensure_bypasses_rls;
use crate::email::EmailSender;

const PURGE_GRACE_DAYS: i64 = 30;
const POLL_INTERVAL_SECS: u64 = 3600;

/// Subject of the warning [`send_deactivation_notices`] sends.
pub const DEACTIVATION_NOTICE_SUBJECT: &str =
    "Votre compte Manage Our Home désactivé va être supprimé";

/// Polling worker. `pool` must be the `BYPASSRLS` admin pool
/// (`AppState.admin_db`): five of the tables it deletes from are under
/// forced RLS policies, and on any other role the pass refuses rather than
/// stamp an account purged with its memberships left in place. Each pass
/// sends the due warnings first, then purges.
pub async fn run(pool: PgPool, email: EmailSender, privacy_policy_url: String) {
    let mut ticker = interval(StdDuration::from_secs(POLL_INTERVAL_SECS));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let send = |to: String, subject: String, body: String| {
            let email = email.clone();
            async move { email.send(&to, &subject, body).await }
        };
        if let Err(e) = send_deactivation_notices(&pool, &privacy_policy_url, send).await {
            tracing::error!(error = ?e, "deactivation notice job failed");
        }
        if let Err(e) = purge_due_accounts(&pool).await {
            tracing::error!(error = ?e, "account purge job failed");
        }
    }
}

/// One pass of warnings: every deactivated account whose notice is due
/// ([`deactivation_notice_due`]) gets one email, sent through `send(to,
/// subject, body)`, and `deactivation_notice_sent_at` is stamped only once
/// the email has gone. A refused email leaves the account unstamped, to be
/// tried again on the next pass; [`deactivation_purge_at`] bounds how long
/// that can hold the purge back.
///
/// Each account is handled in its own transaction, its row locked and its
/// state read again first: a reactivation that landed in between (it
/// clears `deactivated_at`) sends nothing, and one that comes during the
/// send waits for the stamp, then clears it.
pub async fn send_deactivation_notices<F, Fut>(
    pool: &PgPool,
    privacy_policy_url: &str,
    send: F,
) -> anyhow::Result<()>
where
    F: Fn(String, String, String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let candidates = sqlx::query_scalar!(
        r#"
        SELECT id FROM users
        WHERE deactivated_at IS NOT NULL
          AND deactivation_notice_sent_at IS NULL
          AND deleted_at IS NULL
        "#
    )
    .fetch_all(pool)
    .await?;

    for user_id in candidates {
        let mut tx = crate::db::begin(pool).await?;
        let Some(row) = sqlx::query!(
            r#"
            SELECT email, deletion_requested_at, deactivated_at,
                   deactivation_notice_sent_at, deleted_at, now() AS "now!",
                   EXISTS (SELECT 1 FROM account_reactivation_requests r
                           WHERE r.user_id = users.id) AS "reactivation_requested!",
                   reactivation_refused_at
            FROM users WHERE id = $1 FOR UPDATE
            "#,
            user_id
        )
        .fetch_optional(&mut *tx)
        .await?
        else {
            continue;
        };
        let clock = PurgeClock {
            deletion_requested_at: row.deletion_requested_at,
            deactivated_at: row.deactivated_at,
            deactivation_notice_sent_at: row.deactivation_notice_sent_at,
            deleted_at: row.deleted_at,
            reactivation_requested: row.reactivation_requested,
            reactivation_refused_at: row.reactivation_refused_at,
        };
        let (true, Some(deactivated_at)) =
            (deactivation_notice_due(row.now, &clock), row.deactivated_at)
        else {
            tx.rollback().await?;
            continue;
        };
        let body = manage_our_home_shared::validation::rgpd::deactivation_notice_email_body(
            deactivated_at,
            deactivation_purge_at(deactivated_at, Some(row.now), row.reactivation_refused_at),
            privacy_policy_url,
        );
        if let Err(e) = send(row.email, DEACTIVATION_NOTICE_SUBJECT.to_string(), body).await {
            // Never log the address — PII, as in `EmailSender::send`.
            tracing::error!(error = ?e, %user_id, "deactivation notice not sent");
            tx.rollback().await?;
            continue;
        }
        sqlx::query!(
            "UPDATE users SET deactivation_notice_sent_at = $2 WHERE id = $1",
            user_id,
            row.now
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }
    Ok(())
}

/// One pass: every account [`purge_due`] names, each in its own
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

    // The candidates; `purge_account` decides, on the locked row.
    let candidates = sqlx::query_scalar!(
        r#"
        SELECT id FROM users
        WHERE (deletion_requested_at IS NOT NULL OR deactivated_at IS NOT NULL)
          AND deleted_at IS NULL
        "#
    )
    .fetch_all(pool)
    .await?;

    for user_id in candidates {
        if let Err(e) = purge_account(pool, user_id).await {
            tracing::error!(error = ?e, %user_id, "account purge failed for one account");
        }
    }

    Ok(())
}

/// Purges one account, in one transaction — if [`purge_due`] still names
/// it when its turn comes. An account may have cancelled its request
/// (`cancel_delete_account`) or been reactivated by the superadmin since
/// it was listed: the `users` row is locked and its state read again here,
/// and an account no longer due is left untouched, without an error. The
/// lock also makes a concurrent cancellation or reactivation wait for this
/// transaction, or this transaction wait for it.
pub async fn purge_account(pool: &PgPool, user_id: Uuid) -> anyhow::Result<()> {
    let mut tx = crate::db::begin(pool).await?;

    let row = sqlx::query!(
        r#"
        SELECT deletion_requested_at, deactivated_at, deactivation_notice_sent_at,
               deleted_at, now() AS "now!",
               EXISTS (SELECT 1 FROM account_reactivation_requests r
                       WHERE r.user_id = users.id) AS "reactivation_requested!",
                   reactivation_refused_at
        FROM users WHERE id = $1 FOR UPDATE
        "#,
        user_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let still_due = row.is_some_and(|r| {
        purge_due(
            r.now,
            &PurgeClock {
                deletion_requested_at: r.deletion_requested_at,
                deactivated_at: r.deactivated_at,
                deactivation_notice_sent_at: r.deactivation_notice_sent_at,
                deleted_at: r.deleted_at,
                reactivation_requested: r.reactivation_requested,
                reactivation_refused_at: r.reactivation_refused_at,
            },
        )
    });
    if !still_due {
        tx.rollback().await?;
        return Ok(());
    }

    // `delete_account` refuses an owner, but an account can still become
    // one during its grace period — by creating a group, or by being handed
    // one. Its membership goes like any other; a group it owned passes to
    // the member `successor` picks. A group with only deactivated members
    // left gets no owner (they keep their rows), and a group it was alone
    // in stays with no member, like the rest of the content shared under it.
    let memberships = sqlx::query!(
        r#"DELETE FROM group_members WHERE user_id = $1
           RETURNING group_id, role::text AS "role!""#,
        user_id
    )
    .fetch_all(&mut *tx)
    .await?;
    for owned in memberships.iter().filter(|m| m.role == "owner") {
        let remaining: Vec<RemainingMember> = sqlx::query!(
            r#"SELECT gm.user_id, gm.role::text = 'admin' AS "is_admin!", gm.joined_at,
                      u.deactivated_at IS NOT NULL AS "deactivated!",
                      u.deletion_requested_at IS NOT NULL AS "pending_deletion!"
               FROM group_members gm JOIN users u ON u.id = gm.user_id
               WHERE gm.group_id = $1 FOR UPDATE OF gm"#,
            owned.group_id
        )
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|r| RemainingMember {
            user_id: r.user_id,
            is_admin: r.is_admin,
            joined_at: r.joined_at,
            deactivated: r.deactivated,
            pending_deletion: r.pending_deletion,
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
    sqlx::query!("DELETE FROM push_subscriptions WHERE user_id = $1", user_id)
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
    sqlx::query!(
        "DELETE FROM account_reactivation_requests WHERE user_id = $1",
        user_id
    )
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
            terms_accepted_version = NULL,
            terms_accepted_at = NULL,
            reactivation_refused_at = NULL,
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

/// How long an account the superadmin deactivated is kept before the purge
/// anonymises it: 2 years, the CNIL's recommendation for an inactive
/// account — controller's decision of 2026-09-28 (#256).
pub const DEACTIVATION_RETENTION_MONTHS: u32 = 24;

/// How long before that purge its holder is warned by email (#256).
pub const DEACTIVATION_NOTICE_DAYS: i64 = 30;

/// The four timestamps of a `users` row that decide whether, and when, the
/// purge takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PurgeClock {
    pub deletion_requested_at: Option<DateTime<Utc>>,
    pub deactivated_at: Option<DateTime<Utc>>,
    pub deactivation_notice_sent_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    /// A reactivation request from the holder awaits the superadmin's
    /// decision (`account_reactivation_requests`, #289).
    pub reactivation_requested: bool,
    /// When the superadmin first refused a reactivation request since the
    /// account was deactivated (`users.reactivation_refused_at`, #289): a
    /// later request no longer suspends anything, and the purge waits 30
    /// days after it (#296) — after the first refusal only (#313).
    pub reactivation_refused_at: Option<DateTime<Utc>>,
}

/// When an account deactivated at `deactivated_at` becomes purgeable: 2
/// years after its deactivation, and never less than 30 days after its
/// holder was warned — a notice that went out late (the service was down,
/// or the mail relay refused it) pushes the purge back rather than warn of
/// a purge already due. A notice never sent caps the wait at the 2 years
/// plus those 30 days, so an address that bounces for good does not keep
/// the account forever.
///
/// Nor is it ever less than 30 days after the superadmin first refused a
/// reactivation request (`refused_at`, #296; a later refusal does not
/// move it, [`record_refusal`], #313). The refusal clears the
/// warning; once that cap has passed, the purge would otherwise be due at
/// once — the new warning failing, or the refusal landing between a pass's
/// warnings and its purge, and the holder never warned since.
pub fn deactivation_purge_at(
    deactivated_at: DateTime<Utc>,
    notice_sent_at: Option<DateTime<Utc>>,
    refused_at: Option<DateTime<Utc>>,
) -> DateTime<Utc> {
    let retention_end = deactivation_retention_end(deactivated_at);
    let notice = Duration::days(DEACTIVATION_NOTICE_DAYS);
    let after_notice = match notice_sent_at {
        Some(sent_at) => retention_end.max(sent_at + notice),
        None => retention_end + notice,
    };
    match refused_at {
        Some(refused_at) => after_notice.max(refused_at + notice),
        None => after_notice,
    }
}

/// The two `users` stamps a refusal of a reactivation request may rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefusalStamps {
    pub reactivation_refused_at: Option<DateTime<Utc>>,
    pub deactivation_notice_sent_at: Option<DateTime<Utc>>,
}

/// What a refusal at `now` leaves of `before`. Only the first refusal since
/// the deactivation counts: it is dated, and the warning is cleared so that
/// the holder is warned afresh — which postpones the purge once, to 30 days
/// after that refusal or that new warning at least
/// ([`deactivation_purge_at`]). A later refusal rewrites nothing, so it
/// moves the purge date no further (controller's decision of 2026-10-02,
/// #313).
pub fn record_refusal(before: RefusalStamps, now: DateTime<Utc>) -> RefusalStamps {
    match before.reactivation_refused_at {
        Some(_) => before,
        None => RefusalStamps {
            reactivation_refused_at: Some(now),
            deactivation_notice_sent_at: None,
        },
    }
}

/// `deactivated_at` plus the 2 years, in calendar months: a deactivation on
/// 29 February ends on 28 February.
fn deactivation_retention_end(deactivated_at: DateTime<Utc>) -> DateTime<Utc> {
    deactivated_at
        .checked_add_months(Months::new(DEACTIVATION_RETENTION_MONTHS))
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

/// Whether the purge takes this account now: 30 days after a deletion
/// request, whether or not support deactivated the account since (#139),
/// or once [`deactivation_purge_at`] has passed — unless the holder's
/// first reactivation request since the deactivation awaits the
/// superadmin's decision, which suspends that second date, and only it
/// (#289). A purged row never is.
pub fn purge_due(now: DateTime<Utc>, clock: &PurgeClock) -> bool {
    if clock.deleted_at.is_some() {
        return false;
    }
    let requested = clock
        .deletion_requested_at
        .is_some_and(|at| now > at + Duration::days(PURGE_GRACE_DAYS));
    let deactivated = !suspended(clock)
        && clock.deactivated_at.is_some_and(|at| {
            now >= deactivation_purge_at(
                at,
                clock.deactivation_notice_sent_at,
                clock.reactivation_refused_at,
            )
        });
    requested || deactivated
}

/// Whether a pending reactivation request holds back the purge 2 years
/// after the deactivation and its warning: only the first one since the
/// deactivation — after a refusal the holder may ask again, but the
/// deadline runs (arbitrage of 2026-09-29, #289).
fn suspended(clock: &PurgeClock) -> bool {
    clock.reactivation_requested && clock.reactivation_refused_at.is_none()
}

/// Whether the holder of a deactivated account is due its warning: from 30
/// days before the 2 years are up, once. An account whose deletion was
/// requested is purged 30 days after its request, long before, and is
/// warned of nothing; nor is one whose purge a pending reactivation
/// request suspends (#289).
pub fn deactivation_notice_due(now: DateTime<Utc>, clock: &PurgeClock) -> bool {
    if clock.deleted_at.is_some()
        || clock.deletion_requested_at.is_some()
        || clock.deactivation_notice_sent_at.is_some()
        || suspended(clock)
    {
        return false;
    }
    clock.deactivated_at.is_some_and(|at| {
        now >= deactivation_retention_end(at) - Duration::days(DEACTIVATION_NOTICE_DAYS)
    })
}

/// A remaining member of a group whose owner is being purged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemainingMember {
    pub user_id: Uuid,
    pub is_admin: bool,
    pub joined_at: DateTime<Utc>,
    /// Deactivated by support (`deactivated_at`): never inherits.
    pub deactivated: bool,
    /// Awaiting its own purge (`deletion_requested_at`). It can still
    /// cancel the request, so it inherits — but only after every active
    /// member.
    pub pending_deletion: bool,
}

/// The member who inherits a purged owner's group, in this order: active
/// admins, active members, admins awaiting deletion, members awaiting
/// deletion — the longest-standing first within each, equal `joined_at`
/// falling back to `user_id` so the choice never depends on the order the
/// rows come back in. An account support deactivated never inherits;
/// `None` when nobody else is left.
pub fn successor(remaining: &[RemainingMember]) -> Option<Uuid> {
    remaining
        .iter()
        .filter(|m| !m.deactivated)
        .min_by_key(|m| (m.pending_deletion, !m.is_admin, m.joined_at, m.user_id))
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
            deactivated: false,
            pending_deletion: false,
        }
    }

    fn ineligible(id: u128, is_admin: bool, day: u32) -> RemainingMember {
        RemainingMember {
            deactivated: true,
            ..member(id, is_admin, day)
        }
    }

    fn pending(id: u128, is_admin: bool, day: u32) -> RemainingMember {
        RemainingMember {
            pending_deletion: true,
            ..member(id, is_admin, day)
        }
    }

    fn at(y: i32, mo: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, 12, 0, 0).unwrap()
    }

    fn clock() -> PurgeClock {
        PurgeClock {
            deletion_requested_at: None,
            deactivated_at: None,
            deactivation_notice_sent_at: None,
            deleted_at: None,
            reactivation_requested: false,
            reactivation_refused_at: None,
        }
    }

    fn deactivated(on: DateTime<Utc>, notice: Option<DateTime<Utc>>) -> PurgeClock {
        PurgeClock {
            deactivated_at: Some(on),
            deactivation_notice_sent_at: notice,
            ..clock()
        }
    }

    // -- deactivation_purge_at ------------------------------------------------

    #[test]
    fn a_deactivated_account_warned_on_time_is_purgeable_two_calendar_years_after() {
        assert_eq!(
            deactivation_purge_at(at(2026, 3, 10), Some(at(2028, 2, 9)), None),
            at(2028, 3, 10)
        );
    }

    #[test]
    fn two_years_after_the_29th_of_february_is_the_28th() {
        assert_eq!(
            deactivation_purge_at(at(2028, 2, 29), Some(at(2030, 1, 1)), None),
            at(2030, 2, 28)
        );
    }

    #[test]
    fn a_late_notice_pushes_the_purge_to_30_days_after_it() {
        assert_eq!(
            deactivation_purge_at(at(2026, 3, 10), Some(at(2028, 3, 1)), None),
            at(2028, 3, 31)
        );
    }

    #[test]
    fn a_notice_never_sent_caps_the_wait_at_two_years_and_30_days() {
        assert_eq!(
            deactivation_purge_at(at(2026, 3, 10), None, None),
            at(2028, 4, 9)
        );
    }

    // -- purge_due ------------------------------------------------------------

    #[test]
    fn an_active_account_is_never_due() {
        assert!(!purge_due(at(2040, 1, 1), &clock()));
    }

    #[test]
    fn a_deletion_request_is_due_once_its_30_days_have_passed() {
        let c = PurgeClock {
            deletion_requested_at: Some(at(2026, 9, 1)),
            ..clock()
        };
        assert!(!purge_due(at(2026, 10, 1), &c));
        assert!(purge_due(at(2026, 10, 2), &c));
    }

    #[test]
    fn a_deletion_request_followed_by_a_deactivation_is_due_at_30_days() {
        // #139: `deleted_at IS NULL` used to exclude it for good.
        let c = PurgeClock {
            deletion_requested_at: Some(at(2026, 9, 1)),
            deactivated_at: Some(at(2026, 9, 5)),
            ..clock()
        };
        assert!(purge_due(at(2026, 10, 2), &c));
    }

    #[test]
    fn a_deactivated_account_is_due_at_its_purge_date_and_not_before() {
        let c = deactivated(at(2026, 3, 10), Some(at(2028, 2, 9)));
        assert!(!purge_due(at(2028, 3, 9), &c));
        assert!(purge_due(at(2028, 3, 10), &c));
    }

    #[test]
    fn a_deactivated_account_never_warned_waits_the_extra_30_days() {
        let c = deactivated(at(2026, 3, 10), None);
        assert!(!purge_due(at(2028, 3, 10), &c));
        assert!(purge_due(at(2028, 4, 9), &c));
    }

    #[test]
    fn a_purged_account_is_never_due_again() {
        let c = PurgeClock {
            deletion_requested_at: Some(at(2026, 1, 1)),
            deactivated_at: Some(at(2020, 1, 1)),
            deleted_at: Some(at(2026, 2, 1)),
            ..clock()
        };
        assert!(!purge_due(at(2040, 1, 1), &c));
    }

    // -- deactivation_notice_due ----------------------------------------------

    #[test]
    fn the_notice_is_due_30_days_before_the_two_years_and_not_before() {
        let c = deactivated(at(2026, 3, 10), None);
        assert!(!deactivation_notice_due(at(2028, 2, 8), &c));
        assert!(deactivation_notice_due(at(2028, 2, 9), &c));
    }

    #[test]
    fn a_late_notice_is_still_due() {
        let c = deactivated(at(2026, 3, 10), None);
        assert!(deactivation_notice_due(at(2028, 3, 20), &c));
    }

    #[test]
    fn the_notice_is_sent_once() {
        let c = deactivated(at(2026, 3, 10), Some(at(2028, 2, 9)));
        assert!(!deactivation_notice_due(at(2028, 2, 20), &c));
    }

    #[test]
    fn an_account_not_deactivated_gets_no_notice() {
        assert!(!deactivation_notice_due(at(2040, 1, 1), &clock()));
    }

    #[test]
    fn an_account_awaiting_its_deletion_gets_no_notice() {
        let c = PurgeClock {
            deletion_requested_at: Some(at(2026, 3, 1)),
            ..deactivated(at(2026, 3, 10), None)
        };
        assert!(!deactivation_notice_due(at(2028, 2, 20), &c));
    }

    #[test]
    fn a_purged_account_gets_no_notice() {
        let c = PurgeClock {
            deleted_at: Some(at(2027, 1, 1)),
            ..deactivated(at(2026, 3, 10), None)
        };
        assert!(!deactivation_notice_due(at(2028, 2, 20), &c));
    }

    // -- a pending reactivation request (#289) --------------------------------

    fn requested(c: PurgeClock) -> PurgeClock {
        PurgeClock {
            reactivation_requested: true,
            ..c
        }
    }

    #[test]
    fn a_pending_reactivation_request_suspends_the_two_year_purge() {
        let c = requested(deactivated(at(2026, 3, 10), Some(at(2028, 2, 9))));
        assert!(!purge_due(at(2028, 3, 10), &c));
        assert!(!purge_due(at(2040, 1, 1), &c));
    }

    /// Once the request is decided (its row deleted), the date #256 set
    /// applies again, unchanged.
    #[test]
    fn the_two_year_purge_resumes_once_the_request_is_decided() {
        let c = deactivated(at(2026, 3, 10), Some(at(2028, 2, 9)));
        assert!(!purge_due(at(2028, 3, 10), &requested(c)));
        assert!(purge_due(at(2028, 3, 10), &c));
    }

    /// Only the purge #256 set is suspended: a deletion the holder asked
    /// for still goes through at 30 days (#139).
    #[test]
    fn a_pending_reactivation_request_does_not_hold_back_a_requested_deletion() {
        let c = requested(PurgeClock {
            deletion_requested_at: Some(at(2026, 9, 1)),
            ..deactivated(at(2026, 9, 5), None)
        });
        assert!(purge_due(at(2026, 10, 2), &c));
    }

    fn refused(on: DateTime<Utc>, c: PurgeClock) -> PurgeClock {
        PurgeClock {
            reactivation_refused_at: Some(on),
            ..c
        }
    }

    /// Arbitrage of 2026-09-29: after a refusal the holder may ask again,
    /// but the deadline runs — an overdue account is purged.
    #[test]
    fn a_new_request_after_a_refusal_does_not_suspend_the_purge() {
        let c = requested(refused(
            at(2027, 1, 1),
            deactivated(at(2026, 3, 10), Some(at(2028, 2, 9))),
        ));
        assert!(!purge_due(at(2028, 3, 9), &c));
        assert!(purge_due(at(2028, 3, 10), &c));
    }

    #[test]
    fn a_new_request_after_a_refusal_does_not_hold_back_the_notice() {
        let c = requested(refused(at(2027, 1, 1), deactivated(at(2026, 3, 10), None)));
        assert!(deactivation_notice_due(at(2028, 2, 9), &c));
    }

    /// A refusal on its own changes nothing to the dates #256 set.
    #[test]
    fn a_refusal_without_a_new_request_leaves_the_two_year_purge_as_is() {
        let c = refused(
            at(2027, 1, 1),
            deactivated(at(2026, 3, 10), Some(at(2028, 2, 9))),
        );
        assert!(!purge_due(at(2028, 3, 9), &c));
        assert!(purge_due(at(2028, 3, 10), &c));
    }

    /// #296: a refusal clears the warning. Once the 2 years are up, the
    /// purge would otherwise be due at once when the new warning fails to
    /// go out, or when the refusal lands between a pass's warnings and its
    /// purge — the holder never warned.
    #[test]
    fn a_late_refusal_puts_the_purge_30_days_after_it() {
        assert_eq!(
            deactivation_purge_at(at(2026, 3, 10), None, Some(at(2028, 6, 1))),
            at(2028, 7, 1)
        );
    }

    #[test]
    fn a_refusal_long_before_the_warning_leaves_the_purge_date_as_is() {
        assert_eq!(
            deactivation_purge_at(at(2026, 3, 10), Some(at(2028, 2, 9)), Some(at(2027, 1, 1))),
            at(2028, 3, 10)
        );
    }

    #[test]
    fn a_late_refusal_whose_new_warning_never_went_is_not_purged_for_30_days() {
        let c = refused(at(2028, 6, 1), deactivated(at(2026, 3, 10), None));
        assert!(!purge_due(at(2028, 6, 1), &c));
        assert!(!purge_due(at(2028, 6, 30), &c));
        assert!(purge_due(at(2028, 7, 1), &c));
    }

    // -- record_refusal (#313) --------------------------------------------------

    fn stamps(refused: Option<DateTime<Utc>>, notice: Option<DateTime<Utc>>) -> RefusalStamps {
        RefusalStamps {
            reactivation_refused_at: refused,
            deactivation_notice_sent_at: notice,
        }
    }

    /// The purge date of an account deactivated on 10 March 2026 with
    /// these stamps.
    fn purge_on(s: RefusalStamps) -> DateTime<Utc> {
        deactivation_purge_at(
            at(2026, 3, 10),
            s.deactivation_notice_sent_at,
            s.reactivation_refused_at,
        )
    }

    /// The first refusal opens the one 30-day postponement: it is dated,
    /// and the warning is cleared so that the holder is warned afresh.
    #[test]
    fn a_first_refusal_is_dated_and_clears_the_warning() {
        assert_eq!(
            record_refusal(stamps(None, Some(at(2028, 2, 9))), at(2028, 6, 1)),
            stamps(Some(at(2028, 6, 1)), None)
        );
    }

    /// A later refusal keeps the first one's date and the warning sent
    /// since: there is one postponement, not one per refusal.
    #[test]
    fn a_later_refusal_rewrites_nothing() {
        let after_first = stamps(Some(at(2028, 6, 1)), Some(at(2028, 6, 2)));
        assert_eq!(record_refusal(after_first, at(2028, 6, 20)), after_first);
        let unwarned = stamps(Some(at(2028, 6, 1)), None);
        assert_eq!(record_refusal(unwarned, at(2028, 6, 20)), unwarned);
    }

    #[test]
    fn a_second_refusal_before_the_new_warning_does_not_move_the_purge() {
        let first = record_refusal(stamps(None, Some(at(2028, 2, 9))), at(2028, 6, 1));
        assert_eq!(purge_on(first), at(2028, 7, 1));
        let second = record_refusal(first, at(2028, 6, 25));
        assert_eq!(purge_on(second), at(2028, 7, 1));
    }

    #[test]
    fn a_second_refusal_after_the_new_warning_does_not_move_the_purge() {
        let first = record_refusal(stamps(None, Some(at(2028, 2, 9))), at(2028, 2, 20));
        let warned = RefusalStamps {
            deactivation_notice_sent_at: Some(at(2028, 2, 21)),
            ..first
        };
        assert_eq!(purge_on(warned), at(2028, 3, 22));
        let second = record_refusal(warned, at(2028, 3, 15));
        assert_eq!(purge_on(second), at(2028, 3, 22));
    }

    /// The floor #296 set still holds after any number of refusals: never
    /// less than 30 days after the first one.
    #[test]
    fn the_purge_stays_30_days_after_the_first_refusal_at_least() {
        let mut s = record_refusal(stamps(None, None), at(2028, 6, 1));
        for day in [5, 10, 29] {
            s = record_refusal(s, at(2028, 6, day));
            assert_eq!(purge_on(s), at(2028, 7, 1));
        }
    }

    #[test]
    fn no_notice_is_sent_while_a_reactivation_request_is_pending() {
        let c = requested(deactivated(at(2026, 3, 10), None));
        assert!(!deactivation_notice_due(at(2028, 2, 9), &c));
        assert!(deactivation_notice_due(
            at(2028, 2, 9),
            &deactivated(at(2026, 3, 10), None)
        ));
    }

    /// Order: active admins, active members, then admins awaiting
    /// deletion, then members awaiting deletion.
    #[test]
    fn an_active_member_inherits_ahead_of_an_older_admin_awaiting_deletion() {
        let remaining = [pending(1, true, 1), member(2, false, 5)];
        assert_eq!(successor(&remaining), Some(Uuid::from_u128(2)));
    }

    #[test]
    fn with_only_accounts_awaiting_deletion_the_admin_inherits_first() {
        let remaining = [
            pending(1, false, 1),
            pending(2, true, 5),
            ineligible(3, true, 1),
        ];
        assert_eq!(successor(&remaining), Some(Uuid::from_u128(2)));
    }

    #[test]
    fn with_only_members_awaiting_deletion_the_longest_standing_inherits() {
        let remaining = [pending(1, false, 4), pending(2, false, 2)];
        assert_eq!(successor(&remaining), Some(Uuid::from_u128(2)));
    }

    #[test]
    fn an_ineligible_admin_is_passed_over_for_the_next_admin() {
        let remaining = [
            ineligible(1, true, 1),
            member(2, true, 4),
            member(3, false, 2),
        ];
        assert_eq!(successor(&remaining), Some(Uuid::from_u128(2)));
    }

    #[test]
    fn with_every_admin_ineligible_the_longest_standing_eligible_member_inherits() {
        let remaining = [
            ineligible(1, true, 1),
            ineligible(2, false, 2),
            member(3, false, 5),
            member(4, false, 3),
        ];
        assert_eq!(successor(&remaining), Some(Uuid::from_u128(4)));
    }

    #[test]
    fn a_group_left_with_only_ineligible_members_has_no_successor() {
        let remaining = [ineligible(1, true, 1), ineligible(2, false, 2)];
        assert_eq!(successor(&remaining), None);
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
