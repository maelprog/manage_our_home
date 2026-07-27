use axum::async_trait;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use chrono::{Duration, Utc};
use cookie::{Cookie, SameSite};
use sqlx::{PgPool, Postgres, Transaction};
use tower_cookies::Cookies;
use uuid::Uuid;

use crate::error::AppError;
use crate::AppState;

pub const SESSION_COOKIE_NAME: &str = "session_id";
pub const SESSION_TTL_DAYS: i64 = 30;

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub session_id: Uuid,
    pub email: String,
    pub display_name: String,
    pub email_verified: bool,
    /// The global technical superadmin flag (`users.is_superadmin`, migration
    /// 0009). Carried here so `GET /auth/me` can tell `apps/web` whether to
    /// render the `/admin` nav/route tree (front epic F9, #24) — the backend's
    /// `SuperAdminUser` extractor stays the authority for the `/admin/*`
    /// endpoints themselves.
    pub is_superadmin: bool,
    /// Whether the account authenticates with a password
    /// (`users.password_hash IS NOT NULL`). Carried here so `GET /auth/me` can
    /// tell `apps/web` whether the RGPD deletion flow must ask for the current
    /// password (front epic F10, #25) — `delete_account` verifies it only for
    /// such accounts, and stays the authority.
    pub has_password: bool,
    /// Pending self-service deletion request (`users.deletion_requested_at`),
    /// exposed through `GET /auth/me` so `apps/web` can render the grace-period
    /// banner + cancel action (front epic F10, #25). Read live on every request,
    /// like `is_superadmin`, so requesting or cancelling takes effect on the next
    /// page load.
    pub deletion_requested_at: Option<chrono::DateTime<Utc>>,
}

#[async_trait]
impl<S> FromRequestParts<S> for AuthUser
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
            SELECT s.id as session_id, s.expires_at, s.revoked_at,
                   u.id as user_id, u.email, u.display_name, u.email_verified,
                   u.is_superadmin, u.deleted_at, u.deletion_requested_at,
                   (u.password_hash IS NOT NULL) as "has_password!"
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

        sqlx::query!(
            "UPDATE sessions SET last_seen_at = now() WHERE id = $1",
            session_id
        )
        .execute(&app_state.db)
        .await
        .ok();

        Ok(AuthUser {
            user_id: row.user_id,
            session_id: row.session_id,
            email: row.email,
            display_name: row.display_name,
            email_verified: row.email_verified,
            is_superadmin: row.is_superadmin,
            has_password: row.has_password,
            deletion_requested_at: row.deletion_requested_at,
        })
    }
}

pub fn build_session_cookie(session_id: Uuid, secure: bool) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE_NAME, session_id.to_string()))
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(cookie::time::Duration::days(SESSION_TTL_DAYS))
        .build()
}

pub fn expired_session_cookie() -> Cookie<'static> {
    let mut c = Cookie::build((SESSION_COOKIE_NAME, "")).path("/").build();
    c.make_removal();
    c
}

pub async fn create_session(pool: &PgPool, user_id: Uuid) -> Result<Uuid, sqlx::Error> {
    let expires_at = Utc::now() + Duration::days(SESSION_TTL_DAYS);
    let rec = sqlx::query!(
        r#"
        INSERT INTO sessions (user_id, expires_at)
        VALUES ($1, $2)
        RETURNING id
        "#,
        user_id,
        expires_at
    )
    .fetch_one(pool)
    .await?;
    Ok(rec.id)
}

pub async fn revoke_session(pool: &PgPool, session_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE sessions SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
        session_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Revokes every active session for a user. `except` optionally keeps one
/// session alive (used by the authenticated password-change flow, AC #5).
pub async fn revoke_all_sessions(
    pool: &PgPool,
    user_id: Uuid,
    except: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        UPDATE sessions
        SET revoked_at = now()
        WHERE user_id = $1 AND revoked_at IS NULL AND id IS DISTINCT FROM $2
        "#,
        user_id,
        except
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Opens a transaction with `app.family_id` / `app.user_id` set via
/// `SET LOCAL`, so RLS policies on `group_members`/`invitations`/`groups`
/// enforce tenant isolation at the DB layer for every query run on it
/// (architecture.md's RLS-from-v1 requirement, AC #15).
pub async fn scoped_tx<'a>(
    pool: &'a PgPool,
    family_id: Uuid,
    user_id: Uuid,
) -> Result<Transaction<'a, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('app.family_id', $1, true)")
        .bind(family_id.to_string())
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT set_config('app.user_id', $1, true)")
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// Same as `scoped_tx` but without a known `family_id` yet (e.g. listing
/// "my groups"). Only `app.user_id` is set; the `groups` RLS policy falls
/// back to membership-based visibility in this case.
pub async fn user_scoped_tx<'a>(
    pool: &'a PgPool,
    user_id: Uuid,
) -> Result<Transaction<'a, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('app.user_id', $1, true)")
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// Resolves an invitation's `group_id` from its token alone (before
/// `family_id` is known), relying on the `invitations` RLS policy's
/// token-based branch rather than bypassing RLS.
pub async fn token_scoped_tx<'a>(
    pool: &'a PgPool,
    token: Uuid,
) -> Result<Transaction<'a, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('app.invitation_token', $1, true)")
        .bind(token.to_string())
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}
