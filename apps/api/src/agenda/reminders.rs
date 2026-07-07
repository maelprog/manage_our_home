use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agenda::recurrence;
use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::AppState;

/// How far ahead recurring-event reminders are materialized into
/// `scheduled_notifications`. The worker (jobs/scheduled_notifications.rs)
/// re-runs this refill periodically so an open-ended RRULE never needs to
/// be scheduled indefinitely up front.
pub const NOTIFICATION_WINDOW_DAYS: i64 = 30;

#[derive(Deserialize)]
pub struct CreateReminderRequest {
    pub offset_minutes: i32,
}

#[derive(Serialize)]
pub struct ReminderResponse {
    pub id: Uuid,
    pub event_id: Uuid,
    pub offset_minutes: i32,
}

pub async fn create_reminder(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<CreateReminderRequest>,
) -> AppResult<impl IntoResponse> {
    if body.offset_minutes < 0 {
        return Err(AppError::BadRequest("offset_minutes_must_be_non_negative".into()));
    }

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let event = sqlx::query!(
        "SELECT starts_at, rrule FROM events WHERE id = $1 AND group_id = $2",
        event_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    let reminder = sqlx::query!(
        r#"
        INSERT INTO event_reminders (event_id, offset_minutes)
        VALUES ($1, $2)
        RETURNING id
        "#,
        event_id,
        body.offset_minutes,
    )
    .fetch_one(&mut *tx)
    .await?;

    refill_notifications(&mut tx, reminder.id, event_id, event.starts_at, event.rrule.as_deref(), body.offset_minutes)
        .await
        .map_err(|_| AppError::Internal(anyhow::anyhow!("failed to schedule notifications")))?;

    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(ReminderResponse {
            id: reminder.id,
            event_id,
            offset_minutes: body.offset_minutes,
        }),
    ))
}

/// Materializes `scheduled_notifications` rows for every occurrence of
/// `event_id` falling within the next `NOTIFICATION_WINDOW_DAYS`, for the
/// given reminder. Idempotent via the `(event_reminder_id, occurrence_at)`
/// unique constraint — safe to call again on refill.
pub async fn refill_notifications(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    reminder_id: Uuid,
    event_id: Uuid,
    starts_at: DateTime<Utc>,
    rrule: Option<&str>,
    offset_minutes: i32,
) -> anyhow::Result<()> {
    let now = Utc::now();
    let window_end = now + Duration::days(NOTIFICATION_WINDOW_DAYS);

    let occurrences: Vec<DateTime<Utc>> = match rrule {
        Some(r) => recurrence::expand_occurrences(r, starts_at, now, window_end)?,
        None => vec![starts_at],
    };

    for occurrence_at in occurrences {
        let fire_at = occurrence_at - Duration::minutes(offset_minutes as i64);
        sqlx::query!(
            r#"
            INSERT INTO scheduled_notifications (event_reminder_id, event_id, occurrence_at, fire_at)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (event_reminder_id, occurrence_at) DO NOTHING
            "#,
            reminder_id,
            event_id,
            occurrence_at,
            fire_at,
        )
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}

pub async fn delete_reminder(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id, reminder_id)): Path<(Uuid, Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let result = sqlx::query!(
        "DELETE FROM event_reminders WHERE id = $1 AND event_id = $2",
        reminder_id,
        event_id,
    )
    .execute(&mut *tx)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
