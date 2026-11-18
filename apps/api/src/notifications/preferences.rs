//! A member's reminder channel and their subscribed devices (#306), and
//! the hour of their daily expiry recap (#700), and whether they are told
//! of the agenda changes (#850).
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

use super::push::{self, validate_endpoint};
use super::{parse_expiry_digest_hour, ReminderChannel};
use crate::auth::session::AuthUser;
use crate::error::{AppError, AppResult};
use crate::AppState;

/// `GET /account/notifications`.
#[utoipa::path(
    get,
    path = "/account/notifications",
    tag = "notifications",
    responses(
        (status = 200, description = "The account's notification settings", body = NotificationSettings),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn get_settings(
    State(state): State<AppState>,
    auth: AuthUser,
) -> AppResult<Json<NotificationSettings>> {
    let row = sqlx::query!(
        r#"SELECT u.reminder_channel, u.expiry_digest_hour, u.expiry_digest_enabled,
                  u.notify_agenda_added,
                  u.notify_agenda_changed, u.notify_agenda_deleted,
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
        expiry_digest_hour: row.expiry_digest_hour,
        expiry_digest_enabled: row.expiry_digest_enabled,
        notify_agenda_added: row.notify_agenda_added,
        notify_agenda_changed: row.notify_agenda_changed,
        notify_agenda_deleted: row.notify_agenda_deleted,
    }))
}

