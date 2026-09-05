use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Utc};
use manage_our_home_shared::validation::agenda::normalize_all_day;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::agenda::{attachments, can_modify, recurrence};
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
    /// Family members this event is for. Missing/empty defaults to
    /// `[creator]` (see `resolve_assignees`) — issue #73 asked for "assigned
    /// to the creator by default" rather than an event with nobody on it.
    #[serde(default)]
    pub assignee_ids: Option<Vec<Uuid>>,
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
    /// Required alongside `completed` when the task is recurring — completion
    /// is tracked per occurrence, not on the series as a whole, so we need to
    /// know *which* occurrence is being marked done/undone.
    pub occurrence_at: Option<DateTime<Utc>>,
    /// `None` leaves the current assignees untouched (same convention as
    /// every other field here); `Some(_)` replaces them, defaulting back to
    /// `[creator]` if that leaves nothing (e.g. every box unchecked).
    #[serde(default)]
    pub assignee_ids: Option<Vec<Uuid>>,
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
    pub assignee_ids: Vec<Uuid>,
}

fn validate_request(
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    rrule: Option<&str>,
) -> AppResult<()> {
    if ends_at < starts_at {
        return Err(AppError::BadRequest("ends_at_before_starts_at".into()));
    }
    if let Some(r) = rrule {
        recurrence::validate(r, starts_at)
            .map_err(|_| AppError::BadRequest("invalid_rrule".into()))?;
    }
    Ok(())
}

/// The timestamps to store for an event, given whether it is `all_day`.
///
/// `all_day` is not a display flag: it carries the promise that the event
/// covers whole civil days, and nothing enforced that promise before #101 —
/// `new.rs` only set the checkbox, and the two `datetime-local` fields
/// default to "now" and "now + 1 h". A birthday created at 08:00 → 09:00 was
/// therefore *finished* at 09:01 and dropped off the dashboard, which keeps
/// occurrences by `occurrence_ends_at` (`apps/web/src/routes/home.rs`, #73).
///
/// This sits in the API rather than in the form that reported the bug on
/// purpose. `apps/web` writes events from two places (`agenda/new.rs` and
/// `agenda/edit.rs`), and an invariant one of them upholds is not an
/// invariant — the edit form, or any other client of `POST/PATCH
/// /groups/:id/events`, would keep minting rows that break it. Every write
/// those routes make funnels through `create_event`/`update_event`, so this
/// is the narrowest place that covers all of them.
///
/// The one write path it does *not* cover is the Google Calendar mirror
/// (`google_calendar/imports.rs`), which INSERTs into `events` directly. Its
/// timestamps come from the feed rather than from a user, and are left as
/// the feed states them — see the PR for #101.
fn normalized_bounds(
    all_day: bool,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    if all_day {
        normalize_all_day(starts_at, ends_at)
    } else {
        (starts_at, ends_at)
    }
}

pub async fn create_event(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<CreateEventRequest>,
) -> AppResult<impl IntoResponse> {
    // Validated on what the client actually sent, *then* normalized: a
    // genuinely backwards range stays a 400 instead of being silently
    // repaired into a valid day.
    validate_request(body.starts_at, body.ends_at, body.rrule.as_deref())?;
    let (starts_at, ends_at) = normalized_bounds(body.all_day, body.starts_at, body.ends_at);

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
        starts_at,
        ends_at,
        body.all_day,
        body.is_task,
        body.rrule,
    )
    .fetch_one(&mut *tx)
    .await?;

    let valid_members = valid_member_ids(&mut tx, group_id).await?;
    let assignee_ids = resolve_assignees(
        &body.assignee_ids.unwrap_or_default(),
        &valid_members,
        auth.user_id,
    );
    replace_assignees(&mut tx, event.id, &assignee_ids).await?;
    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(event_response(event, assignee_ids)),
    ))
}

