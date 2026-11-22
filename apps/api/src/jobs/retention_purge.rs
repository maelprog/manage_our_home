//! Scheduled retention purge (#138).
//!
//! A retention period that no code applies is a promise nobody keeps:
//! `audit_log`, `email_verification_tokens`, `password_reset_tokens`,
//! `invitations` and `sessions` all grew without bound, each holding
//! personal data long after it had any use (`invitations` even holds the
//! address of a third party who has no account). So did `users` itself for
//! an address never confirmed (#834): such an account cannot log in, and
//! the token purge left its row in place for good.
//!
//! The durations below are the controller's decision of 2026-09-19 unless
//! their own comment dates a later one, not a technical default, and they
//! are the ones published in
//! `docs/privacy-policy.md` and recorded in `docs/registre-traitements.md`.
//!
//! Since #908 it also closes, then deletes six months later, the reports on
//! publications of the shared recipe library.
//!
//! The pass also deletes the login lock's counters past their own bound
//! (`auth::throttle::purge_cutoff`, half an hour, #427): not a retention
//! period of its own, the instant past which no counter can still count.

use std::time::Duration as StdDuration;

use anyhow::Context;
use chrono::{DateTime, Duration, Months, Utc};
use sqlx::PgPool;
use tokio::time::{interval, MissedTickBehavior};

use crate::attachment_reconcile::ensure_bypasses_rls;
use crate::auth::session::SESSION_IDLE_TIMEOUT_DAYS;
use crate::jobs::single_runner::{exclusively, Job};

/// Rolling six months, the CNIL's recommendation for access logs
/// (délib. 2021-122). The décret n° 2021-1362 minimum of one year was
/// weighed and set aside: it binds hosting providers and public
/// communication services, not a family app run by one individual — to be
/// revisited if the service changes nature.
pub const AUDIT_LOG_RETENTION_MONTHS: u32 = 6;
/// Twice the token's own validity: for a day after it expires, a click on
/// the link still answers "expired" (410) rather than "unknown" (404).
pub const EMAIL_VERIFICATION_RETENTION_HOURS: i64 = 48;
/// An email/password account whose address was never confirmed cannot
/// log in (`auth::login`): past a week, its address, name and hash are
/// personal data with no purpose left (controller's decision of
/// 2026-11-13, #834). Counted from the account's creation, not from the
/// last link mailed.
pub const UNVERIFIED_ACCOUNT_RETENTION_DAYS: i64 = 7;
/// A reset token is deleted the moment it is used, so this only catches
/// the ones nobody clicked. It matches the token's validity exactly.
pub const PASSWORD_RESET_RETENTION_HOURS: i64 = 1;
/// An invitation is deleted when it is accepted, so this only catches the
/// ones nobody accepted — with the invited address they carry.
pub const INVITATION_RETENTION_DAYS: i64 = 30;
/// The notification centre's entries (#534), read or not: long enough to
/// find a month's reminders and messages again, short enough that the
/// list holds no history of its own (controller's decision of 2026-10-21).
pub const NOTIFICATION_RETENTION_DAYS: i64 = 30;
/// A report on a publication of the shared library (#908), counted from
/// its closure — dismissed, the publication withdrawn, or gone on its own
/// (`lapsed`); an open report is kept until it is closed (user's decision
/// of 2026-11-20).
pub const LIBRARY_REPORT_RETENTION_MONTHS: u32 = 6;

/// The instant, per table, before which a row has outlived its retention.
///
/// Computed once per pass in Rust rather than as `now()` inside each
/// statement, so one pass applies one clock to every table — and so
/// the durations themselves are testable without a database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionCutoffs {
    /// `audit_log.occurred_at` older than this goes.
    pub audit_log: DateTime<Utc>,
    /// `email_verification_tokens.created_at` older than this goes.
    pub email_verification: DateTime<Utc>,
    /// `password_reset_tokens.created_at` older than this goes.
    pub password_reset: DateTime<Utc>,
    /// `invitations.created_at` older than this goes.
    pub invitation: DateTime<Utc>,
    /// `notifications.created_at` older than this goes.
    pub notification: DateTime<Utc>,
    /// `library_reports.closed_at` older than this goes.
    pub library_report: DateTime<Utc>,
    /// A session whose `last_seen_at` is older than this has been idle
    /// past `SESSION_IDLE_TIMEOUT_DAYS` and is already refused.
    pub session_idle: DateTime<Utc>,
    /// `users.created_at` of an unverified account older than this goes.
    pub unverified_account: DateTime<Utc>,
    /// The pass's clock, against which `expires_at`/`revoked_at` are read.
    pub now: DateTime<Utc>,
}

