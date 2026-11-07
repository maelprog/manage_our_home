use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Utc};
use manage_our_home_shared::dto::agenda::EditScope;
use manage_our_home_shared::validation::agenda::normalize_all_day;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::agenda::{
    attachments, can_modify, is_participant, may_set_privacy, recurrence, reminders,
};
use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::AppState;

#[derive(Deserialize, utoipa::ToSchema)]
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
    /// Visible to its participants alone — its creator and its assignees
    /// (#620, `events_isolation` in migration 0044). Missing means shared,
    /// the behaviour every event had before.
    #[serde(default)]
    pub is_private: bool,
}

#[derive(Deserialize, utoipa::ToSchema)]
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
    /// `None` leaves the event's privacy as it is. Only someone who sees
    /// the event can send it, so only a participant can change it on a
    /// private one (#620).
    pub is_private: Option<bool>,
    /// Which occurrences of a series the change applies to (#708). Missing
    /// is `all`: the row is updated in place, as every `PATCH` was before.
    /// `this` and `this_and_following` need `occurrence_at`, and a series —
    /// see `split_series`.
    #[serde(default)]
    pub scope: Option<EditScope>,
}

#[derive(Serialize, utoipa::ToSchema)]
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
    pub is_private: bool,
}

/// `title` is `None` when the request does not carry one at all — a PATCH
/// leaving the stored title alone (`COALESCE($3, title)` below). `all_day` is
/// the flag the row will be stored with: an all-day series is unrolled on
/// another anchor, and its rule has to be checked on that one
/// (`recurrence::validate`, #161).
fn validate_request(
    title: Option<&str>,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    all_day: bool,
    rrule: Option<&str>,
) -> AppResult<()> {
    // A title only the web forms refused (`validate_event_form`) was not an
    // invariant (#120): every other client of these two routes could store a
    // blank one, and an event with no readable name is unusable in the agenda
    // and in the reminders. Same `trim()` as the form, so both refuse the
    // same strings.
    if title.is_some_and(|t| t.trim().is_empty()) {
        return Err(AppError::BadRequest("title_required".into()));
    }
    if ends_at < starts_at {
        return Err(AppError::BadRequest("ends_at_before_starts_at".into()));
    }
    if let Some(r) = rrule {
        recurrence::validate(r, starts_at, all_day)
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
/// The one write path it does not cover is the Google Calendar mirror
/// (`google_calendar/imports.rs`), which writes `events` directly. That used
/// to mean imported events kept whatever bounds the feed stated, invariant or
/// no invariant (#118); the mirror now upholds the same promise through its
/// own `import_bounds`, which has to re-anchor the feed's UTC-midnight dates
/// on Paris before normalizing and so cannot simply call this.
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

#[utoipa::path(
    post,
    path = "/groups/{id}/events",
    tag = "agenda",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
    ),
    request_body = CreateEventRequest,
    responses(
        (status = 201, description = "Event created", body = EventResponse),
        (status = 400, description = "`title_required`, `ends_at_before_starts_at` or `invalid_rrule`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn create_event(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<CreateEventRequest>,
) -> AppResult<impl IntoResponse> {
    // Validated on what the client actually sent, *then* normalized: a
    // genuinely backwards range stays a 400 instead of being silently
    // repaired into a valid day.
    validate_request(
        Some(&body.title),
        body.starts_at,
        body.ends_at,
        body.all_day,
        body.rrule.as_deref(),
    )?;
    let (starts_at, ends_at) = normalized_bounds(body.all_day, body.starts_at, body.ends_at);

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let event = sqlx::query_as!(
        EventRow,
        r#"
        INSERT INTO events (group_id, created_by, title, description, location, starts_at, ends_at, all_day, is_task, rrule, is_private)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        RETURNING id, group_id, created_by, title, description, location, starts_at, ends_at, all_day, is_task, completed_at, rrule, is_private, exdates
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
        body.is_private,
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

/// Replaces `event_id`'s assignees with `assignee_ids`: the two halves
/// below, back to back. The update path runs them apart, around its write
/// to `events` (see `update_event`).
async fn replace_assignees(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event_id: Uuid,
    assignee_ids: &[Uuid],
) -> AppResult<()> {
    upsert_assignees(tx, event_id, assignee_ids).await?;
    drop_other_assignees(tx, event_id, assignee_ids).await
}

/// First half of a replacement: writes every row of the new set, the ones
/// already there included.
///
/// The set used to be replaced by deleting every row, then inserting the
/// new ones. On a private event (#620) that order locks out a participant
/// who is only an assignee: once their own row is deleted, the policy of
/// `events` no longer shows them the event, and the policy of
/// `event_assignees` refuses the insert that follows. Writing the new set
/// first, and deleting what falls out of it last, keeps them a participant
/// for as long as the replacement needs them to be.
///
/// The rows kept are rewritten, not skipped: `created_at` takes the
/// transaction's `now()` on every row of the set, as the wholesale insert
/// gave it, so `assignees_for_events` still orders them by `user_id` alone.
async fn upsert_assignees(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event_id: Uuid,
    assignee_ids: &[Uuid],
) -> AppResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO event_assignees (event_id, user_id)
        SELECT $1, u FROM UNNEST($2::uuid[]) AS u
        ON CONFLICT (event_id, user_id) DO UPDATE SET created_at = EXCLUDED.created_at
        "#,
        event_id,
        assignee_ids,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Second half of a replacement: deletes the rows outside the new set. One
/// statement, so every row it deletes is matched against the policy as it
/// stood before it ran — a participant taking themselves off the event
/// still sees the rows they are deleting.
async fn drop_other_assignees(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event_id: Uuid,
    assignee_ids: &[Uuid],
) -> AppResult<()> {
    sqlx::query!(
        "DELETE FROM event_assignees WHERE event_id = $1 AND NOT (user_id = ANY($2))",
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
    is_private: bool,
    /// The occurrences the series excludes (#707), dropped by
    /// `recurrence::expand_series`. Not part of `EventResponse`.
    exdates: Vec<DateTime<Utc>>,
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
        is_private: r.is_private,
    }
}

#[utoipa::path(
    get,
    path = "/groups/{id}/events/{event_id}",
    tag = "agenda",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("event_id" = uuid::Uuid, Path, description = "Event id"),
    ),
    responses(
        (status = 200, description = "The event", body = EventResponse),
        (status = 404, description = "`not_found`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn get_event(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let event = sqlx::query_as!(
        EventRow,
        r#"SELECT id, group_id, created_by, title, description, location, starts_at, ends_at, all_day, is_task, completed_at, rrule, is_private, exdates
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

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RangeQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct OccurrenceResponse {
    #[serde(flatten)]
    pub event: EventResponse,
    pub occurrence_starts_at: DateTime<Utc>,
    pub occurrence_ends_at: DateTime<Utc>,
}

/// The spans a stored row renders in `[from, to]` (inclusive): its unrolled
/// occurrences for a series, the row itself for a one-off.
///
/// A series that does not unroll is rendered **as a one-off** — the row on
/// its own, if it meets the window — and the error is handed back for the
/// caller to log. It used to fail `list_events` outright, and with it every
/// other event of the window: `/agenda` and the dashboard went down for one
/// row (#161). Such rows get stored by paths `validate` does not see — a
/// calendar re-import moving `starts_at` past the rule's `UNTIL`, or a row
/// written before a check existed.
///
/// Rendered rather than skipped, on purpose. Skipped, the event would
/// vanish from every view without a trace, and the family could neither see
/// that something is wrong nor open it to fix or delete it. Its own row is
/// also the one instance RFC 5545 always grants a series: `DTSTART` is its
/// first occurrence (§3.8.5.3). What is lost is the rest of the series; the
/// log is there so it does not go unnoticed.
///
/// A series' excluded occurrences (`exdates`, #707) are not rendered: the
/// unroll drops them. A one-off has none.
fn row_spans(
    rrule: Option<&str>,
    all_day: bool,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    exdates: &[DateTime<Utc>],
) -> (Vec<recurrence::OccurrenceSpan>, Option<rrule::RRuleError>) {
    let row_on_its_own = || {
        (starts_at <= to && ends_at >= from)
            .then_some((starts_at, ends_at))
            .into_iter()
            .collect()
    };
    let Some(rrule) = rrule else {
        return (row_on_its_own(), None);
    };
    // The unroll the reminders read as well (#169), all-day on civil
    // dates: see `recurrence::expand_series`.
    match recurrence::expand_series(rrule, all_day, starts_at, ends_at, from, to, exdates) {
        Ok(spans) => (spans, None),
        Err(error) => (row_on_its_own(), Some(error)),
    }
}

/// AC: lists events (and tasks, since tasks are events) visible in
/// `[from, to]`, expanding any recurring event's occurrences on the fly
/// rather than materializing them in the DB.
#[utoipa::path(
    get,
    path = "/groups/{id}/events",
    tag = "agenda",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        RangeQuery,
    ),
    responses(
        (status = 200, description = "The occurrences between `from` and `to`, recurrences expanded", body = crate::api_docs::schemas::OccurrenceList),
        (status = 400, description = "`to_before_from`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
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
        SELECT id, group_id, created_by, title, description, location, starts_at, ends_at, all_day, is_task, completed_at, rrule, is_private, exdates
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
        let assignee_ids = assignees.get(&row.id).cloned().unwrap_or_default();
        let (spans, unroll_error) = row_spans(
            row.rrule.as_deref(),
            row.all_day,
            row.starts_at,
            row.ends_at,
            range.from,
            range.to,
            &row.exdates,
        );
        if let Some(error) = unroll_error {
            tracing::error!(
                event_id = %row.id,
                group_id = %row.group_id,
                rrule = ?row.rrule,
                starts_at = %row.starts_at,
                all_day = row.all_day,
                error = %error,
                "stored series does not unroll, rendering its row on its own (#161)"
            );
        }
        let is_series = row.rrule.is_some();
        let base = event_response(row, assignee_ids);
        for (occurrence_starts_at, occurrence_ends_at) in spans {
            // A series' tasks are completed per occurrence
            // (event_occurrence_completions), a one-off on its own row.
            let completed_at = if !is_series {
                base.completed_at
            } else if base.is_task {
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
                    is_private: base.is_private,
                },
                occurrence_starts_at,
                occurrence_ends_at,
            });
        }
    }

    Ok(Json(json!({ "occurrences": occurrences })))
}

#[utoipa::path(
    patch,
    path = "/groups/{id}/events/{event_id}",
    tag = "agenda",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("event_id" = uuid::Uuid, Path, description = "Event id"),
    ),
    request_body = UpdateEventRequest,
    responses(
        (status = 200, description = "Event updated — or, under scope `this` or `this_and_following`, the event created in place of the occurrences changed", body = EventResponse),
        (status = 400, description = "`title_required`, `ends_at_before_starts_at` or `invalid_rrule`, `completed_only_valid_for_tasks` or `occurrence_at_required_for_recurring_task`; under a scope other than `all`: `scope_requires_occurrence`, `scope_requires_recurring_event`, `scope_not_allowed_for_imported_event` or `occurrence_not_in_series`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 404, description = "`not_found`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn update_event(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateEventRequest>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;

    // `FOR NO KEY UPDATE` (#391): everything below decides on this read —
    // the rule is validated against its `all_day` and `starts_at`, and the
    // bounds and rule a PATCH leaves out are written back from it. Unlocked,
    // a write committing in between (a Google sync, which locks the same row
    // since #372) was overwritten on a stale decision: an hourly rule
    // validated on an hour-bound row landed on the row the sync had just made
    // all-day, and the bounds and rule the sync had just written were
    // rewritten with the old ones. Locked, this read waits for that write to commit and
    // `scoped_tx` holds the row until this one does.
    //
    // `NO KEY`: a PATCH never changes the row's key, and the plain
    // `FOR UPDATE` would also conflict with the `FOR KEY SHARE` every foreign
    // key into `events` takes — an upload's second transaction, held through
    // `put_object`, and reminder or assignee inserts would stall the PATCH
    // for nothing. `FOR NO KEY UPDATE` still conflicts with the sync's
    // `FOR UPDATE OF e` and with any other write to the row.
    let existing = sqlx::query_as!(
        EventRow,
        r#"SELECT id, group_id, created_by, title, description, location, starts_at, ends_at, all_day, is_task, completed_at, rrule, is_private, exdates
           FROM events WHERE id = $1 AND group_id = $2 FOR NO KEY UPDATE"#,
        event_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    let current_assignees = assignees_for_events(&mut tx, &[event_id])
        .await?
        .remove(&event_id)
        .unwrap_or_default();
    let actor_is_participant =
        is_participant(auth.user_id, existing.created_by, &current_assignees);
    if !can_modify(
        &actor_role,
        existing.created_by == auth.user_id,
        existing.is_private && actor_is_participant,
    ) {
        return Err(AppError::Forbidden);
    }

    // `Some(_)` replaces the assignee set (falling back to the creator if
    // that empties it out, `resolve_assignees`); `None` leaves it as-is.
    let new_assignees = match &body.assignee_ids {
        Some(requested) => {
            let valid_members = valid_member_ids(&mut tx, group_id).await?;
            Some(resolve_assignees(
                requested,
                &valid_members,
                existing.created_by,
            ))
        }
        None => None,
    };
    let will_be_private = body.is_private.unwrap_or(existing.is_private);
    let actor_participant_after = is_participant(
        auth.user_id,
        existing.created_by,
        new_assignees.as_deref().unwrap_or(&current_assignees),
    );
    if !may_set_privacy(
        existing.is_private,
        will_be_private,
        actor_participant_after,
    ) {
        return Err(AppError::Forbidden);
    }

    let scope = body.scope.unwrap_or_default();
    if scope != EditScope::All {
        let occurrence_at = body
            .occurrence_at
            .ok_or_else(|| AppError::BadRequest("scope_requires_occurrence".into()))?;
        let Some(series_rule) = existing.rrule.clone() else {
            return Err(AppError::BadRequest(
                "scope_requires_recurring_event".into(),
            ));
        };
        if is_imported(&mut tx, event_id).await? {
            return Err(AppError::BadRequest(
                "scope_not_allowed_for_imported_event".into(),
            ));
        }
        let located = recurrence::locate_occurrence(
            &series_rule,
            existing.all_day,
            existing.starts_at,
            existing.ends_at,
            &existing.exdates,
            occurrence_at,
        )
        .ok()
        .flatten()
        .ok_or_else(|| AppError::BadRequest("occurrence_not_in_series".into()))?;
        // « This one and the following » from the first occurrence the
        // series lists is the whole series: nothing is left to cut off, and
        // the row is updated in place below.
        if scope == EditScope::This || located.listed_before > 0 {
            // Sorted: the order `assignees_for_events` reads a set written
            // in one statement back in (see `update_event`'s own return).
            let mut assignee_ids = new_assignees.unwrap_or(current_assignees);
            assignee_ids.sort();
            let event = split_series(
                &mut tx,
                Split {
                    actor: auth.user_id,
                    series: &existing,
                    series_rule: &series_rule,
                    located,
                    scope,
                    body: &body,
                    assignee_ids: &assignee_ids,
                    is_private: will_be_private,
                },
            )
            .await?;
            tx.commit().await?;
            return Ok(Json(event_response(event, assignee_ids)));
        }
    }

    let starts_at = body.starts_at.unwrap_or(existing.starts_at);
    let ends_at = body.ends_at.unwrap_or(existing.ends_at);
    let rrule = match &body.rrule {
        Some(r) if r.is_empty() => None,
        Some(r) => Some(r.clone()),
        None => existing.rrule.clone(),
    };
    // `all_day` uses the same "absent field leaves it alone" convention as
    // the SQL below (`COALESCE($8, all_day)`), so the flag the row will end
    // up with is what decides normalization — a PATCH that touches neither
    // the flag nor the timestamps still re-asserts the invariant, which is
    // exactly what makes `normalize_all_day` idempotent worth having. It
    // decides the anchor the rule is validated on, too.
    let all_day = body.all_day.unwrap_or(existing.all_day);
    validate_request(
        body.title.as_deref(),
        starts_at,
        ends_at,
        all_day,
        rrule.as_deref(),
    )?;
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
            set_occurrence_completion(&mut tx, event_id, occurrence_at, completed, auth.user_id)
                .await?;
        }
        existing.completed_at
    } else {
        match body.completed {
            Some(true) => Some(Utc::now()),
            Some(false) => None,
            None => existing.completed_at,
        }
    };

    // The new assignees are written before the row and the old ones
    // deleted after it (#620): the policy checks the row this `UPDATE`
    // writes, and on a private event it passes only for a participant. Made
    // private by someone who adds themselves to it, the event needs their
    // row already there; left by an assignee who takes themselves off it,
    // it needs theirs still there.
    if let Some(new_assignees) = &new_assignees {
        upsert_assignees(&mut tx, event_id, new_assignees).await?;
    }

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
            is_private = $11,
            updated_at = now()
        WHERE id = $1 AND group_id = $2
        RETURNING id, group_id, created_by, title, description, location, starts_at, ends_at, all_day, is_task, completed_at, rrule, is_private, exdates
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
        will_be_private,
    )
    .fetch_one(&mut *tx)
    .await?;

    let assignee_ids = match new_assignees {
        Some(mut new_assignees) => {
            drop_other_assignees(&mut tx, event_id, &new_assignees).await?;
            // Not read back: an assignee who just left a private event no
            // longer sees its assignees. Every row of the set carries the
            // same `created_at` (`upsert_assignees`), so this is the order
            // `assignees_for_events` reads them in.
            new_assignees.sort();
            new_assignees
        }
        None => current_assignees,
    };
    tx.commit().await?;

    Ok(Json(event_response(event, assignee_ids)))
}

/// Whether `event_id` mirrors an event of an imported calendar. The import
/// rewrites such a row from its feed (`google_calendar/imports.rs`) and
/// knows nothing of a series split off it: a scope other than `all` is
/// refused on it (#708).
async fn is_imported(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event_id: Uuid,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM calendar_import_events WHERE event_id = $1) AS "imported!""#,
        event_id,
    )
    .fetch_one(&mut **tx)
    .await?)
}

