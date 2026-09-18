//! Request/response shapes for the Agenda endpoints
//! (`apps/api/src/agenda/`), consumed by `apps/web`'s SSR client. Kept
//! field-for-field identical to `apps/api`'s wire structs so there is one
//! documented shape; only the fields `apps/web` needs are declared (serde
//! ignores extras like `created_at`/`updated_at` on deserialize). The
//! backend is *not* modified by this epic — these mirror it, they don't
//! replace it (contrast the Auth epic, where the structs physically moved).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `POST /groups/:id/events` request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateEventRequest {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub all_day: bool,
    pub is_task: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rrule: Option<String>,
    /// Family members this event is for. `None`/empty defaults to
    /// `[creator]` on the backend (issue #73 — "assigned to the creator by
    /// default").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee_ids: Option<Vec<Uuid>>,
}

/// `PATCH /groups/:id/events/:event_id` request body. Every field is
/// optional; `completed` + `occurrence_at` drive per-occurrence completion
/// of a recurring task (see `apps/api/src/agenda/events.rs::update_event`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateEventRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ends_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub all_day: Option<bool>,
    /// An empty string clears the recurrence (matches the backend's
    /// `Some("") => None` handling); `None` leaves it unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rrule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurrence_at: Option<DateTime<Utc>>,
    /// `None` leaves the current assignees untouched; `Some(_)` (even
    /// empty) replaces them, falling back to `[creator]` if that would
    /// leave none (see the backend's `resolve_assignees`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee_ids: Option<Vec<Uuid>>,
}

/// `GET/POST/PATCH /groups/:id/events[/:event_id]` response body. For a
/// recurring task, `completed_at` on this base row is *not* authoritative —
/// per-occurrence completion is reported by `OccurrenceResponse` instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Family members this event is for. **Treat this list as possibly
    /// empty.** The `/agenda` write paths do keep at least one (the creator,
    /// by default — see `CreateEventRequest::assignee_ids` and the backend's
    /// `resolve_assignees`), but they are not the only way a row enters
    /// `events`, and the database carries no constraint of its own.
    ///
    /// The one known way this could still arrive empty holds only if one
    /// day several API instances run against the same database: events
    /// predating `0011_event_assignees.sql`, which created the junction
    /// table empty, read through one instance while another is part-way
    /// through the pass that applies `0013_backfill_event_assignees.sql`.
    /// A single instance cannot serve it: `apps/api/src/main.rs` finishes
    /// the migration pass before it binds its listener, and
    /// `infra/docker-compose.yml` runs one `api`. Were it to hold, the
    /// window would close when the pass finishes — those events then carry
    /// their creator.
    ///
    /// A second way stayed in that list after #105 had already closed it:
    /// the same events on a deployment where `0013` had run and inserted
    /// nothing. Read that file's header — migrations used to run on the
    /// runtime pool, whose role does not bypass `FORCE ROW LEVEL SECURITY`,
    /// so the statement's source selected zero rows, the INSERT said
    /// nothing, and the migration was recorded as applied; those rows did
    /// survive indefinitely. No database ever got there:
    /// `0013`'s header records that none had it recorded as applied when
    /// #105 landed, and migrations now run on `MIGRATION_DATABASE_URL` under
    /// a pass that refuses to start unless its connection provably bypasses
    /// RLS (`apps/api/src/migrations.rs`).
    ///
    /// A third one was listed here until #106: the Google Calendar import
    /// inserted into `events` and never into `event_assignees`, which held on
    /// every deployment, before or after any backfill, and did not go away on
    /// its own. It now assigns the account that ran the sync and rewrites the
    /// rows it wrote before (`apps/api/src/google_calendar/imports.rs`).
    ///
    /// So nothing left above outlives a finished migration pass. It is still
    /// a list of what is *known*, not a proof, and the database carries no
    /// constraint of its own — hence "possibly empty".
    ///
    /// Claiming an invariant here that no write path enforces was what let
    /// the dashboard render a bare "?" ring on those rows (#99). Readers must
    /// handle the empty case: `web/routes/home.rs::row_assignee_ids` falls
    /// back to the creator, `web/routes/agenda/detail.rs::assignees_html`
    /// drops its line.
    pub assignee_ids: Vec<Uuid>,
}

/// One expanded occurrence in a `GET /groups/:id/events?from&to` window.
/// The backend flattens `EventResponse` into this shape and, for a
/// recurring task, overrides `completed_at` with *that occurrence's*
/// completion (from `event_occurrence_completions`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OccurrenceResponse {
    #[serde(flatten)]
    pub event: EventResponse,
    pub occurrence_starts_at: DateTime<Utc>,
    pub occurrence_ends_at: DateTime<Utc>,
}

/// `GET /groups/:id/events?from&to` response envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OccurrenceList {
    pub occurrences: Vec<OccurrenceResponse>,
}

/// `POST /groups/:id/events/:event_id/reminders` request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateReminderRequest {
    pub offset_minutes: i32,
}

/// `POST /groups/:id/events/:event_id/reminders` (201) response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReminderResponse {
    pub id: Uuid,
    pub event_id: Uuid,
    pub offset_minutes: i32,
}

/// One element of `GET /groups/:id/events/:event_id/attachments`, and the
/// `POST …/attachments` (201) response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentResponse {
    pub id: Uuid,
    pub event_id: Uuid,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: i64,
}
