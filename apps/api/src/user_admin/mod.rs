pub mod admin;

use axum::async_trait;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use crate::auth::session::AuthUser;
use crate::error::AppError;
use crate::AppState;

/// Pure gate: a superadmin-only action is allowed iff the actor's
/// `users.is_superadmin` flag is set. Trivial on its own, but written
/// test-first per CLAUDE.md's TDD process, same shape as `can_modify` in
/// `messagerie`/`stocks` — the interesting property this locks down is
/// that it's a hard boolean gate with no partial/role-based nuance (unlike
/// group roles), since v1 has a single expected superadmin account.
pub(crate) fn is_superadmin(actor_is_superadmin: bool) -> bool {
    actor_is_superadmin
}

/// Oldest session the `/admin/*` routes accept (#226), counted from
/// `sessions.created_at` however much the session is used. A member route
/// still takes the same session up to `SESSION_TTL_DAYS`: the cap narrows
/// what a stolen superadmin cookie opens, not the account itself.
pub const SUPERADMIN_SESSION_MAX_AGE_HOURS: i64 = 12;
/// Inactivity the `/admin/*` routes tolerate (#226), shorter than the
/// 7 days of #195. Read off the same `last_seen_at`, which is only
/// rewritten once an hour: a superadmin session can be refused after a
/// little over one hour of actual inactivity, never during constant use.
/// Once past it, `load_session` no longer rewrites `last_seen_at`
/// (`refreshes_last_seen`), so no later request makes the session good for
/// the admin routes again.
pub const SUPERADMIN_IDLE_TIMEOUT_HOURS: i64 = 2;

/// Whether a session last used at `last_seen_at` has sat unused past
/// [`SUPERADMIN_IDLE_TIMEOUT_HOURS`]. The bound itself is still valid.
pub(crate) fn superadmin_session_is_idle(now: DateTime<Utc>, last_seen_at: DateTime<Utc>) -> bool {
    last_seen_at + Duration::hours(SUPERADMIN_IDLE_TIMEOUT_HOURS) < now
}

/// Whether a valid session is recent enough for a superadmin action
/// (#226): opened at most [`SUPERADMIN_SESSION_MAX_AGE_HOURS`] ago and
/// used within [`SUPERADMIN_IDLE_TIMEOUT_HOURS`]. Both bounds are still
/// valid, like `expires_at`'s; a date ahead of the api's clock refuses
/// nothing.
pub(crate) fn superadmin_session_is_fresh(
    now: DateTime<Utc>,
    created_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
) -> bool {
    created_at + Duration::hours(SUPERADMIN_SESSION_MAX_AGE_HOURS) >= now
        && !superadmin_session_is_idle(now, last_seen_at)
}

/// `AuthUser` plus `users.is_superadmin = true`, else 403. Only handlers
/// gated behind this extractor are allowed to touch `AppState.admin_db` (the
/// `BYPASSRLS` pool) — see `src/user_admin/admin.rs`.
///
/// The session check is `AuthUser`'s, run first: an invalid session is a 401
/// whatever the account's flag, and only a valid one gets as far as the 403.
/// It also means an admin request refreshes `sessions.last_seen_at` like any
/// other authenticated request (#196 — the copy of the check this extractor
/// used to carry skipped that refresh).
///
/// A superadmin's session must then be recent (#226,
/// [`superadmin_session_is_fresh`]), or it is a 401 too, the body the same
/// as any other: `apps/web` reads it as "log in again". The flag is checked
/// before the age, so a member's old session still gets the 403. The
/// session itself stays valid on every other route.
#[derive(Debug, Clone)]
pub struct SuperAdminUser {
    pub user_id: Uuid,
}

#[async_trait]
impl<S> FromRequestParts<S> for SuperAdminUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth = AuthUser::from_request_parts(parts, state).await?;
        if !is_superadmin(auth.is_superadmin) {
            return Err(AppError::Forbidden);
        }
        if !superadmin_session_is_fresh(
            Utc::now(),
            auth.session_created_at,
            auth.session_last_seen_at,
        ) {
            return Err(AppError::Unauthorized);
        }
        Ok(SuperAdminUser {
            user_id: auth.user_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration, Utc};

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    // -- superadmin_session_is_fresh (#226) --------------------------------

    #[test]
    fn a_session_just_opened_and_used_is_fresh() {
        let now = at("2026-10-03T12:00:00Z");
        assert!(superadmin_session_is_fresh(now, now, now));
    }

    #[test]
    fn a_session_opened_exactly_the_max_age_ago_is_still_fresh() {
        let now = at("2026-10-03T12:00:00Z");
        assert!(superadmin_session_is_fresh(
            now,
            at("2026-10-03T00:00:00Z"),
            now
        ));
    }

    /// However much it is used: the cap is on the session's age.
    #[test]
    fn a_session_opened_past_the_max_age_is_refused_even_in_use() {
        let now = at("2026-10-03T12:00:00Z");
        assert!(!superadmin_session_is_fresh(
            now,
            at("2026-10-02T23:59:59Z"),
            now
        ));
        assert!(!superadmin_session_is_fresh(
            now,
            now - Duration::days(29),
            now
        ));
    }

    #[test]
    fn a_session_idle_exactly_the_admin_timeout_is_still_fresh() {
        let now = at("2026-10-03T12:00:00Z");
        assert!(superadmin_session_is_fresh(
            now,
            at("2026-10-03T09:00:00Z"),
            at("2026-10-03T10:00:00Z")
        ));
    }

    /// Well within the 7 days of #195, and within the max age.
    #[test]
    fn a_session_idle_past_the_admin_timeout_is_refused() {
        let now = at("2026-10-03T12:00:00Z");
        assert!(!superadmin_session_is_fresh(
            now,
            at("2026-10-03T09:00:00Z"),
            at("2026-10-03T09:59:59Z")
        ));
    }

    /// Dates ahead of the api's clock (the database's `now()` set them)
    /// refuse nothing.
    #[test]
    fn dates_in_the_future_are_fresh() {
        let now = at("2026-10-03T12:00:00Z");
        let later = at("2026-10-03T12:05:00Z");
        assert!(superadmin_session_is_fresh(now, later, later));
    }

    /// The durations chosen for #226: shorter than a member session's on
    /// both counts, and the idle timeout longer than the interval at which
    /// `last_seen_at` is rewritten (60 minutes), or a superadmin in
    /// constant use could be refused.
    #[test]
    fn the_admin_caps_are_twelve_hours_and_two_hours_idle() {
        assert_eq!(SUPERADMIN_SESSION_MAX_AGE_HOURS, 12);
        assert_eq!(SUPERADMIN_IDLE_TIMEOUT_HOURS, 2);
        assert!(
            Duration::hours(SUPERADMIN_IDLE_TIMEOUT_HOURS)
                < Duration::days(crate::auth::session::SESSION_IDLE_TIMEOUT_DAYS)
        );
        assert!(
            Duration::hours(SUPERADMIN_SESSION_MAX_AGE_HOURS)
                < Duration::days(crate::auth::session::SESSION_TTL_DAYS)
        );
    }

    #[test]
    fn superadmin_flag_true_is_allowed() {
        assert!(is_superadmin(true));
    }

    #[test]
    fn superadmin_flag_false_is_denied() {
        assert!(!is_superadmin(false));
    }
}
