//! The deactivated-account routes (#289): the only ones a restricted
//! session — opened by a correct login on an account the superadmin
//! deactivated (#256) — may call. Each extracts [`DeactivatedSession`];
//! every other route extracts `AuthUser`, which answers such a session with
//! 403 `account_deactivated`.

use axum::extract::State;
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use serde_json::json;
use tower_cookies::Cookies;

use manage_our_home_shared::dto::auth::{DeactivatedAccountResponse, ReactivationRequestBody};
use manage_our_home_shared::validation::user_admin::validate_reactivation_message;

use crate::error::{AppError, AppResult};
use crate::AppState;

use super::session::{clear_session_cookie, revoke_session, DeactivatedSession};

/// `GET /account/deactivated`: when the account was deactivated, and
/// whether a reactivation request is pending.
pub async fn status(
    State(state): State<AppState>,
    session: DeactivatedSession,
) -> AppResult<Json<DeactivatedAccountResponse>> {
    let row = sqlx::query!(
        r#"
        SELECT u.deactivated_at AS "deactivated_at!", r.requested_at AS "requested_at?"
        FROM users u
        LEFT JOIN account_reactivation_requests r ON r.user_id = u.id
        WHERE u.id = $1 AND u.deactivated_at IS NOT NULL AND u.deleted_at IS NULL
        "#,
        session.user_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::Unauthorized)?;

    Ok(Json(DeactivatedAccountResponse {
        deactivated_at: row.deactivated_at,
        reactivation_requested_at: row.requested_at,
    }))
}

/// `POST /account/deactivated/reactivation-request`: records the holder's
/// request, with an optional note, for the superadmin to decide on. One
/// pending request at a time (409 `reactivation_already_requested`). While
/// it is pending, the purge 2 years after the deactivation is suspended
/// (`jobs::account_purge::purge_due`). Written to `audit_log`.
///
/// The `users` row is locked first, as the purge and the superadmin's
/// actions lock it: a request can land neither on an account being purged
/// nor on one reactivated in the meantime.
pub async fn request_reactivation(
    State(state): State<AppState>,
    session: DeactivatedSession,
    Json(body): Json<ReactivationRequestBody>,
) -> AppResult<impl IntoResponse> {
    let message = validate_reactivation_message(body.message.as_deref().unwrap_or(""))
        .map_err(|code| AppError::Unprocessable(code.into()))?;

    let mut tx = crate::db::begin(&state.db).await?;
    sqlx::query_scalar!(
        r#"SELECT id FROM users
           WHERE id = $1 AND deactivated_at IS NOT NULL AND deleted_at IS NULL
           FOR UPDATE"#,
        session.user_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::Unauthorized)?;

    let inserted = sqlx::query!(
        r#"INSERT INTO account_reactivation_requests (user_id, message)
           VALUES ($1, $2)
           ON CONFLICT (user_id) DO NOTHING"#,
        session.user_id,
        message
    )
    .execute(&mut *tx)
    .await?;
    if inserted.rows_affected() == 0 {
        return Err(AppError::Conflict("reactivation_already_requested".into()));
    }

    crate::audit::record(
        &mut tx,
        Some(session.user_id),
        "account_reactivation_requested",
        "user",
        &session.user_id.to_string(),
        json!({}),
    )
    .await?;
    tx.commit().await?;

    Ok(StatusCode::CREATED)
}

/// `POST /account/deactivated/logout`: `POST /auth/logout`'s counterpart,
/// which only takes a full session.
pub async fn logout(
    State(state): State<AppState>,
    cookies: Cookies,
    session: DeactivatedSession,
) -> AppResult<impl IntoResponse> {
    revoke_session(&state.db, session.session_id).await?;
    clear_session_cookie(&cookies);
    Ok(StatusCode::NO_CONTENT)
}
