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
/// Once past it, the session's admin access is closed for good
/// ([`closed_admin_expiry`]), so no later request — member activity
/// included, which keeps rewriting `last_seen_at` — opens the admin routes
/// again.
pub const SUPERADMIN_IDLE_TIMEOUT_HOURS: i64 = 2;

/// Whether a session last used at `last_seen_at` has sat unused past
/// [`SUPERADMIN_IDLE_TIMEOUT_HOURS`]. The bound itself is still valid.
pub(crate) fn superadmin_session_is_idle(now: DateTime<Utc>, last_seen_at: DateTime<Utc>) -> bool {
    last_seen_at + Duration::hours(SUPERADMIN_IDLE_TIMEOUT_HOURS) < now
}

/// What separates the end of a session's admin access from the end of the
/// session itself: the absolute lifetime minus the max age.
fn admin_access_margin() -> Duration {
    Duration::days(crate::auth::session::SESSION_TTL_DAYS)
        - Duration::hours(SUPERADMIN_SESSION_MAX_AGE_HOURS)
}

/// The last moment the admin routes take a session (#339), read off its
/// `expires_at`, the one date of a session that is shown nowhere. As
/// `insert_session` writes it, that is the max age; [`closed_admin_expiry`]
/// moves it earlier. Holding the closure there, rather than in
/// `last_seen_at`, leaves `last_seen_at` to date the session's last
/// activity, member routes included — what the sessions page shows and
/// what the 7 days of #195 count from.
pub(crate) fn admin_access_end(expires_at: DateTime<Utc>) -> DateTime<Utc> {
    expires_at - admin_access_margin()
}

/// The `expires_at` that closes a superadmin session's admin access, when
/// a request finds it idle past [`SUPERADMIN_IDLE_TIMEOUT_HOURS`] while that
/// access is still open (#339). The access then ends where the timeout
/// fell, `last_seen_at` plus the timeout, already past. `None` when there is
/// nothing to close: the session is in use, already closed, or past its max
/// age.
///
/// The price: the absolute end of the session moves earlier by what was left
/// of the max age at that moment — at most the max age minus the timeout,
/// 10 hours, on a 30-day lifetime.
pub(crate) fn closed_admin_expiry(
    now: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if !superadmin_session_is_idle(now, last_seen_at) || admin_access_end(expires_at) < now {
        return None;
    }
    let closed =
        last_seen_at + Duration::hours(SUPERADMIN_IDLE_TIMEOUT_HOURS) + admin_access_margin();
    (closed < expires_at).then_some(closed)
}

