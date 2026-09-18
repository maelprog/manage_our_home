pub mod admin;

use axum::async_trait;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
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

/// `AuthUser` plus `users.is_superadmin = true`, else 403. Only handlers
/// gated behind this extractor are allowed to touch `AppState.admin_db` (the
/// `BYPASSRLS` pool) — see `src/user_admin/admin.rs`.
///
/// The session check is `AuthUser`'s, run first: an invalid session is a 401
/// whatever the account's flag, and only a valid one gets as far as the 403.
/// It also means an admin request refreshes `sessions.last_seen_at` like any
/// other authenticated request (#196 — the copy of the check this extractor
/// used to carry skipped that refresh).
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
        Ok(SuperAdminUser {
            user_id: auth.user_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::is_superadmin;

    #[test]
    fn superadmin_flag_true_is_allowed() {
        assert!(is_superadmin(true));
    }

    #[test]
    fn superadmin_flag_false_is_denied() {
        assert!(!is_superadmin(false));
    }
}
