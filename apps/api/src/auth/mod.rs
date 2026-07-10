pub mod oauth_google;
pub mod session;

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower_cookies::Cookies;
use uuid::Uuid;

use manage_our_home_shared::dto::auth::{
    ChangePasswordRequest, ForgotPasswordRequest, LoginRequest, MeResponse, RegisterRequest,
    ResendVerificationRequest, ResetPasswordRequest, SetPasswordRequest,
};

use manage_our_home_shared::validation::auth::{
    validate_display_name, validate_email, validate_password,
};

use crate::crypto::{hash_password, verify_password};
use crate::error::{AppError, AppResult};
use crate::AppState;

use self::session::{
    build_session_cookie, create_session, expired_session_cookie, revoke_all_sessions,
    revoke_session, AuthUser, SESSION_COOKIE_NAME,
};

const EMAIL_VERIFICATION_TTL_HOURS: i64 = 24;
const PASSWORD_RESET_TTL_HOURS: i64 = 24;
const ACCOUNT_DELETION_GRACE_DAYS: i64 = 30;

/// Maps a validation error code (`&'static str`) to a 422 carrying that
/// exact code in the response's `error` field.
fn unprocessable(code: &'static str) -> AppError {
    AppError::Unprocessable(code.into())
}

/// AC #15 (issue #15): "who am I" for the frontend's server-side session
/// check on every page load. Reuses the existing `AuthUser` extractor —
/// 200 with the shape below if extraction succeeds, 401
/// `{"error":"unauthorized"}` otherwise (identical to every other
/// `AuthUser`-gated handler, see `session.rs`'s `FromRequestParts` impl).
pub async fn me(auth: AuthUser) -> Json<MeResponse> {
    Json(MeResponse {
        user_id: auth.user_id,
        email: auth.email,
        display_name: auth.display_name,
        email_verified: auth.email_verified,
    })
}