/// What `split_series` works from: the series as locked by `update_event`,
/// the occurrence it is split at, and the request already checked against
/// the permissions — `assignee_ids` and `is_private` are what the new event
/// gets.
struct Split<'a> {
    actor: Uuid,
    series: &'a EventRow,
    series_rule: &'a str,
    located: recurrence::OccurrenceInSeries,
    scope: EditScope,
    body: &'a UpdateEventRequest,
    assignee_ids: &'a [Uuid],
    is_private: bool,
}

/// A `PATCH` with scope `this` or `this_and_following` (#708), in the
/// caller's transaction. Returns the event it creates; the series keeps its
/// id and everything the request does not take from it.
///
/// - `this`: the occurrence is added to the series' `exdates`, and a one-off
///   event takes its place — no rule, whatever the request carries in
///   `rrule`.
/// - `this_and_following`: the series is cut right before the occurrence
///   (`recurrence::truncated_before`) and a new series carries on from it
///   with the rest of its rule (`recurrence::continued_after`), unless the
///   request gives it another one. Its exclusions and task completions from
///   the occurrence on go with it, moved as the series moves
///   (`recurrence::carried_over`).
///
/// In both cases the new event starts where the occurrence did — or where
/// `starts_at`/`ends_at` move it — and takes every other field the request
/// leaves out from the series. It is created by the series' creator, with
/// the series' reminders copied and its assignees (or the ones the request
/// sets); the attachments stay on the series. A recurring task occurrence's
/// completion moves with it: onto the one-off row's `completed_at` for
/// `this`, onto the new series for `this_and_following`, unless the request
/// sets `completed`.
///
/// The notifications already scheduled for the occurrences that leave the
/// series and are still pending are deleted; the copied reminders schedule
/// the new event's own.
async fn split_series(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    split: Split<'_>,
) -> AppResult<EventRow> {
    let Split {
        actor,
        series,
        series_rule,
        located,
        scope,
        body,
        assignee_ids,
        is_private,
    } = split;
    let (occurrence_starts_at, occurrence_ends_at) = located.span;

    let all_day = body.all_day.unwrap_or(series.all_day);
    let starts_at = body.starts_at.unwrap_or(occurrence_starts_at);
    let ends_at = body.ends_at.unwrap_or(occurrence_ends_at);
    let rrule = match scope {
        EditScope::This => None,
        _ => match &body.rrule {
            Some(r) if r.is_empty() => None,
            Some(r) => Some(r.clone()),
            None => Some(
                recurrence::continued_after(series_rule, located.produced_before)
                    .map_err(|_| AppError::BadRequest("invalid_rrule".into()))?,
            ),
        },
    };
    validate_request(
        body.title.as_deref(),
        starts_at,
        ends_at,
        all_day,
        rrule.as_deref(),
    )?;
    let (starts_at, ends_at) = normalized_bounds(all_day, starts_at, ends_at);
    if body.completed.is_some() && !series.is_task {
        return Err(AppError::BadRequest(
            "completed_only_valid_for_tasks".into(),
        ));
    }

    // A one-off task is completed on its row; for `this`, it keeps the
    // completion its occurrence had in the series.
    let one_off = rrule.is_none();
    let completed_at = match body.completed {
        Some(true) if one_off => Some(Utc::now()),
        None if scope == EditScope::This && series.is_task => sqlx::query_scalar!(
            "SELECT completed_at FROM event_occurrence_completions WHERE event_id = $1 AND occurrence_at = $2",
            series.id,
            occurrence_starts_at,
        )
        .fetch_optional(&mut **tx)
        .await?,
        _ => None,
    };
    let exdates = match scope {
        EditScope::This => Vec::new(),
        _ => recurrence::carried_over(
            &series.exdates,
            series.all_day,
            occurrence_starts_at,
            starts_at,
        ),
    };

    // Shared at first, made private below once its assignees are written:
    // the policy of `events` lets a private row through only for a
    // participant, and an assignee who is not the creator becomes one with
    // their `event_assignees` row (#620, as in `update_event`).
    let created = sqlx::query_as!(
        EventRow,
        r#"
        INSERT INTO events (group_id, created_by, title, description, location, starts_at, ends_at, all_day, is_task, completed_at, rrule, is_private, exdates)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, false, $12)
        RETURNING id, group_id, created_by, title, description, location, starts_at, ends_at, all_day, is_task, completed_at, rrule, is_private, exdates
        "#,
        series.group_id,
        series.created_by,
        body.title.clone().unwrap_or_else(|| series.title.clone()),
        body.description.clone().or_else(|| series.description.clone()),
        body.location.clone().or_else(|| series.location.clone()),
        starts_at,
        ends_at,
        all_day,
        series.is_task,
        completed_at,
        rrule,
        &exdates,
    )
    .fetch_one(&mut **tx)
    .await?;
    upsert_assignees(tx, created.id, assignee_ids).await?;
    let created = if is_private {
        sqlx::query_as!(
            EventRow,
            r#"
            UPDATE events SET is_private = true WHERE id = $1
            RETURNING id, group_id, created_by, title, description, location, starts_at, ends_at, all_day, is_task, completed_at, rrule, is_private, exdates
            "#,
            created.id,
        )
        .fetch_one(&mut **tx)
        .await?
    } else {
        created
    };

    // The series lets go of the occurrence(s), and of the notifications
    // still pending for them.
    match scope {
        EditScope::This => {
            sqlx::query!(
                "UPDATE events SET exdates = array_append(exdates, $2), updated_at = now() WHERE id = $1",
                series.id,
                occurrence_starts_at,
            )
            .execute(&mut **tx)
            .await?;
            sqlx::query!(
                "DELETE FROM event_occurrence_completions WHERE event_id = $1 AND occurrence_at = $2",
                series.id,
                occurrence_starts_at,
            )
            .execute(&mut **tx)
            .await?;
            sqlx::query!(
                "DELETE FROM scheduled_notifications WHERE event_id = $1 AND occurrence_at = $2 AND status = 'pending'",
                series.id,
                occurrence_starts_at,
            )
            .execute(&mut **tx)
            .await?;
        }
        _ => {
            let cut =
                recurrence::truncated_before(series_rule, series.all_day, occurrence_starts_at)
                    .map_err(|_| AppError::BadRequest("invalid_rrule".into()))?;
            sqlx::query!(
                "UPDATE events SET rrule = $2, updated_at = now() WHERE id = $1",
                series.id,
                cut,
            )
            .execute(&mut **tx)
            .await?;
            move_completions(tx, series, &created, occurrence_starts_at).await?;
            sqlx::query!(
                "DELETE FROM scheduled_notifications WHERE event_id = $1 AND occurrence_at >= $2 AND status = 'pending'",
                series.id,
                occurrence_starts_at,
            )
            .execute(&mut **tx)
            .await?;
            if let (Some(completed), false) = (body.completed, one_off) {
                set_occurrence_completion(tx, created.id, starts_at, completed, actor).await?;
            }
        }
    }

    copy_reminders(tx, series.id, &created).await?;
    Ok(created)
}