/// What the purge reads of an account to decide whether it is one nobody
/// ever confirmed (#834).
#[derive(Debug, Clone, Copy)]
pub struct AccountForRetention {
    pub created_at: DateTime<Utc>,
    pub email_verified: bool,
    /// A row in `oauth_identities`: the Google account that added a
    /// password is unverified again (`auth::set_password`) but in use.
    /// The only case: an unverified account a Google identity is bound to
    /// later comes out verified (`oauth_google::Binding::ClaimUnverified`,
    /// #956).
    pub google_identity: bool,
    /// A row in `group_members`.
    pub group_member: bool,
    /// An invitation `consumed_by` it.
    pub accepted_invitation: bool,
    /// Deactivated by the superadmin: `account_purge` owns it, two years
    /// with a notice, and the address stays refused to `register`.
    pub deactivated: bool,
    /// Already purged and anonymised by `account_purge`.
    pub deleted: bool,
}

impl AccountForRetention {
    /// An unverified account goes once it is strictly older than
    /// `cutoffs.unverified_account`, counted from its creation whatever
    /// links were mailed since — unless anything shows it was used or
    /// another job owns it.
    pub fn is_purgeable_unverified(&self, cutoffs: &RetentionCutoffs) -> bool {
        !self.email_verified
            && self.created_at < cutoffs.unverified_account
            && !self.google_identity
            && !self.group_member
            && !self.accepted_invitation
            && !self.deactivated
            && !self.deleted
    }
}

impl RetentionCutoffs {
    pub fn at(now: DateTime<Utc>) -> Self {
        Self {
            // `checked_sub_months` clamps to the end of a shorter month
            // (31 August → 28 or 29 February); it only fails past year
            // -262143, which `now` never is.
            audit_log: now
                .checked_sub_months(Months::new(AUDIT_LOG_RETENTION_MONTHS))
                .unwrap_or(DateTime::<Utc>::MIN_UTC),
            email_verification: now - Duration::hours(EMAIL_VERIFICATION_RETENTION_HOURS),
            password_reset: now - Duration::hours(PASSWORD_RESET_RETENTION_HOURS),
            invitation: now - Duration::days(INVITATION_RETENTION_DAYS),
            notification: now - Duration::days(NOTIFICATION_RETENTION_DAYS),
            library_report: now
                .checked_sub_months(Months::new(LIBRARY_REPORT_RETENTION_MONTHS))
                .unwrap_or(DateTime::<Utc>::MIN_UTC),
            session_idle: now - Duration::days(SESSION_IDLE_TIMEOUT_DAYS),
            unverified_account: now - Duration::days(UNVERIFIED_ACCOUNT_RETENTION_DAYS),
            now,
        }
    }
}

/// Once an hour. The reset token's retention is one hour, so the interval
/// is what bounds how long an unused one survives: under two hours.
pub const PURGE_INTERVAL: StdDuration = StdDuration::from_secs(3600);

/// Rows deleted by one pass, per table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PurgeCounts {
    pub audit_log: u64,
    pub email_verification_tokens: u64,
    /// Accounts whose address was never confirmed (#834); their tokens go
    /// with them and are not counted above.
    pub unverified_accounts: u64,
    pub password_reset_tokens: u64,
    pub invitations: u64,
    pub sessions: u64,
    /// The notification centre's entries (#534).
    pub notifications: u64,
    /// Login lock counters past `auth::throttle::purge_cutoff` (#427).
    pub login_throttle: u64,
    /// Sliding-window limits past their window (`rate_limit::LIMITS`, #428).
    pub rate_limit_log: u64,
    /// CSP report counters whose window is over (#428).
    pub csp_report_throttle: u64,
    /// Open reports on a publication gone otherwise than by moderation,
    /// closed as `lapsed` (#908).
    pub library_reports_lapsed: u64,
    /// Reports closed past [`LIBRARY_REPORT_RETENTION_MONTHS`] (#908).
    pub library_reports: u64,
}

/// Polling worker, `account_purge`'s shape. `pool` must be the `BYPASSRLS`
/// admin pool (`AppState.admin_db`): `invitations` is under a forced RLS
/// policy, and on any other role the pass refuses rather than delete
/// nothing from it and report success.
///
/// The first tick fires at startup, so a process restarted more often
/// than hourly still purges.
pub async fn run(pool: PgPool) {
    let mut ticker = interval(PURGE_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        match scheduled_pass(&pool, RetentionCutoffs::at(Utc::now())).await {
            Ok(Some(counts)) => tracing::info!(?counts, "retention purge pass done"),
            Ok(None) => tracing::debug!("retention purge pass skipped: another process runs it"),
            Err(e) => tracing::error!(error = ?e, "retention purge job failed"),
        }
    }
}