/// Whether a valid session is recent enough for a superadmin action
/// (#226): opened at most [`SUPERADMIN_SESSION_MAX_AGE_HOURS`] ago, its
/// admin access not closed ([`admin_access_end`], #339), and used within
/// [`SUPERADMIN_IDLE_TIMEOUT_HOURS`]. The bounds are still valid, like
/// `expires_at`'s; a date ahead of the api's clock refuses nothing.
pub(crate) fn superadmin_session_is_fresh(
    now: DateTime<Utc>,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
) -> bool {
    created_at + Duration::hours(SUPERADMIN_SESSION_MAX_AGE_HOURS) >= now
        && admin_access_end(expires_at) >= now
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
            auth.session_expires_at,
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

    /// A session as `insert_session` writes it: `expires_at` the absolute
    /// lifetime after its opening, its admin access never closed.
    fn opened_fresh(
        now: DateTime<Utc>,
        created_at: DateTime<Utc>,
        last_seen_at: DateTime<Utc>,
    ) -> bool {
        superadmin_session_is_fresh(now, created_at, unclosed(created_at), last_seen_at)
    }

    fn unclosed(created_at: DateTime<Utc>) -> DateTime<Utc> {
        created_at + Duration::days(crate::auth::session::SESSION_TTL_DAYS)
    }

    #[test]
    fn a_session_just_opened_and_used_is_fresh() {
        let now = at("2026-10-03T12:00:00Z");
        assert!(opened_fresh(now, now, now));
    }

    #[test]
    fn a_session_opened_exactly_the_max_age_ago_is_still_fresh() {
        let now = at("2026-10-03T12:00:00Z");
        assert!(opened_fresh(now, at("2026-10-03T00:00:00Z"), now));
    }

    /// However much it is used: the cap is on the session's age.
    #[test]
    fn a_session_opened_past_the_max_age_is_refused_even_in_use() {
        let now = at("2026-10-03T12:00:00Z");
        assert!(!opened_fresh(now, at("2026-10-02T23:59:59Z"), now));
        assert!(!opened_fresh(now, now - Duration::days(29), now));
    }

    #[test]
    fn a_session_idle_exactly_the_admin_timeout_is_still_fresh() {
        let now = at("2026-10-03T12:00:00Z");
        assert!(opened_fresh(
            now,
            at("2026-10-03T09:00:00Z"),
            at("2026-10-03T10:00:00Z")
        ));
    }

    /// Well within the 7 days of #195, and within the max age.
    #[test]
    fn a_session_idle_past_the_admin_timeout_is_refused() {
        let now = at("2026-10-03T12:00:00Z");
        assert!(!opened_fresh(
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
        assert!(opened_fresh(now, later, later));
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

    // -- closed_admin_expiry (#339) -----------------------------------------

    /// Idle past the admin timeout within its first 12 hours: the admin
    /// access is closed at the moment the timeout was crossed, by moving
    /// `expires_at` so that its admin end falls there.
    #[test]
    fn an_idle_session_gets_its_admin_access_closed_where_the_timeout_fell() {
        let now = at("2026-10-03T12:00:00Z");
        let created = at("2026-10-03T07:00:00Z");
        let last_seen = at("2026-10-03T09:30:00Z");
        let closed = closed_admin_expiry(now, unclosed(created), last_seen).unwrap();
        assert_eq!(admin_access_end(closed), at("2026-10-03T11:30:00Z"));
        assert!(closed < unclosed(created));
    }

    /// Once closed, no later activity reopens it: `last_seen_at` is
    /// rewritten by the member routes, `/auth/me` included, but the admin
    /// routes read the closed `expires_at`.
    #[test]
    fn a_closed_session_stays_refused_however_recent_its_last_activity() {
        let now = at("2026-10-03T12:00:00Z");
        let created = at("2026-10-03T07:00:00Z");
        let closed =
            closed_admin_expiry(now, unclosed(created), at("2026-10-03T09:30:00Z")).unwrap();
        assert!(!superadmin_session_is_fresh(now, created, closed, now));
        assert!(!superadmin_session_is_fresh(
            now + Duration::minutes(5),
            created,
            closed,
            now
        ));
    }

    /// Closing again changes nothing: one write per session.
    #[test]
    fn a_closed_session_is_not_closed_again() {
        let now = at("2026-10-03T12:00:00Z");
        let created = at("2026-10-03T07:00:00Z");
        let last_seen = at("2026-10-03T09:30:00Z");
        let closed = closed_admin_expiry(now, unclosed(created), last_seen).unwrap();
        assert_eq!(closed_admin_expiry(now, closed, last_seen), None);
        assert_eq!(
            closed_admin_expiry(now + Duration::hours(1), closed, last_seen),
            None
        );
    }

    #[test]
    fn a_session_used_within_the_admin_timeout_is_not_closed() {
        let now = at("2026-10-03T12:00:00Z");
        let created = at("2026-10-03T07:00:00Z");
        assert_eq!(
            closed_admin_expiry(now, unclosed(created), at("2026-10-03T10:00:00Z")),
            None
        );
        assert_eq!(closed_admin_expiry(now, unclosed(created), now), None);
    }

    /// Past its 12 hours, the admin access already ended at the max age:
    /// nothing to write, and the absolute lifetime is left whole.
    #[test]
    fn a_session_past_the_max_age_is_not_closed() {
        let now = at("2026-10-03T12:00:00Z");
        let created = at("2026-10-02T16:00:00Z");
        assert_eq!(
            closed_admin_expiry(now, unclosed(created), at("2026-10-03T08:00:00Z")),
            None
        );
        // Idle since its first hour: the timeout fell within the max age,
        // but the max age has ended the admin access since.
        assert_eq!(
            closed_admin_expiry(now, unclosed(created), at("2026-10-02T17:00:00Z")),
            None
        );
    }

    /// The price of the closure: the absolute end moves earlier by what was
    /// left of the 12 hours when the timeout fell — at most 10 hours, never
    /// past the 30 days.
    #[test]
    fn closing_shortens_the_absolute_lifetime_by_at_most_ten_hours() {
        let now = at("2026-10-03T12:00:00Z");
        let created = at("2026-10-03T09:59:00Z");
        let closed = closed_admin_expiry(now, unclosed(created), created).unwrap();
        assert_eq!(unclosed(created) - closed, Duration::hours(10));
    }

    /// Unclosed, the admin access ends where the max age does.
    #[test]
    fn the_admin_access_of_an_unclosed_session_ends_at_the_max_age() {
        let created = at("2026-10-03T07:00:00Z");
        assert_eq!(
            admin_access_end(unclosed(created)),
            created + Duration::hours(SUPERADMIN_SESSION_MAX_AGE_HOURS)
        );
    }
}