/// AC #1, #2: registers an email/password account, always unverified,
/// sends a verification email. Returns a generic 409 on duplicate email
/// so the response never reveals whether the existing account uses a
/// password, Google, or both.
pub async fn register(
    State(state): State<AppState>,
    Json(body): Json<RegisterRequest>,
) -> AppResult<impl IntoResponse> {
    validate_email(&body.email).map_err(unprocessable)?;
    validate_password(&body.password).map_err(unprocessable)?;
    validate_display_name(&body.display_name).map_err(unprocessable)?;

    let existing = sqlx::query_scalar!("SELECT id FROM users WHERE email = $1", body.email)
        .fetch_optional(&state.db)
        .await?;
    if existing.is_some() {
        return Err(AppError::Conflict("account_already_exists".into()));
    }

    let password_hash = hash_password(&body.password).map_err(AppError::Internal)?;

    let mut tx = state.db.begin().await?;
    let user = sqlx::query!(
        r#"
        INSERT INTO users (email, password_hash, display_name, email_verified)
        VALUES ($1, $2, $3, false)
        RETURNING id
        "#,
        body.email,
        password_hash,
        body.display_name,
    )
    .fetch_one(&mut *tx)
    .await?;

    let expires_at = Utc::now() + Duration::hours(EMAIL_VERIFICATION_TTL_HOURS);
    let token = sqlx::query_scalar!(
        r#"
        INSERT INTO email_verification_tokens (user_id, expires_at)
        VALUES ($1, $2)
        RETURNING token
        "#,
        user.id,
        expires_at,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    // Lands on apps/web's /verify-email page (which consumes the token via
    // this API's GET /auth/verify-email), not on the API endpoint itself.
    let link = format!("{}/verify-email?token={token}", state.frontend_base_url);
    if let Err(e) = state
        .email
        .send(
            &body.email,
            "Vérifiez votre email",
            crate::email::EmailSender::verification_email_body(&link),
        )
        .await
    {
        tracing::error!(error = ?e, "failed to send verification email");
    }

    Ok((StatusCode::CREATED, Json(json!({ "user_id": user.id }))))
}

#[derive(Deserialize)]
pub struct VerifyEmailQuery {
    pub token: Uuid,
}

pub async fn verify_email(
    State(state): State<AppState>,
    Query(query): Query<VerifyEmailQuery>,
) -> AppResult<impl IntoResponse> {
    let mut tx = state.db.begin().await?;
    let row = sqlx::query!(
        r#"
        SELECT user_id, expires_at, consumed_at
        FROM email_verification_tokens
        WHERE token = $1
        FOR UPDATE
        "#,
        query.token
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    if row.consumed_at.is_some() {
        return Err(AppError::Gone);
    }
    if row.expires_at < Utc::now() {
        return Err(AppError::Gone);
    }

    sqlx::query!(
        "UPDATE email_verification_tokens SET consumed_at = now() WHERE token = $1",
        query.token
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE users SET email_verified = true WHERE id = $1",
        row.user_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(StatusCode::OK)
}

pub async fn login(
    State(state): State<AppState>,
    cookies: Cookies,
    Json(body): Json<LoginRequest>,
) -> AppResult<impl IntoResponse> {
    let user = sqlx::query!(
        "SELECT id, password_hash, email_verified FROM users WHERE email = $1 AND deleted_at IS NULL",
        body.email
    )
    .fetch_optional(&state.db)
    .await?;

    let user = match user {
        Some(u) => u,
        None => return Err(AppError::Unauthorized),
    };

    let Some(hash) = user.password_hash else {
        return Err(AppError::Unauthorized);
    };

    let ok = verify_password(&body.password, &hash).map_err(AppError::Internal)?;
    if !ok {
        return Err(AppError::Unauthorized);
    }
    // A password is only usable once its owning email has been verified —
    // covers both fresh registrations and password added to a
    // previously Google-only account (AC #7).
    if !user.email_verified {
        return Err(AppError::Unauthorized);
    }

    let session_id = create_session(&state.db, user.id).await?;
    cookies.add(build_session_cookie(session_id, state.secure_cookies));

    Ok((StatusCode::OK, Json(json!({ "user_id": user.id }))))
}

pub async fn logout(
    State(state): State<AppState>,
    cookies: Cookies,
    auth: AuthUser,
) -> AppResult<impl IntoResponse> {
    revoke_session(&state.db, auth.session_id).await?;
    cookies.add(expired_session_cookie());
    Ok(StatusCode::NO_CONTENT)
}

/// AC #4: identical response whether or not the account exists, to avoid
/// leaking which emails are registered (anti-enumeration).
pub async fn forgot_password(
    State(state): State<AppState>,
    Json(body): Json<ForgotPasswordRequest>,
) -> AppResult<impl IntoResponse> {
    if let Some(user) = sqlx::query!(
        "SELECT id FROM users WHERE email = $1 AND deleted_at IS NULL",
        body.email
    )
    .fetch_optional(&state.db)
    .await?
    {
        let expires_at = Utc::now() + Duration::hours(PASSWORD_RESET_TTL_HOURS);
        let token = sqlx::query_scalar!(
            r#"
            INSERT INTO password_reset_tokens (user_id, expires_at)
            VALUES ($1, $2)
            RETURNING token
            "#,
            user.id,
            expires_at,
        )
        .fetch_one(&state.db)
        .await?;

        // Lands on apps/web's /reset-password form (which POSTs the new
        // password to this API's /auth/password/reset), not on the API
        // endpoint itself (POST-only — a GET there would 405).
        let link = format!("{}/reset-password?token={token}", state.frontend_base_url);
        if let Err(e) = state
            .email
            .send(
                &body.email,
                "Réinitialisation de mot de passe",
                crate::email::EmailSender::password_reset_body(&link),
            )
            .await
        {
            tracing::error!(error = ?e, "failed to send password reset email");
        }
    }

    Ok(StatusCode::OK)
}

const RESEND_COOLDOWN_MINUTES: i32 = 5;

/// Anti-enumeration: always returns 200 with an empty body, whether the
/// email is unknown, already verified, or unverified. Mirrors
/// `forgot_password`. When the account exists and is still unverified, any
/// outstanding verification tokens are invalidated and a fresh one is
/// issued and emailed (best-effort). A per-email cooldown is enforced
/// entirely in SQL: if a token was issued for this user within the last
/// `RESEND_COOLDOWN_MINUTES`, the whole operation is a silent no-op (no new
/// token, no email).
pub async fn resend_verification(
    State(state): State<AppState>,
    Json(body): Json<ResendVerificationRequest>,
) -> AppResult<impl IntoResponse> {
    let Some(user) = sqlx::query!(
        "SELECT id FROM users WHERE email = $1 AND email_verified = false AND deleted_at IS NULL",
        body.email
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(StatusCode::OK);
    };

    let expires_at = Utc::now() + Duration::hours(EMAIL_VERIFICATION_TTL_HOURS);
    // Cooldown, invalidation and issuance happen atomically in one
    // statement: the new token is inserted (and old ones consumed) only
    // when no token was created within the cooldown window, so concurrent
    // resends can't slip past the rate limit.
    let token = sqlx::query_scalar!(
        r#"
        WITH recent AS (
            SELECT 1
            FROM email_verification_tokens
            WHERE user_id = $1
              AND created_at > now() - make_interval(mins => $3::int)
            LIMIT 1
        ),
        invalidated AS (
            UPDATE email_verification_tokens
            SET consumed_at = now()
            WHERE user_id = $1
              AND consumed_at IS NULL
              AND NOT EXISTS (SELECT 1 FROM recent)
        )
        INSERT INTO email_verification_tokens (user_id, expires_at)
        SELECT $1, $2
        WHERE NOT EXISTS (SELECT 1 FROM recent)
        RETURNING token
        "#,
        user.id,
        expires_at,
        RESEND_COOLDOWN_MINUTES,
    )
    .fetch_optional(&state.db)
    .await?;

    if let Some(token) = token {
        let link = format!("{}/auth/verify-email?token={token}", state.public_base_url);
        if let Err(e) = state
            .email
            .send(
                &body.email,
                "Vérifiez votre email",
                crate::email::EmailSender::verification_email_body(&link),
            )
            .await
        {
            tracing::error!(error = ?e, "failed to send verification email");
        }
    }

    Ok(StatusCode::OK)
}

/// AC #4: consuming a valid reset token revokes every active session.
pub async fn reset_password(
    State(state): State<AppState>,
    Json(body): Json<ResetPasswordRequest>,
) -> AppResult<impl IntoResponse> {
    let mut tx = state.db.begin().await?;
    let row = sqlx::query!(
        r#"
        SELECT user_id, expires_at, consumed_at
        FROM password_reset_tokens
        WHERE token = $1
        FOR UPDATE
        "#,
        body.token
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    if row.consumed_at.is_some() || row.expires_at < Utc::now() {
        return Err(AppError::Gone);
    }

    validate_password(&body.new_password).map_err(unprocessable)?;
    let password_hash = hash_password(&body.new_password).map_err(AppError::Internal)?;

    sqlx::query!(
        "UPDATE password_reset_tokens SET consumed_at = now() WHERE token = $1",
        body.token
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE users SET password_hash = $1 WHERE id = $2",
        password_hash,
        row.user_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    revoke_all_sessions(&state.db, row.user_id, None).await?;

    Ok(StatusCode::OK)
}

/// AC #5: requires the current password; keeps the calling session alive
/// and revokes every other active session.
pub async fn change_password(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<ChangePasswordRequest>,
) -> AppResult<impl IntoResponse> {
    let user = sqlx::query!(
        "SELECT password_hash FROM users WHERE id = $1",
        auth.user_id
    )
    .fetch_one(&state.db)
    .await?;

    let Some(hash) = user.password_hash else {
        return Err(AppError::Unprocessable("no_password_set".into()));
    };
    if !verify_password(&body.current_password, &hash).map_err(AppError::Internal)? {
        return Err(AppError::Unauthorized);
    }

    validate_password(&body.new_password).map_err(unprocessable)?;
    let new_hash = hash_password(&body.new_password).map_err(AppError::Internal)?;
    sqlx::query!(
        "UPDATE users SET password_hash = $1 WHERE id = $2",
        new_hash,
        auth.user_id
    )
    .execute(&state.db)
    .await?;

    revoke_all_sessions(&state.db, auth.user_id, Some(auth.session_id)).await?;

    Ok(StatusCode::OK)
}

/// AC #7: adding a password to a Google-only account resets
/// `email_verified` to false and sends a fresh verification link; `login`
/// refuses password auth until that link is consumed, so the password is
/// inert in the meantime.
pub async fn set_password(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<SetPasswordRequest>,
) -> AppResult<impl IntoResponse> {
    let user = sqlx::query!(
        "SELECT password_hash FROM users WHERE id = $1",
        auth.user_id
    )
    .fetch_one(&state.db)
    .await?;
    if user.password_hash.is_some() {
        return Err(AppError::Conflict("password_already_set".into()));
    }

    validate_password(&body.new_password).map_err(unprocessable)?;
    let password_hash = hash_password(&body.new_password).map_err(AppError::Internal)?;

    let mut tx = state.db.begin().await?;
    sqlx::query!(
        "UPDATE users SET password_hash = $1, email_verified = false WHERE id = $2",
        password_hash,
        auth.user_id
    )
    .execute(&mut *tx)
    .await?;
    let expires_at = Utc::now() + Duration::hours(EMAIL_VERIFICATION_TTL_HOURS);
    let token = sqlx::query_scalar!(
        r#"
        INSERT INTO email_verification_tokens (user_id, expires_at)
        VALUES ($1, $2)
        RETURNING token
        "#,
        auth.user_id,
        expires_at,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    // Lands on apps/web's /verify-email page (which consumes the token via
    // this API's GET /auth/verify-email), not on the API endpoint itself.
    let link = format!("{}/verify-email?token={token}", state.frontend_base_url);
    if let Err(e) = state
        .email
        .send(
            &auth.email,
            "Vérifiez votre email",
            crate::email::EmailSender::verification_email_body(&link),
        )
        .await
    {
        tracing::error!(error = ?e, "failed to send verification email");
    }

    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
pub struct DeleteAccountRequest {
    pub current_password: Option<String>,
}

#[derive(Serialize)]
struct BlockingGroup {
    group_id: Uuid,
    name: String,
}

/// AC #6: blocked (409, with the list of owned groups) while the user
/// owns any group; otherwise schedules deletion at now()+30d.
pub async fn delete_account(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<DeleteAccountRequest>,
) -> AppResult<impl IntoResponse> {
    let user = sqlx::query!(
        "SELECT password_hash FROM users WHERE id = $1",
        auth.user_id
    )
    .fetch_one(&state.db)
    .await?;
    if let Some(hash) = user.password_hash {
        let provided = body.current_password.ok_or(AppError::Unauthorized)?;
        if !verify_password(&provided, &hash).map_err(AppError::Internal)? {
            return Err(AppError::Unauthorized);
        }
    }
    // Google-only accounts: re-consent is validated on the frontend flow
    // before this endpoint is called (session freshness is enforced by
    // requiring `AuthUser`, i.e. a live session).

    let owned_groups = sqlx::query_as!(
        BlockingGroup,
        r#"
        SELECT g.id as group_id, g.name
        FROM group_members gm
        JOIN groups g ON g.id = gm.group_id
        WHERE gm.user_id = $1 AND gm.role = 'owner'
        "#,
        auth.user_id
    )
    .fetch_all(&state.db)
    .await?;

    if !owned_groups.is_empty() {
        return Err(AppError::ConflictJson(json!({
            "error": "owner_of_groups",
            "groups": owned_groups.iter().map(|g| json!({"id": g.group_id, "name": g.name})).collect::<Vec<_>>(),
        })));
    }

    let mut tx = state.db.begin().await?;
    sqlx::query!(
        "UPDATE users SET deletion_requested_at = now() WHERE id = $1",
        auth.user_id
    )
    .execute(&mut *tx)
    .await?;
    crate::audit::record(
        &mut tx,
        Some(auth.user_id),
        "account_deletion_requested",
        "user",
        &auth.user_id.to_string(),
        json!({}),
    )
    .await?;
    tx.commit().await?;

    Ok(StatusCode::OK)
}

pub async fn cancel_delete_account(
    State(state): State<AppState>,
    auth: AuthUser,
) -> AppResult<impl IntoResponse> {
    let mut tx = state.db.begin().await?;
    let updated = sqlx::query!(
        r#"
        UPDATE users
        SET deletion_requested_at = NULL
        WHERE id = $1 AND deletion_requested_at IS NOT NULL AND deleted_at IS NULL
        "#,
        auth.user_id
    )
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    crate::audit::record(
        &mut tx,
        Some(auth.user_id),
        "account_deletion_cancelled",
        "user",
        &auth.user_id.to_string(),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::OK)
}

pub const _SESSION_COOKIE_NAME_REEXPORT: &str = SESSION_COOKIE_NAME;
pub const _ACCOUNT_DELETION_GRACE_DAYS: i64 = ACCOUNT_DELETION_GRACE_DAYS;

#[cfg(test)]
mod tests {
    use super::*;

    /// `me`'s only logic is reshaping an already-extracted `AuthUser` into
    /// `MeResponse` — the 401 path is entirely the `AuthUser` extractor's
    /// responsibility (identical to every other `AuthUser`-gated handler)
    /// and is exercised end-to-end in `apps/api/tests/auth_flow.rs`
    /// (`GET /auth/me` without a session cookie). This covers the 200
    /// shape directly, without spinning up a full router + DB.
    #[tokio::test]
    async fn me_returns_authenticated_user_shape() {
        let auth = AuthUser {
            user_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            email: "alice@example.test".into(),
            display_name: "Alice".into(),
            email_verified: true,
        };
        let user_id = auth.user_id;

        let Json(body) = me(auth).await;

        assert_eq!(body.user_id, user_id);
        assert_eq!(body.email, "alice@example.test");
        assert_eq!(body.display_name, "Alice");
        assert!(body.email_verified);
    }
}
