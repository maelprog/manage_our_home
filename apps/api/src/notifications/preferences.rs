//! A member's reminder channel and their subscribed devices (#306).
//!
//! Account-level, like `/account/export`: no group, and every query
//! filters on the caller's own id. `push_subscriptions` and `users` are
//! under no RLS policy.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use manage_our_home_shared::dto::notifications::{
    NotificationSettings, PushSubscriptionRequest, UpdateNotificationSettings,
};

use super::push::validate_endpoint;
use super::ReminderChannel;
use crate::auth::session::AuthUser;
use crate::error::{AppError, AppResult};
use crate::AppState;

/// `GET /account/notifications`.
pub async fn get_settings(
    State(state): State<AppState>,
    auth: AuthUser,
) -> AppResult<Json<NotificationSettings>> {
    let row = sqlx::query!(
        r#"SELECT u.reminder_channel,
                  (SELECT count(*) FROM push_subscriptions p WHERE p.user_id = u.id) AS "subscriptions!"
           FROM users u WHERE u.id = $1"#,
        auth.user_id
    )
    .fetch_one(&state.db)
    .await?;
    Ok(Json(NotificationSettings {
        reminder_channel: row.reminder_channel,
        push_subscriptions: row.subscriptions,
        vapid_public_key: state.push.as_ref().map(|v| v.public_key().to_string()),
    }))
}

/// `PUT /account/notifications` — 204, or 400 `invalid_reminder_channel`.
pub async fn update_settings(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<UpdateNotificationSettings>,
) -> AppResult<impl IntoResponse> {
    let channel = ReminderChannel::parse(&body.reminder_channel)
        .ok_or_else(|| AppError::BadRequest("invalid_reminder_channel".into()))?;
    sqlx::query!(
        "UPDATE users SET reminder_channel = $2 WHERE id = $1",
        auth.user_id,
        channel.as_str()
    )
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /account/push-subscriptions` — registers this device's endpoint
/// for the caller: 201. 400 `invalid_push_endpoint` for anything but a
/// browser push service's URL (`push::validate_endpoint`: the reminder
/// worker will POST to it), 409 `push_not_configured` on a server without
/// a VAPID key, where no browser could have subscribed anyway.
///
/// An endpoint already registered — this device, subscribed under another
/// account that signed in on it before — moves to the caller: a device
/// receives the reminders of whoever subscribed on it last. The same
/// device registered again by the same account (the page does it on every
/// view once the permission is granted) keeps its dates.
///
/// An account keeps [`MAX_DEVICES_PER_ACCOUNT`] devices: the one beyond
/// replaces the oldest (by `created_at`). The caller's `users` row is
/// locked for the transaction, so concurrent subscriptions of one account
/// are counted one after the other and the ceiling holds; `created_at` is
/// the wall clock at the insert (`clock_timestamp()`), not the
/// transaction's start, so the one that waited is not taken for the oldest.
pub async fn subscribe(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<PushSubscriptionRequest>,
) -> AppResult<impl IntoResponse> {
    if state.push.is_none() {
        return Err(AppError::Conflict("push_not_configured".into()));
    }
    let endpoint = validate_endpoint(&body.endpoint)
        .map_err(|_| AppError::BadRequest("invalid_push_endpoint".into()))?;
    let mut tx = crate::db::begin(&state.db).await?;
    sqlx::query!(
        "SELECT id FROM users WHERE id = $1 FOR UPDATE",
        auth.user_id
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        r#"INSERT INTO push_subscriptions (user_id, endpoint, created_at)
           VALUES ($1, $2, clock_timestamp())
           ON CONFLICT (endpoint) DO UPDATE
           SET user_id = EXCLUDED.user_id, created_at = EXCLUDED.created_at, last_success_at = NULL,
               consecutive_failures = 0
           WHERE push_subscriptions.user_id <> EXCLUDED.user_id"#,
        auth.user_id,
        endpoint.as_str()
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"DELETE FROM push_subscriptions WHERE id IN (
               SELECT id FROM push_subscriptions WHERE user_id = $1
               ORDER BY created_at DESC, id DESC OFFSET $2)"#,
        auth.user_id,
        MAX_DEVICES_PER_ACCOUNT
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(StatusCode::CREATED)
}

/// Devices an account keeps subscribed (controller's decision of
/// 2026-10-01): far above a household member's phones and computers, low
/// enough that a script cannot grow one account's rows — and the worker's
/// pushes per reminder — without bound.
pub const MAX_DEVICES_PER_ACCOUNT: i64 = 50;

/// `DELETE /account/push-subscriptions` — forgets every device of the
/// caller: 204. The browsers keep their side of the subscription until
/// the member withdraws the permission; nothing is sent to them any more.
pub async fn unsubscribe_all(
    State(state): State<AppState>,
    auth: AuthUser,
) -> AppResult<impl IntoResponse> {
    sqlx::query!(
        "DELETE FROM push_subscriptions WHERE user_id = $1",
        auth.user_id
    )
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