/// [`purge`], unless another process is running it (#426): `None` then,
/// and nothing is deleted by this one.
pub async fn scheduled_pass(
    pool: &PgPool,
    cutoffs: RetentionCutoffs,
) -> anyhow::Result<Option<PurgeCounts>> {
    exclusively(pool, Job::RetentionPurge, purge(pool, cutoffs)).await
}

/// One pass, in one transaction. `cutoffs` is a parameter so the flow
/// tests can move the clock instead of waiting months.
pub async fn purge(pool: &PgPool, cutoffs: RetentionCutoffs) -> anyhow::Result<PurgeCounts> {
    let mut tx = crate::db::begin(pool).await?;
    // The guard is shared with the attachment reconcile pass, whose own
    // message speaks of the bucket: say what is at stake here, so the
    // hourly ERROR line names the right hazard.
    ensure_bypasses_rls(&mut tx).await.context(
        "retention purge refusing to run: `invitations` is FORCE ROW LEVEL SECURITY, so on a \
         role that does not bypass it the DELETE matches no row and the pass would report \
         having erased addresses it left in place. Point ADMIN_DATABASE_URL at the BYPASSRLS \
         admin_role (see apps/api/README.md).",
    )?;

    let audit_log = sqlx::query!(
        "DELETE FROM audit_log WHERE occurred_at < $1",
        cutoffs.audit_log
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    let email_verification_tokens = sqlx::query!(
        "DELETE FROM email_verification_tokens WHERE created_at < $1",
        cutoffs.email_verification
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    // #834: read first, decided by `AccountForRetention`, then deleted by
    // id. `FOR UPDATE` holds the rows, so an account cannot be verified,
    // linked to Google or join a group between the decision and the
    // delete. The `WHERE` only narrows the read; the rule is the Rust one.
    let candidates = sqlx::query!(
        r#"
        SELECT u.id, u.created_at, u.email_verified,
               EXISTS (SELECT 1 FROM oauth_identities o WHERE o.user_id = u.id)
                   AS "google_identity!",
               EXISTS (SELECT 1 FROM group_members m WHERE m.user_id = u.id)
                   AS "group_member!",
               EXISTS (SELECT 1 FROM invitations i WHERE i.consumed_by = u.id)
                   AS "accepted_invitation!",
               u.deactivated_at IS NOT NULL AS "deactivated!",
               u.deleted_at IS NOT NULL AS "deleted!"
        FROM users u
        WHERE NOT u.email_verified AND u.created_at < $1
        FOR UPDATE OF u
        "#,
        cutoffs.unverified_account
    )
    .fetch_all(&mut *tx)
    .await?;
    let unverified: Vec<uuid::Uuid> = candidates
        .into_iter()
        .filter(|u| {
            AccountForRetention {
                created_at: u.created_at,
                email_verified: u.email_verified,
                google_identity: u.google_identity,
                group_member: u.group_member,
                accepted_invitation: u.accepted_invitation,
                deactivated: u.deactivated,
                deleted: u.deleted,
            }
            .is_purgeable_unverified(&cutoffs)
        })
        .map(|u| u.id)
        .collect();
    let unverified_accounts = sqlx::query!("DELETE FROM users WHERE id = ANY($1)", &unverified)
        .execute(&mut *tx)
        .await?
        .rows_affected();

    let password_reset_tokens = sqlx::query!(
        "DELETE FROM password_reset_tokens WHERE created_at < $1",
        cutoffs.password_reset
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    let invitations = sqlx::query!(
        "DELETE FROM invitations WHERE created_at < $1",
        cutoffs.invitation
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    let notifications = sqlx::query!(
        "DELETE FROM notifications WHERE created_at < $1",
        cutoffs.notification
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    // A session goes as soon as `AuthUser` would refuse it for good: logged
    // out or revoked, past its absolute lifetime, idle past the timeout —
    // its end dated by least(expires_at, last_seen_at + idle timeout), the
    // same stored `last_seen_at` the extractor reads (a refused session is
    // never refreshed again, so that end never moves) — or refused for good
    // by the account's state (`session::session_access`): any session of a
    // purged account, a full session of a deactivated one (a Google
    // callback could still open one before #194 was fixed), and a
    // restricted session (#289) of an account no longer deactivated. The
    // restricted session of a deactivated account is the one kept.
    let sessions = sqlx::query!(
        r#"
        DELETE FROM sessions s
        WHERE s.revoked_at IS NOT NULL
           OR s.expires_at < $1
           OR s.last_seen_at < $2
           OR EXISTS (
               SELECT 1 FROM users u
               WHERE u.id = s.user_id
                 AND (u.deleted_at IS NOT NULL
                      OR (u.deactivated_at IS NOT NULL) <> s.restricted)
           )
        "#,
        cutoffs.now,
        cutoffs.session_idle
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    // The login lock's counters (#427): an admission that inserts a pair
    // purges them already, but with no new pair nothing else would. Read
    // against this pass's clock rather than the database's; the bound is
    // half an hour wide, so the drift between the two does not matter.
    let login_throttle = crate::auth::throttle::purge_expired(&mut tx, cutoffs.now.into()).await?;
    // The other limits' counters (#428), for the same reason: otherwise
    // only a later admission of their own limit reclaims them.
    let rate_limit_log = crate::rate_limit::purge_expired(&mut tx, cutoffs.now.into()).await?;
    let csp_report_throttle =
        crate::csp_report_throttle::purge_expired(&mut tx, cutoffs.now.into()).await?;

    // #908: a report whose publication is gone — withdrawn by its
    // publisher, by the family's owner, at the publisher's departure or
    // account purge — has nothing left to decide on. It is closed then, as
    // `lapsed`, and its six months start. One the superadmin withdrew was
    // closed by the withdrawal itself.
    let library_reports_lapsed = sqlx::query!(
        r#"UPDATE library_reports r SET resolution = 'lapsed', closed_at = $1
           WHERE r.resolution IS NULL
             AND NOT EXISTS (SELECT 1 FROM library_recipes l WHERE l.id = r.library_recipe_id)"#,
        cutoffs.now
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    // An open report is never deleted, however old.
    let library_reports = sqlx::query!(
        "DELETE FROM library_reports WHERE closed_at < $1",
        cutoffs.library_report
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    tx.commit().await?;

    Ok(PurgeCounts {
        audit_log,
        email_verification_tokens,
        unverified_accounts,
        password_reset_tokens,
        invitations,
        sessions,
        notifications,
        login_throttle,
        rate_limit_log,
        csp_report_throttle,
        library_reports_lapsed,
        library_reports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{EMAIL_VERIFICATION_TTL_HOURS, PASSWORD_RESET_TTL_HOURS};
    use crate::groups::INVITATION_TTL_DAYS;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-22T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn audit_log_keeps_six_rolling_months() {
        let cutoffs = RetentionCutoffs::at(now());
        assert_eq!(
            cutoffs.audit_log,
            DateTime::parse_from_rfc3339("2026-03-22T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    /// The durations decided on 2026-09-19, as published in the privacy
    /// policy: changing one here without the documents is caught here.
    #[test]
    fn verification_tokens_keep_48_hours_and_invitations_30_days() {
        let cutoffs = RetentionCutoffs::at(now());
        assert_eq!(cutoffs.email_verification, now() - Duration::hours(48));
        assert_eq!(cutoffs.invitation, now() - Duration::days(30));
        assert_eq!(cutoffs.password_reset, now() - Duration::hours(1));
    }

    /// The duration decided on 2026-10-21 (#534), as published.
    #[test]
    fn notifications_keep_30_days() {
        assert_eq!(
            RetentionCutoffs::at(now()).notification,
            now() - Duration::days(30)
        );
    }

    /// The duration decided on 2026-11-20 (#908): six calendar months
    /// after the closure, clamped as the audit log's.
    #[test]
    fn closed_library_reports_keep_six_months() {
        assert_eq!(
            RetentionCutoffs::at(now()).library_report,
            DateTime::parse_from_rfc3339("2026-03-22T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
        let end_of_august = DateTime::parse_from_rfc3339("2026-08-31T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            RetentionCutoffs::at(end_of_august).library_report,
            DateTime::parse_from_rfc3339("2026-02-28T08:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    /// One clock for the whole pass.
    #[test]
    fn the_pass_reads_expiries_against_its_own_clock() {
        assert_eq!(RetentionCutoffs::at(now()).now, now());
    }

    /// The purge must never take a token the user can still click. Written
    /// against the TTL constant, not a literal, so shortening the
    /// retention below the validity fails here rather than in production.
    #[test]
    fn a_verification_token_is_never_purged_while_still_valid() {
        let cutoffs = RetentionCutoffs::at(now());
        assert!(
            cutoffs.email_verification + Duration::hours(EMAIL_VERIFICATION_TTL_HOURS)
                <= cutoffs.now,
            "a token created at the cutoff would still be valid"
        );
    }

    #[test]
    fn an_invitation_is_never_purged_while_still_valid() {
        let cutoffs = RetentionCutoffs::at(now());
        assert!(
            cutoffs.invitation + Duration::days(INVITATION_TTL_DAYS) <= cutoffs.now,
            "an invitation created at the cutoff would still be accepted"
        );
    }

    /// The reset token is the one case where retention and validity are
    /// the same hour: it is deleted at use, and an unused one is worth
    /// nothing past its expiry.
    #[test]
    fn a_reset_token_is_purged_as_soon_as_it_expires() {
        let cutoffs = RetentionCutoffs::at(now());
        assert_eq!(
            cutoffs.password_reset,
            cutoffs.now - Duration::hours(PASSWORD_RESET_TTL_HOURS)
        );
    }

    /// An account as `register` leaves it: unverified, password only, in
    /// no group, live.
    fn unverified(created_at: DateTime<Utc>) -> AccountForRetention {
        AccountForRetention {
            created_at,
            email_verified: false,
            google_identity: false,
            group_member: false,
            accepted_invitation: false,
            deactivated: false,
            deleted: false,
        }
    }

    /// #834: the duration as published.
    #[test]
    fn unverified_accounts_keep_7_days() {
        assert_eq!(
            RetentionCutoffs::at(now()).unverified_account,
            now() - Duration::days(7)
        );
    }

    #[test]
    fn an_unverified_account_six_days_old_is_kept() {
        let cutoffs = RetentionCutoffs::at(now());
        assert!(!unverified(now() - Duration::days(6)).is_purgeable_unverified(&cutoffs));
    }

    #[test]
    fn an_unverified_account_past_seven_days_is_purged() {
        let cutoffs = RetentionCutoffs::at(now());
        let just_past = now() - Duration::days(7) - Duration::minutes(1);
        assert!(unverified(just_past).is_purgeable_unverified(&cutoffs));
    }

    /// Strictly older than the cutoff, as every other table of the pass.
    #[test]
    fn an_unverified_account_exactly_at_the_cutoff_is_kept() {
        let cutoffs = RetentionCutoffs::at(now());
        assert!(!unverified(cutoffs.unverified_account).is_purgeable_unverified(&cutoffs));
    }

    #[test]
    fn a_verified_account_is_never_purged_here() {
        let cutoffs = RetentionCutoffs::at(now());
        let account = AccountForRetention {
            email_verified: true,
            ..unverified(now() - Duration::days(3650))
        };
        assert!(!account.is_purgeable_unverified(&cutoffs));
    }

    /// Each of these marks an account that has been used, or that another
    /// job owns: none goes, however old and unverified. A Google account
    /// that added a password is unverified again (`set_password`), the
    /// deactivated one has its own two-year purge with notice
    /// (`account_purge`), and a purged one is already anonymised.
    #[test]
    fn a_used_or_otherwise_owned_account_is_never_purged_here() {
        let cutoffs = RetentionCutoffs::at(now());
        let old = unverified(now() - Duration::days(3650));
        for (what, account) in [
            (
                "google",
                AccountForRetention {
                    google_identity: true,
                    ..old
                },
            ),
            (
                "member",
                AccountForRetention {
                    group_member: true,
                    ..old
                },
            ),
            (
                "invitation",
                AccountForRetention {
                    accepted_invitation: true,
                    ..old
                },
            ),
            (
                "deactivated",
                AccountForRetention {
                    deactivated: true,
                    ..old
                },
            ),
            (
                "deleted",
                AccountForRetention {
                    deleted: true,
                    ..old
                },
            ),
        ] {
            assert!(!account.is_purgeable_unverified(&cutoffs), "{what}");
        }
    }

    /// The idle cutoff dates the end of a session by
    /// `last_seen_at + SESSION_IDLE_TIMEOUT_DAYS`, the same arithmetic
    /// `session::is_idle` refuses it on.
    #[test]
    fn a_session_is_purged_when_it_has_been_idle_past_the_timeout() {
        let cutoffs = RetentionCutoffs::at(now());
        assert_eq!(
            cutoffs.session_idle,
            cutoffs.now - Duration::days(SESSION_IDLE_TIMEOUT_DAYS)
        );
    }
}