/// `PUT /account/notifications` — 204, or 400 `invalid_reminder_channel`,
/// or 400 `invalid_expiry_digest_hour` for a recap hour outside 0..23
/// (`parse_expiry_digest_hour`, #700). A field left out — the recap's
/// hour or switch, any of the three agenda switches (#853) — stays as it
/// is; the recap off, its hour is kept, and may still be changed.
#[utoipa::path(
    put,
    path = "/account/notifications",
    tag = "notifications",
    request_body = UpdateNotificationSettings,
    responses(
        (status = 204, description = "Settings saved"),
        (status = 400, description = "`invalid_reminder_channel` or `invalid_expiry_digest_hour`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn update_settings(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<UpdateNotificationSettings>,
) -> AppResult<impl IntoResponse> {
    let channel = ReminderChannel::parse(&body.reminder_channel)
        .ok_or_else(|| AppError::BadRequest("invalid_reminder_channel".into()))?;
    let digest_hour = body
        .expiry_digest_hour
        .map(|hour| {
            parse_expiry_digest_hour(hour)
                .ok_or_else(|| AppError::BadRequest("invalid_expiry_digest_hour".into()))
        })
        .transpose()?;
    sqlx::query!(
        r#"UPDATE users SET reminder_channel = $2,
                  expiry_digest_hour = COALESCE($3, expiry_digest_hour),
                  expiry_digest_enabled = COALESCE($4, expiry_digest_enabled),
                  notify_agenda_added = COALESCE($5, notify_agenda_added),
                  notify_agenda_changed = COALESCE($6, notify_agenda_changed),
                  notify_agenda_deleted = COALESCE($7, notify_agenda_deleted)
           WHERE id = $1"#,
        auth.user_id,
        channel.as_str(),
        digest_hour,
        body.expiry_digest_enabled,
        body.notify_agenda_added,
        body.notify_agenda_changed,
        body.notify_agenda_deleted,
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
/// receives the reminders of whoever subscribed on it last, with a fresh
/// record. The same device registered again by the same account (the page
/// does it on every view once the permission is granted) keeps its record
/// and only has `last_seen_at` moved.
///
/// An account keeps [`MAX_DEVICES_PER_ACCOUNT`] devices: the one beyond
/// replaces the device least recently in use — registered (`last_seen_at`)
/// or delivered to (`last_success_at`), whichever is later. The caller's
/// `users` row is locked for the transaction, so concurrent subscriptions
/// of one account are counted one after the other and the ceiling holds;
/// the dates written are the wall clock at the write (`clock_timestamp()`),
/// not the transaction's start, so a subscription that waited for the lock
/// is not taken for the least recent.
#[utoipa::path(
    post,
    path = "/account/push-subscriptions",
    tag = "notifications",
    request_body = PushSubscriptionRequest,
    responses(
        (status = 201, description = "Push subscription stored"),
        (status = 400, description = "`invalid_push_endpoint`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 409, description = "`push_not_configured`: the server has no VAPID key", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
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
        r#"INSERT INTO push_subscriptions (user_id, endpoint, created_at, last_seen_at)
           VALUES ($1, $2, clock_timestamp(), clock_timestamp())
           ON CONFLICT (endpoint) DO UPDATE SET
               last_seen_at = EXCLUDED.last_seen_at,
               user_id = EXCLUDED.user_id,
               created_at = CASE WHEN push_subscriptions.user_id = EXCLUDED.user_id
                   THEN push_subscriptions.created_at ELSE EXCLUDED.created_at END,
               last_success_at = CASE WHEN push_subscriptions.user_id = EXCLUDED.user_id
                   THEN push_subscriptions.last_success_at END,
               consecutive_failures = CASE WHEN push_subscriptions.user_id = EXCLUDED.user_id
                   THEN push_subscriptions.consecutive_failures ELSE 0 END,
               failing_since = CASE WHEN push_subscriptions.user_id = EXCLUDED.user_id
                   THEN push_subscriptions.failing_since END,
               pending_push_kind = CASE WHEN push_subscriptions.user_id = EXCLUDED.user_id
                   THEN push_subscriptions.pending_push_kind END,
               pending_pushes = CASE WHEN push_subscriptions.user_id = EXCLUDED.user_id
                   THEN push_subscriptions.pending_pushes ELSE 0 END,
               pending_until = CASE WHEN push_subscriptions.user_id = EXCLUDED.user_id
                   THEN push_subscriptions.pending_until END"#,
        auth.user_id,
        endpoint.as_str()
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"DELETE FROM push_subscriptions WHERE id IN (
               SELECT id FROM push_subscriptions WHERE user_id = $1
               ORDER BY GREATEST(last_seen_at, last_success_at) DESC, id DESC OFFSET $2)"#,
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

/// `POST /account/push-subscriptions/received`, `{"endpoint": …}` — what
/// the service worker asks on each push, which carries nothing: 200
/// `{"kind": …}`, the kind of notification this device got
/// (`push::pending_on_receipt`), `null` for the neutral text — pushes of
/// two kinds on their way, none pending, or an endpoint that is not one of
/// the caller's devices. The receipt is taken off the device's count.
#[utoipa::path(
    post,
    path = "/account/push-subscriptions/received",
    tag = "notifications",
    request_body = PushSubscriptionRequest,
    responses(
        (status = 200, description = "What the device is to show for the push it received", body = crate::api_docs::schemas::PushReceived),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn received(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<PushSubscriptionRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let mut tx = crate::db::begin(&state.db).await?;
    let device = sqlx::query!(
        r#"SELECT id, pending_push_kind, pending_pushes, pending_until FROM push_subscriptions
           WHERE user_id = $1 AND endpoint = $2 FOR UPDATE"#,
        auth.user_id,
        body.endpoint,
    )
    .fetch_optional(&mut *tx)
    .await?;
    let mut shown = None;
    if let Some(device) = device {
        let pending = push::Pending::from_columns(
            device.pending_push_kind.as_deref(),
            device.pending_pushes,
            device.pending_until,
        );
        let after;
        (shown, after) = push::pending_on_receipt(pending, chrono::Utc::now());
        write_pending(&mut tx, device.id, after).await?;
    }
    tx.commit().await?;
    Ok(Json(
        serde_json::json!({ "kind": shown.map(push::PushKind::as_str) }),
    ))
}

/// Writes the pushes pending on `device`.
pub(crate) async fn write_pending(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    device: uuid::Uuid,
    pending: push::Pending,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE push_subscriptions
           SET pending_push_kind = $2, pending_pushes = $3, pending_until = $4
           WHERE id = $1"#,
        device,
        pending.kind.map(push::PendingKind::as_str),
        pending.count,
        pending.until,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// `DELETE /account/push-subscriptions` — forgets every device of the
/// caller: 204. The browsers keep their side of the subscription until
/// the member withdraws the permission; nothing is sent to them any more.
#[utoipa::path(
    delete,
    path = "/account/push-subscriptions",
    tag = "notifications",
    responses(
        (status = 204, description = "Every push subscription of the account removed"),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn read(json: &str) -> UpdateNotificationSettings {
        serde_json::from_str(json).unwrap()
    }

    /// #853: the recap's switch and its hour are each set or left as they
    /// are; the hour is never cleared, and is kept while the recap is off.
    #[test]
    fn the_recap_switch_and_hour_are_each_set_or_left() {
        let base = r#""reminder_channel":"push""#;
        let off = read(&format!(
            r#"{{{base},"expiry_digest_enabled":false,"expiry_digest_hour":7}}"#
        ));
        assert_eq!(
            (off.expiry_digest_enabled, off.expiry_digest_hour),
            (Some(false), Some(7))
        );
        let left = read(&format!("{{{base}}}"));
        assert_eq!(
            (left.expiry_digest_enabled, left.expiry_digest_hour),
            (None, None)
        );
        assert!(
            serde_json::from_str::<UpdateNotificationSettings>(&format!(
                r#"{{{base},"expiry_digest_enabled":"no"}}"#
            ))
            .is_err()
        );
    }

    /// What the web sends is what the API reads: a field left as it is goes
    /// out as nothing at all.
    #[test]
    fn the_update_round_trips() {
        for (on, hour) in [(Some(false), Some(21)), (Some(true), None), (None, None)] {
            let sent = UpdateNotificationSettings {
                reminder_channel: "none".into(),
                expiry_digest_enabled: on,
                expiry_digest_hour: hour,
                notify_agenda_added: Some(true),
                notify_agenda_changed: None,
                notify_agenda_deleted: Some(false),
            };
            let json = serde_json::to_string(&sent).unwrap();
            let back = read(&json);
            assert_eq!(
                (back.expiry_digest_enabled, back.expiry_digest_hour),
                (on, hour),
                "{json}"
            );
            assert_eq!(back.notify_agenda_added, Some(true));
            assert_eq!(back.notify_agenda_changed, None);
            assert_eq!(back.notify_agenda_deleted, Some(false));
            assert!(!json.contains("null"), "{json}");
        }
    }
}