/// Every family member id for `group_id` — the set an event's assignees
/// must be drawn from (`resolve_assignees` filters against it).
async fn valid_member_ids(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
) -> AppResult<Vec<Uuid>> {
    let rows = sqlx::query!(
        "SELECT user_id FROM group_members WHERE group_id = $1",
        group_id,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.into_iter().map(|r| r.user_id).collect())
}

/// Resolves the final assignee set for an event: the requested ids,
/// filtered to actual family members (a stale or forged id is dropped
/// rather than rejecting the whole request) and deduplicated in the order
/// they were requested, or `[creator]` when that leaves nothing — the
/// "assigned to the creator by default" rule from issue #73, which also
/// covers an explicit empty selection (a form with no box checked): an
/// event is never left with zero assignees.
fn resolve_assignees(requested: &[Uuid], valid_members: &[Uuid], creator: Uuid) -> Vec<Uuid> {
    let mut seen = std::collections::HashSet::new();
    let resolved: Vec<Uuid> = requested
        .iter()
        .filter(|id| valid_members.contains(id))
        .filter(|id| seen.insert(**id))
        .copied()
        .collect();
    if resolved.is_empty() {
        vec![creator]
    } else {
        resolved
    }
}

/// Replaces `event_id`'s assignees wholesale — simpler than diffing, and
/// the table is small (at most a family's member count).
async fn replace_assignees(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event_id: Uuid,
    assignee_ids: &[Uuid],
) -> AppResult<()> {
    sqlx::query!("DELETE FROM event_assignees WHERE event_id = $1", event_id,)
        .execute(&mut **tx)
        .await?;
    sqlx::query!(
        r#"
        INSERT INTO event_assignees (event_id, user_id)
        SELECT $1, u FROM UNNEST($2::uuid[]) AS u
        "#,
        event_id,
        assignee_ids,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Assignees for a batch of events, grouped by event id — the same
/// separate-fetch-then-merge shape `list_events` already uses for
/// `event_occurrence_completions`, so a range query costs one extra
/// `ANY($1)` lookup rather than N.
///
/// `user_id` is the tie-break, and it is not decoration: `replace_assignees`
/// inserts a whole set in one `UNNEST`, so every row of an event shares the
/// same `created_at` and `ORDER BY created_at` alone left Postgres free to
/// return them in any order. The dashboard renders the *first* assignee's
/// initial in the avatar, so that would have made the letter in the ring —
/// and the order of the names beside it — flicker between two page loads of
/// the same unchanged event (#98 verification, round 2).
async fn assignees_for_events(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event_ids: &[Uuid],
) -> AppResult<HashMap<Uuid, Vec<Uuid>>> {
    if event_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query!(
        r#"
        SELECT event_id, user_id FROM event_assignees
        WHERE event_id = ANY($1)
        ORDER BY event_id, created_at, user_id
        "#,
        event_ids,
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut map: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for row in rows {
        map.entry(row.event_id).or_default().push(row.user_id);
    }
    Ok(map)
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

/// `EventRow` doesn't carry `assignee_ids` (it comes from a separate
/// `event_assignees` fetch, see `assignees_for_events`), so this is a plain
/// function rather than `From` — every call site has to supply it, which is
/// the point: there is no accidental all-zero-assignees response.
fn event_response(r: EventRow, assignee_ids: Vec<Uuid>) -> EventResponse {
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
        assignee_ids,
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
    let assignee_ids = assignees_for_events(&mut tx, &[event_id])
        .await?
        .remove(&event_id)
        .unwrap_or_default();
    tx.commit().await?;

    Ok(Json(event_response(event, assignee_ids)))
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

    // Recurring tasks track completion per occurrence (see
    // event_occurrence_completions) rather than on the events row itself —
    // fetch the completions for this window so each expanded occurrence can
    // report its own completed_at instead of inheriting the series'.
    let recurring_task_ids: Vec<Uuid> = rows
        .iter()
        .filter(|r| r.is_task && r.rrule.is_some())
        .map(|r| r.id)
        .collect();
    let occurrence_completions: HashMap<(Uuid, DateTime<Utc>), DateTime<Utc>> =
        if recurring_task_ids.is_empty() {
            HashMap::new()
        } else {
            sqlx::query!(
                r#"
            SELECT event_id, occurrence_at, completed_at
            FROM event_occurrence_completions
            WHERE event_id = ANY($1) AND occurrence_at BETWEEN $2 AND $3
            "#,
                &recurring_task_ids,
                range.from,
                range.to,
            )
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|r| ((r.event_id, r.occurrence_at), r.completed_at))
            .collect()
        };
    let event_ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let assignees = assignees_for_events(&mut tx, &event_ids).await?;
    tx.commit().await?;

    let mut occurrences = Vec::new();
    for row in rows {
        let duration = row.ends_at - row.starts_at;
        let assignee_ids = assignees.get(&row.id).cloned().unwrap_or_default();
        if let Some(rrule) = row.rrule.clone() {
            // An all-day series is unrolled on civil dates, not on instants.
            // Its stored start sits on Paris midnight — 22:00Z in summer,
            // 23:00Z in winter — so unrolling it in UTC carries every later
            // occurrence onto the neighbouring day as soon as the clocks
            // change, which is #101's own symptom re-created one level up.
            // See `recurrence::expand_all_day_occurrences`.
            let spans: Vec<recurrence::OccurrenceSpan> = if row.all_day {
                recurrence::expand_all_day_occurrences(
                    &rrule,
                    row.starts_at,
                    row.ends_at,
                    range.from,
                    range.to,
                )
            } else {
                recurrence::expand_occurrences(&rrule, row.starts_at, range.from, range.to)
                    .map(|starts| starts.into_iter().map(|s| (s, s + duration)).collect())
            }
            .map_err(|_| AppError::Internal(anyhow::anyhow!("failed to expand rrule")))?;
            let base = event_response(row, assignee_ids);
            for (occurrence_starts_at, occurrence_ends_at) in spans {
                let completed_at = if base.is_task {
                    occurrence_completions
                        .get(&(base.id, occurrence_starts_at))
                        .copied()
                } else {
                    None
                };
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
                        completed_at,
                        rrule: base.rrule.clone(),
                        assignee_ids: base.assignee_ids.clone(),
                    },
                    occurrence_starts_at,
                    occurrence_ends_at,
                });
            }
        } else if row.starts_at <= range.to && row.ends_at >= range.from {
            let starts_at = row.starts_at;
            let ends_at = row.ends_at;
            occurrences.push(OccurrenceResponse {
                event: event_response(row, assignee_ids),
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
        "SELECT created_by, starts_at, ends_at, all_day, rrule, is_task, completed_at FROM events WHERE id = $1 AND group_id = $2",
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
    // `all_day` uses the same "absent field leaves it alone" convention as
    // the SQL below (`COALESCE($8, all_day)`), so the flag the row will end
    // up with is what decides normalization — a PATCH that touches neither
    // the flag nor the timestamps still re-asserts the invariant, which is
    // exactly what makes `normalize_all_day` idempotent worth having.
    let all_day = body.all_day.unwrap_or(existing.all_day);
    let (starts_at, ends_at) = normalized_bounds(all_day, starts_at, ends_at);

    if body.completed.is_some() && !existing.is_task {
        return Err(AppError::BadRequest(
            "completed_only_valid_for_tasks".into(),
        ));
    }

    // Recurring tasks: completion is per-occurrence (event_occurrence_completions),
    // never on the events row — otherwise completing one occurrence would mark
    // the whole series (every past/future occurrence) as done. One-off tasks
    // keep using events.completed_at directly, as before.
    let completed_at = if existing.rrule.is_some() {
        if let Some(completed) = body.completed {
            let occurrence_at = body.occurrence_at.ok_or(AppError::BadRequest(
                "occurrence_at_required_for_recurring_task".into(),
            ))?;
            if completed {
                sqlx::query!(
                    r#"
                    INSERT INTO event_occurrence_completions (event_id, occurrence_at, completed_by)
                    VALUES ($1, $2, $3)
                    ON CONFLICT (event_id, occurrence_at)
                    DO UPDATE SET completed_at = now(), completed_by = $3
                    "#,
                    event_id,
                    occurrence_at,
                    auth.user_id,
                )
                .execute(&mut *tx)
                .await?;
            } else {
                sqlx::query!(
                    "DELETE FROM event_occurrence_completions WHERE event_id = $1 AND occurrence_at = $2",
                    event_id,
                    occurrence_at,
                )
                .execute(&mut *tx)
                .await?;
            }
        }
        existing.completed_at
    } else {
        match body.completed {
            Some(true) => Some(Utc::now()),
            Some(false) => None,
            None => existing.completed_at,
        }
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

    // `Some(_)` replaces the assignee set (falling back to the creator if
    // that empties it out, `resolve_assignees`); `None` leaves it as-is.
    if let Some(requested) = &body.assignee_ids {
        let valid_members = valid_member_ids(&mut tx, group_id).await?;
        let assignee_ids = resolve_assignees(requested, &valid_members, existing.created_by);
        replace_assignees(&mut tx, event_id, &assignee_ids).await?;
    }
    let assignee_ids = assignees_for_events(&mut tx, &[event_id])
        .await?
        .remove(&event_id)
        .unwrap_or_default();
    tx.commit().await?;

    Ok(Json(event_response(event, assignee_ids)))
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

    // The attachment rows cascade from `events`, their objects do not:
    // once the rows are gone nothing references the stored bytes again, so
    // collect the keys and drop the objects first (see `delete_objects` for
    // why this order and not the reverse).
    let storage_keys = attachments::storage_keys_for_events(&mut tx, &[event_id]).await?;
    attachments::delete_objects(&state.storage, &storage_keys).await?;

    sqlx::query!(
        "DELETE FROM events WHERE id = $1 AND group_id = $2",
        event_id,
        group_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uid(n: u8) -> Uuid {
        Uuid::from_u128(u128::from(n))
    }

    // -- resolve_assignees ------------------------------------------------

    #[test]
    fn no_requested_assignees_defaults_to_the_creator() {
        let creator = uid(1);
        let members = vec![creator, uid(2)];
        assert_eq!(resolve_assignees(&[], &members, creator), vec![creator]);
    }

    #[test]
    fn a_single_valid_assignee_is_kept_as_is() {
        let creator = uid(1);
        let members = vec![creator, uid(2)];
        assert_eq!(
            resolve_assignees(&[uid(2)], &members, creator),
            vec![uid(2)]
        );
    }

    #[test]
    fn several_valid_assignees_are_kept_in_requested_order() {
        let creator = uid(1);
        let members = vec![uid(1), uid(2), uid(3)];
        assert_eq!(
            resolve_assignees(&[uid(3), uid(1)], &members, creator),
            vec![uid(3), uid(1)]
        );
    }

    #[test]
    fn duplicates_in_the_request_are_deduplicated() {
        let creator = uid(1);
        let members = vec![uid(1), uid(2)];
        assert_eq!(
            resolve_assignees(&[uid(2), uid(2), uid(1)], &members, creator),
            vec![uid(2), uid(1)]
        );
    }

    #[test]
    fn an_id_that_is_not_a_family_member_is_dropped() {
        let creator = uid(1);
        let members = vec![uid(1), uid(2)];
        let outsider = uid(99);
        assert_eq!(
            resolve_assignees(&[outsider, uid(2)], &members, creator),
            vec![uid(2)]
        );
    }

    #[test]
    fn requesting_only_invalid_ids_falls_back_to_the_creator() {
        let creator = uid(1);
        let members = vec![uid(1), uid(2)];
        let outsider = uid(99);
        assert_eq!(
            resolve_assignees(&[outsider], &members, creator),
            vec![creator]
        );
    }
}
