pub mod admin;

use axum::async_trait;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use chrono::Utc;
use tower_cookies::Cookies;
use uuid::Uuid;

use crate::auth::session::SESSION_COOKIE_NAME;
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

/// Mirrors `AuthUser` (session-cookie lookup against the normal `db` pool,
/// since a session's validity is itself a tenant-agnostic fact), but
/// additionally requires `users.is_superadmin = true`, else 403. Only
/// handlers gated behind this extractor are allowed to touch
/// `AppState.admin_db` (the `BYPASSRLS` pool) — see `src/user_admin/admin.rs`.
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
        let app_state = AppState::from_ref(state);
        let cookies = Cookies::from_request_parts(parts, state)
            .await
            .map_err(|_| AppError::Unauthorized)?;
        let cookie = cookies
            .get(SESSION_COOKIE_NAME)
            .ok_or(AppError::Unauthorized)?;
        let session_id: Uuid = cookie.value().parse().map_err(|_| AppError::Unauthorized)?;

        let row = sqlx::query!(
            r#"
            SELECT s.expires_at, s.revoked_at,
                   u.id as user_id, u.deleted_at, u.is_superadmin
            FROM sessions s
            JOIN users u ON u.id = s.user_id
            WHERE s.id = $1
            "#,
            session_id
        )
        .fetch_optional(&app_state.db)
        .await
        .map_err(AppError::from)?
        .ok_or(AppError::Unauthorized)?;

        if row.revoked_at.is_some() || row.expires_at < Utc::now() || row.deleted_at.is_some() {
            return Err(AppError::Unauthorized);
        }

        if !is_superadmin(row.is_superadmin) {
            return Err(AppError::Forbidden);
        }

        Ok(SuperAdminUser {
            user_id: row.user_id,
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
