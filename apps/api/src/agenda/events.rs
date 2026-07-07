use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::agenda::{can_modify, recurrence};
use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::AppState;

#[derive(Deserialize)]
pub struct CreateEventRequest {
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    #[serde(default)]
    pub all_day: bool,
    #[serde(default)]
    pub is_task: bool,
    pub rrule: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateEventRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub all_day: Option<bool>,
    pub rrule: Option<String>,
    pub completed: Option<bool>,
}

#[derive(Serialize)]
pub struct EventResponse {
    pub id: Uuid,
    pub group_id: Uuid,
    pub created_by: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub all_day: bool,
    pub is_task: bool,
    pub completed_at: Option<DateTime<Utc>>,
    pub rrule: Option<String>,
}

fn validate_request(starts_at: DateTime<Utc>, ends_at: DateTime<Utc>, rrule: Option<&str>) -> AppResult<()> {
    if ends_at < starts_at {
        return Err(AppError::BadRequest("ends_at_before_starts_at".into()));
    }
    if let Some(r) = rrule {
        recurrence::validate(r, starts_at).map_err(|_| AppError::BadRequest("invalid_rrule".into()))?;
    }
    Ok(())
}

pub async fn create_event(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<CreateEventRequest>,
) -> AppResult<impl IntoResponse> {
    validate_request(body.starts_at, body.ends_at, body.rrule.as_deref())?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let event = sqlx::query_as!(
        EventRow,
        r#"
        INSERT INTO events (group_id, created_by, title, description, location, starts_at, ends_at, all_day, is_task, rrule)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        RETURNING id, group_id, created_by, title, description, location, starts_at, ends_at, all_day, is_task, completed_at, rrule
        "#,
        group_id,
        auth.user_id,
        body.title,
        body.description,
        body.location,
        body.starts_at,
        body.ends_at,
        body.all_day,
        body.is_task,
        body.rrule,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(EventResponse::from(event))))
}

struct EventRow {
    id: Uuid,
    group_id: Uuid,
    created_by: Uuid,
    title: String,
    description: Option<String>,
    location: Option<String>,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    all_day: bool,
    is_task: bool,
    completed_at: Option<DateTime<Utc>>,
    rrule: Option<String>,
}

impl From<EventRow> for EventResponse {
    fn from(r: EventRow) -> Self {
        EventResponse {
            id: r.id,
            group_id: r.group_id,
            created_by: r.created_by,
            title: r.title,
            description: r.description,
            location: r.location,
            starts_at: r.starts_at,
            ends_at: r.ends_at,
            all_day: r.all_day,
            is_task: r.is_task,
            completed_at: r.completed_at,
            rrule: r.rrule,
        }
    }
}

