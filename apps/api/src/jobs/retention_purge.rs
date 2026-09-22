//! Scheduled retention purge (#138).
//!
//! A retention period that no code applies is a promise nobody keeps:
//! `audit_log`, `email_verification_tokens`, `password_reset_tokens`,
//! `invitations` and `sessions` all grew without bound, each holding
//! personal data long after it had any use (`invitations` even holds the
//! address of a third party who has no account).
//!
//! The durations below are the controller's decision of 2026-09-19, not a
//! technical default, and they are the ones published in
//! `docs/privacy-policy.md` and recorded in `docs/registre-traitements.md`.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Months, Utc};
use sqlx::PgPool;
use tokio::time::{interval, MissedTickBehavior};

use crate::attachment_reconcile::ensure_bypasses_rls;
use crate::auth::session::SESSION_IDLE_TIMEOUT_DAYS;

/// Rolling six months, the CNIL's recommendation for access logs
/// (délib. 2021-122). The décret n° 2021-1362 minimum of one year was
/// weighed and set aside: it binds hosting providers and public
/// communication services, not a family app run by one individual — to be
/// revisited if the service changes nature.
pub const AUDIT_LOG_RETENTION_MONTHS: u32 = 6;
/// Twice the token's own validity: for a day after it expires, a click on
/// the link still answers "expired" (410) rather than "unknown" (404).
pub const EMAIL_VERIFICATION_RETENTION_HOURS: i64 = 48;
/// A reset token is deleted the moment it is used, so this only catches
/// the ones nobody clicked. It matches the token's validity exactly.
pub const PASSWORD_RESET_RETENTION_HOURS: i64 = 1;
/// An invitation is deleted when it is accepted, so this only catches the
/// ones nobody accepted — with the invited address they carry.
pub const INVITATION_RETENTION_DAYS: i64 = 30;

/// The instant, per table, before which a row has outlived its retention.
///
/// Computed once per pass in Rust rather than as `now()` inside each
/// statement, so one pass applies one clock to all five tables — and so
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
    /// A session whose `last_seen_at` is older than this has been idle
    /// past `SESSION_IDLE_TIMEOUT_DAYS` and is already refused.
    pub session_idle: DateTime<Utc>,
    /// The pass's clock, against which `expires_at`/`revoked_at` are read.
    pub now: DateTime<Utc>,
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
            session_idle: now - Duration::days(SESSION_IDLE_TIMEOUT_DAYS),
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
    pub password_reset_tokens: u64,
    pub invitations: u64,
    pub sessions: u64,
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
        match purge(&pool, RetentionCutoffs::at(Utc::now())).await {
            Ok(counts) => tracing::info!(?counts, "retention purge pass done"),
            Err(e) => tracing::error!(error = ?e, "retention purge job failed"),
        }
    }
}

/// One pass, in one transaction. `cutoffs` is a parameter so the flow
/// tests can move the clock instead of waiting months.
pub async fn purge(pool: &PgPool, cutoffs: RetentionCutoffs) -> anyhow::Result<PurgeCounts> {
    let mut tx = crate::db::begin(pool).await?;
    ensure_bypasses_rls(&mut tx).await?;

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

    // A session goes as soon as `AuthUser` would refuse it for good: logged
    // out or revoked, past its absolute lifetime, idle past the timeout —
    // its end dated by least(expires_at, last_seen_at + idle timeout), the
    // same stored `last_seen_at` the extractor reads (a refused session is
    // never refreshed again, so that end never moves) — or belonging to a
    // deactivated account, whose sessions a Google callback could still
    // open before #194 was fixed.
    let sessions = sqlx::query!(
        r#"
        DELETE FROM sessions s
        WHERE s.revoked_at IS NOT NULL
           OR s.expires_at < $1
           OR s.last_seen_at < $2
           OR EXISTS (
               SELECT 1 FROM users u
               WHERE u.id = s.user_id AND u.deleted_at IS NOT NULL
           )
        "#,
        cutoffs.now,
        cutoffs.session_idle
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    tx.commit().await?;

    Ok(PurgeCounts {
        audit_log,
        email_verification_tokens,
        password_reset_tokens,
        invitations,
        sessions,
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