/// Moves the task completions of `series` from `occurrence` on to the new
/// series `to` (#708): each completion keeps its author and time, on the
/// instant its occurrence has in the new series. A new event without a rule
/// takes none: a one-off task is completed on its row.
async fn move_completions(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    series: &EventRow,
    to: &EventRow,
    occurrence: DateTime<Utc>,
) -> AppResult<()> {
    if !series.is_task || to.rrule.is_none() {
        return Ok(());
    }
    let (to, new_start) = (to.id, to.starts_at);
    let rows = sqlx::query!(
        "DELETE FROM event_occurrence_completions WHERE event_id = $1 AND occurrence_at >= $2 RETURNING occurrence_at, completed_at, completed_by",
        series.id,
        occurrence,
    )
    .fetch_all(&mut **tx)
    .await?;
    for row in rows {
        // One instant at a time, so each keeps its own completion.
        let Some(occurrence_at) =
            recurrence::carried_over(&[row.occurrence_at], series.all_day, occurrence, new_start)
                .pop()
        else {
            continue;
        };
        sqlx::query!(
            r#"
            INSERT INTO event_occurrence_completions (event_id, occurrence_at, completed_at, completed_by)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (event_id, occurrence_at) DO NOTHING
            "#,
            to,
            occurrence_at,
            row.completed_at,
            row.completed_by,
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Marks the occurrence of `event_id` at `occurrence_at` done or not done —
/// a recurring task is completed per occurrence (`update_event`).
async fn set_occurrence_completion(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event_id: Uuid,
    occurrence_at: DateTime<Utc>,
    completed: bool,
    actor: Uuid,
) -> AppResult<()> {
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
            actor,
        )
        .execute(&mut **tx)
        .await?;
    } else {
        sqlx::query!(
            "DELETE FROM event_occurrence_completions WHERE event_id = $1 AND occurrence_at = $2",
            event_id,
            occurrence_at,
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Gives `event` a copy of every reminder of `from`, and schedules each the
/// way `POST …/reminders` does (#708).
async fn copy_reminders(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    from: Uuid,
    event: &EventRow,
) -> AppResult<()> {
    let copies = sqlx::query!(
        r#"
        INSERT INTO event_reminders (event_id, offset_minutes)
        SELECT $2, offset_minutes FROM event_reminders WHERE event_id = $1
        RETURNING id, offset_minutes
        "#,
        from,
        event.id,
    )
    .fetch_all(&mut **tx)
    .await?;
    for copy in copies {
        reminders::refill_notifications(
            tx,
            copy.id,
            event.id,
            reminders::EventTimes {
                starts_at: event.starts_at,
                ends_at: event.ends_at,
                all_day: event.all_day,
                rrule: event.rrule.as_deref(),
                exdates: &event.exdates,
            },
            copy.offset_minutes,
        )
        .await
        .map_err(|_| AppError::Internal(anyhow::anyhow!("failed to schedule notifications")))?;
    }
    Ok(())
}

#[utoipa::path(
    delete,
    path = "/groups/{id}/events/{event_id}",
    tag = "agenda",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("event_id" = uuid::Uuid, Path, description = "Event id"),
    ),
    responses(
        (status = 204, description = "Event deleted, with its attachments"),
        (status = 404, description = "`not_found`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn delete_event(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;

    // `FOR UPDATE` (#391): the storage keys collected below have to be all
    // the event has when its row goes. Unlocked, an upload committing in
    // between (`upload_attachment` holds the event `FOR KEY SHARE`) added a
    // row this delete never saw: its object was not deleted, and the row went
    // with the cascade, leaving bytes nothing points at. `FOR UPDATE`
    // conflicts with `FOR KEY SHARE`: this read waits for the upload and
    // collects its key, or an upload arriving after it waits and answers 404.
    let existing = sqlx::query!(
        "SELECT created_by, is_private FROM events WHERE id = $1 AND group_id = $2 FOR UPDATE",
        event_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    // A private event is visible to its participants alone (#620): the
    // policy already answered 404 to anyone else. Checked here all the same,
    // on the same rule as `update_event`, rather than inferred from the row
    // having been found.
    let assignees = assignees_for_events(&mut tx, &[event_id])
        .await?
        .remove(&event_id)
        .unwrap_or_default();
    if !can_modify(
        &actor_role,
        existing.created_by == auth.user_id,
        existing.is_private && is_participant(auth.user_id, existing.created_by, &assignees),
    ) {
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

    // -- validate_request: title (#120) -----------------------------------

    fn refused_code(result: AppResult<()>) -> String {
        match result {
            Err(AppError::BadRequest(code)) => code,
            _ => panic!("expected a 400"),
        }
    }

    fn an_hour() -> (DateTime<Utc>, DateTime<Utc>) {
        use chrono::TimeZone;
        (
            Utc.with_ymd_and_hms(2026, 9, 10, 8, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 9, 10, 9, 0, 0).unwrap(),
        )
    }

    #[test]
    fn an_empty_title_is_refused() {
        let (s, e) = an_hour();
        assert_eq!(
            refused_code(validate_request(Some(""), s, e, false, None)),
            "title_required"
        );
    }

    #[test]
    fn a_title_of_spaces_is_refused() {
        let (s, e) = an_hour();
        assert_eq!(
            refused_code(validate_request(Some("   "), s, e, false, None)),
            "title_required"
        );
    }

    /// `str::trim` drops every Unicode whitespace, U+00A0 included — the same
    /// call `apps/shared`'s `validate_event_form` makes, so the form and the
    /// API refuse the same strings rather than merely similar ones.
    #[test]
    fn a_title_of_non_breaking_spaces_is_refused() {
        let (s, e) = an_hour();
        assert_eq!(
            refused_code(validate_request(
                Some("\u{00a0}\u{00a0}"),
                s,
                e,
                false,
                None
            )),
            "title_required"
        );
    }

    #[test]
    fn a_title_padded_with_spaces_is_accepted() {
        let (s, e) = an_hour();
        assert!(validate_request(Some("  Anniversaire  "), s, e, false, None).is_ok());
    }

    /// A PATCH that carries no `title` leaves the stored one alone, so there
    /// is nothing to check — and nothing to refuse.
    #[test]
    fn an_absent_title_is_not_checked() {
        let (s, e) = an_hour();
        assert!(validate_request(None, s, e, false, None).is_ok());
    }

    /// Both guards broken: the title is the one reported, so the client fixes
    /// the field the form would have flagged first.
    #[test]
    fn a_blank_title_outranks_a_backwards_range() {
        let (s, e) = an_hour();
        assert_eq!(
            refused_code(validate_request(Some(" "), e, s, false, None)),
            "title_required"
        );
    }

    #[test]
    fn a_backwards_range_under_a_valid_title_is_still_refused() {
        let (s, e) = an_hour();
        assert_eq!(
            refused_code(validate_request(Some("Anniversaire"), e, s, false, None)),
            "ends_at_before_starts_at"
        );
    }

    // -- row_spans (#161) -------------------------------------------------

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn a_one_off_row_inside_the_window_renders_once() {
        let (s, e) = (utc(2026, 9, 10, 8, 0), utc(2026, 9, 10, 9, 0));
        let (spans, error) = row_spans(
            None,
            false,
            s,
            e,
            utc(2026, 9, 1, 0, 0),
            utc(2026, 9, 30, 0, 0),
            &[],
        );
        assert_eq!(spans, vec![(s, e)]);
        assert!(error.is_none());
    }

    #[test]
    fn a_one_off_row_touching_a_window_bound_is_kept_and_one_outside_is_not() {
        let (s, e) = (utc(2026, 9, 10, 8, 0), utc(2026, 9, 10, 9, 0));
        assert_eq!(
            row_spans(None, false, s, e, e, utc(2026, 9, 11, 0, 0), &[]).0,
            vec![(s, e)]
        );
        assert_eq!(
            row_spans(None, false, s, e, utc(2026, 9, 9, 0, 0), s, &[]).0,
            vec![(s, e)]
        );
        assert!(row_spans(
            None,
            false,
            s,
            e,
            utc(2026, 9, 11, 0, 0),
            utc(2026, 9, 12, 0, 0),
            &[]
        )
        .0
        .is_empty());
    }

    #[test]
    fn a_series_that_unrolls_renders_its_occurrences() {
        let s = utc(2026, 9, 10, 8, 0);
        let (spans, error) = row_spans(
            Some("FREQ=DAILY;COUNT=3"),
            false,
            s,
            s + chrono::Duration::hours(1),
            utc(2026, 9, 1, 0, 0),
            utc(2026, 9, 30, 0, 0),
            &[],
        );
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[2].0, utc(2026, 9, 12, 8, 0));
        assert!(error.is_none());
    }

    #[test]
    fn a_series_moved_past_its_until_renders_its_row_and_reports_why() {
        // The re-import reproduction from #161: a series valid on
        // 2026-10-01, moved by the feed to 2026-10-25 past its UNTIL.
        let (s, e) = (utc(2026, 10, 25, 1, 30), utc(2026, 10, 25, 2, 30));
        let (spans, error) = row_spans(
            Some("FREQ=DAILY;UNTIL=20261010T235959Z"),
            false,
            s,
            e,
            utc(2026, 10, 1, 0, 0),
            utc(2026, 10, 31, 23, 59),
            &[],
        );
        assert_eq!(spans, vec![(s, e)]);
        assert!(error.is_some());
    }

    #[test]
    fn an_all_day_series_until_the_day_before_renders_its_row_and_reports_why() {
        // The all-day reproduction from #161, stored before write refused it.
        let (s, e) = (utc(2026, 9, 4, 22, 0), utc(2026, 9, 5, 22, 0));
        let (spans, error) = row_spans(
            Some("FREQ=DAILY;UNTIL=20260904T235959Z"),
            true,
            s,
            e,
            utc(2026, 8, 31, 22, 0),
            utc(2026, 9, 30, 21, 59),
            &[],
        );
        assert_eq!(spans, vec![(s, e)]);
        assert!(error.is_some());
    }

    #[test]
    fn a_series_that_does_not_unroll_outside_the_window_renders_nothing() {
        let (s, e) = (utc(2026, 10, 25, 1, 30), utc(2026, 10, 25, 2, 30));
        let (spans, error) = row_spans(
            Some("FREQ=DAILY;UNTIL=20261010T235959Z"),
            false,
            s,
            e,
            utc(2026, 11, 1, 0, 0),
            utc(2026, 11, 30, 0, 0),
            &[],
        );
        assert!(spans.is_empty());
        assert!(error.is_some());
    }
}