pub async fn get_event(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let event = sqlx::query_as!(
        EventRow,
        r#"SELECT id, group_id, created_by, title, description, location, starts_at, ends_at, all_day, is_task, completed_at, rrule
           FROM events WHERE id = $1 AND group_id = $2"#,
        event_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    tx.commit().await?;

    Ok(Json(EventResponse::from(event)))
}

#[derive(Deserialize)]
pub struct RangeQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct OccurrenceResponse {
    #[serde(flatten)]
    pub event: EventResponse,
    pub occurrence_starts_at: DateTime<Utc>,
    pub occurrence_ends_at: DateTime<Utc>,
}

/// AC: lists events (and tasks, since tasks are events) visible in
/// `[from, to]`, expanding any recurring event's occurrences on the fly
/// rather than materializing them in the DB.
pub async fn list_events(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Query(range): Query<RangeQuery>,
) -> AppResult<impl IntoResponse> {
    if range.to < range.from {
        return Err(AppError::BadRequest("to_before_from".into()));
    }

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let rows = sqlx::query_as!(
        EventRow,
        r#"
        SELECT id, group_id, created_by, title, description, location, starts_at, ends_at, all_day, is_task, completed_at, rrule
        FROM events
        WHERE group_id = $1
          AND starts_at <= $3
          AND (rrule IS NOT NULL OR ends_at >= $2)
        ORDER BY starts_at
        "#,
        group_id,
        range.from,
        range.to,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut occurrences = Vec::new();
    for row in rows {
        let duration = row.ends_at - row.starts_at;
        if let Some(rrule) = row.rrule.clone() {
            let starts = recurrence::expand_occurrences(&rrule, row.starts_at, range.from, range.to)
                .map_err(|_| AppError::Internal(anyhow::anyhow!("failed to expand rrule")))?;
            let base = EventResponse::from(row);
            for occurrence_starts_at in starts {
                occurrences.push(OccurrenceResponse {
                    event: EventResponse {
                        id: base.id,
                        group_id: base.group_id,
                        created_by: base.created_by,
                        title: base.title.clone(),
                        description: base.description.clone(),
                        location: base.location.clone(),
                        starts_at: base.starts_at,
                        ends_at: base.ends_at,
                        all_day: base.all_day,
                        is_task: base.is_task,
                        completed_at: base.completed_at,
                        rrule: base.rrule.clone(),
                    },
                    occurrence_starts_at,
                    occurrence_ends_at: occurrence_starts_at + duration,
                });
            }
        } else if row.starts_at <= range.to && row.ends_at >= range.from {
            let starts_at = row.starts_at;
            let ends_at = row.ends_at;
            occurrences.push(OccurrenceResponse {
                event: EventResponse::from(row),
                occurrence_starts_at: starts_at,
                occurrence_ends_at: ends_at,
            });
        }
    }

    Ok(Json(json!({ "occurrences": occurrences })))
}

pub async fn update_event(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateEventRequest>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;

    let existing = sqlx::query!(
        "SELECT created_by, starts_at, ends_at, rrule, is_task, completed_at FROM events WHERE id = $1 AND group_id = $2",
        event_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    if !can_modify(&actor_role, existing.created_by == auth.user_id) {
        return Err(AppError::Forbidden);
    }

    let starts_at = body.starts_at.unwrap_or(existing.starts_at);
    let ends_at = body.ends_at.unwrap_or(existing.ends_at);
    let rrule = match &body.rrule {
        Some(r) if r.is_empty() => None,
        Some(r) => Some(r.clone()),
        None => existing.rrule.clone(),
    };
    validate_request(starts_at, ends_at, rrule.as_deref())?;

    if body.completed.is_some() && !existing.is_task {
        return Err(AppError::BadRequest("completed_only_valid_for_tasks".into()));
    }
    let completed_at = match body.completed {
        Some(true) => Some(Utc::now()),
        Some(false) => None,
        None => existing.completed_at,
    };

    let event = sqlx::query_as!(
        EventRow,
        r#"
        UPDATE events SET
            title = COALESCE($3, title),
            description = COALESCE($4, description),
            location = COALESCE($5, location),
            starts_at = $6,
            ends_at = $7,
            all_day = COALESCE($8, all_day),
            rrule = $9,
            completed_at = $10,
            updated_at = now()
        WHERE id = $1 AND group_id = $2
        RETURNING id, group_id, created_by, title, description, location, starts_at, ends_at, all_day, is_task, completed_at, rrule
        "#,
        event_id,
        group_id,
        body.title,
        body.description,
        body.location,
        starts_at,
        ends_at,
        body.all_day,
        rrule,
        completed_at,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(EventResponse::from(event)))
}

pub async fn delete_event(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;

    let existing = sqlx::query!(
        "SELECT created_by FROM events WHERE id = $1 AND group_id = $2",
        event_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    if !can_modify(&actor_role, existing.created_by == auth.user_id) {
        return Err(AppError::Forbidden);
    }

    sqlx::query!("DELETE FROM events WHERE id = $1 AND group_id = $2", event_id, group_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}
