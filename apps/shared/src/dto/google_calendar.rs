//! Request/response shapes for the Google Calendar import endpoints
//! (`apps/api/src/google_calendar/imports.rs`), shared with `apps/web`'s SSR
//! client (front epic F11, issue #52). Mirrors — rather than moves — the
//! backend's local structs, the same convention `user_admin.rs` uses: the
//! backend shipped with epic #9 and this epic changes no API surface.
//!
//! **The feed URL is write-only.** `CreateCalendarImportRequest` carries it in;
//! `CalendarImportResponse` deliberately has no `feed_url` field because the API
//! never returns one — it is a bearer credential for a member's Google calendar
//! (see `imports.rs`'s doc comment, same principle as never returning a password
//! hash). There is no update request type either: the backend exposes no
//! `PATCH`, so changing a label or a URL means delete + recreate.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Body of `POST /groups/:gid/calendar-imports`.
///
/// `feed_url` is the private "Adresse secrète au format iCal" of one Google
/// calendar. It is submitted once and never echoed back: `apps/web` must keep it
/// out of query strings, PRG parameters, re-rendered form values and logs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateCalendarImportRequest {
    pub label: String,
    pub feed_url: String,
}

/// One connected calendar, as returned by `POST`/`GET
/// /groups/:gid/calendar-imports`. `last_imported_at` is `None` until the first
/// successful run of `POST …/:import_id/import` — v1 is pull-on-demand only, so
/// a freshly created connection has imported nothing yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarImportResponse {
    pub id: Uuid,
    pub group_id: Uuid,
    pub created_by: Uuid,
    pub label: String,
    pub last_imported_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// `GET /groups/:gid/calendar-imports` response envelope — the backend wraps the
/// list in a single-key object with `serde_json::json!`, same shape as
/// `user_admin.rs`'s `{"users": […]}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarImportsResponse {
    pub imports: Vec<CalendarImportResponse>,
}

/// `POST /groups/:gid/calendar-imports/:import_id/import` result: what one
/// on-demand run did. `skipped` counts VEVENTs whose upstream version was
/// unchanged since the last run — a re-import of an untouched feed reports
/// everything as skipped, which is the user-visible face of the backend's
/// UID-keyed idempotence.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ImportRunResponse {
    pub imported: usize,
    pub updated: usize,
    pub skipped: usize,
}
