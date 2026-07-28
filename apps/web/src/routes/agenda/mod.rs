//! Agenda screens (front epic #3, issue #18): a hand-rolled month/week
//! calendar, event/task creation, detail with per-occurrence completion,
//! edit/delete, reminders, and file attachments — plus the Google Calendar
//! import screens (`imports`, front epic F11/#52), which live here rather than
//! under `/groups/:id/settings` because what they produce is agenda data. Same
//! SSR pattern as
//! `routes/groups/*` — plain `<form method=post>` submissions, per-page
//! error tables mapping `apps/api/src/agenda/`'s exact status/error codes
//! to French copy, PRG (`?notice=`/`?error=` codes) after every mutation.
//!
//! Timezone (v1 decision, epic spec on #18): the backend stores/expands in
//! UTC; these pages display and accept input in **Europe/Paris** — one
//! fixed family timezone for v1 (no per-user tz). All naive `datetime-local`
//! input is interpreted as Paris and converted to UTC before hitting
//! apps/api; every UTC instant coming back is rendered in Paris and bucketed
//! into calendar day-cells by its Paris civil date.

pub mod attachments;
pub mod calendar;
pub mod detail;
pub mod edit;
pub mod imports;
pub mod new;
pub mod reminders;

use axum::http::HeaderMap;
use axum::response::Html;
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Europe::Paris;
use chrono_tz::Tz;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::GroupSummary;
use uuid::Uuid;

use crate::app::shell;
use crate::family::{active_group_id_from_headers, resolve_active_group};
use crate::routes::groups::{cookie_of, header_with_groups};
use crate::state::AppState;

/// Resolved active-family context for an Agenda page: the family id every
/// `/groups/:gid/…` API call is scoped to, plus the shared authenticated
/// header (nav + family switcher, #17). `None` means the user has no group
/// yet — callers redirect to `/groups/new`.
pub(crate) struct FamilyContext {
    pub gid: Uuid,
    /// The caller's role in the active family (`owner`/`admin`/`standard`),
    /// used to gate edit/delete controls (`can_modify`).
    pub role: String,
    pub header: String,
}

pub(crate) async fn family_context(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    redirect_to: &str,
) -> Option<FamilyContext> {
    let (groups, header) = header_with_groups(state, headers, me, redirect_to).await;
    let preferred = active_group_id_from_headers(headers);
    let active: Option<&GroupSummary> = resolve_active_group(&groups, preferred);
    active.map(|g| FamilyContext {
        gid: g.group_id,
        role: g.role.clone(),
        header,
    })
}

/// Mirror of `apps/api/src/agenda/mod.rs::can_modify`: the event's creator,
/// or a group owner/admin, may edit or delete it. Used to decide whether to
/// render those controls (the backend still 403s a forged request).
pub(crate) fn can_modify(role: &str, is_creator: bool) -> bool {
    is_creator || role == "owner" || role == "admin"
}

// -- timezone helpers (Europe/Paris, v1 fixed) ------------------------------

pub(crate) const DISPLAY_TZ: Tz = Paris;

/// Interprets a browser `datetime-local` value (`YYYY-MM-DDTHH:MM`, no zone)
/// as an Europe/Paris wall-clock time and converts it to a UTC instant.
/// Returns `None` on a malformed string; DST-ambiguous/skipped local times
/// resolve to the earliest valid instant rather than failing.
pub(crate) fn paris_local_to_utc(input: &str) -> Option<DateTime<Utc>> {
    let naive = NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M")
        .or_else(|_| NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S"))
        .ok()?;
    let local = DISPLAY_TZ
        .from_local_datetime(&naive)
        .earliest()
        .or_else(|| DISPLAY_TZ.from_local_datetime(&naive).latest())?;
    Some(local.with_timezone(&Utc))
}

/// Formats a UTC instant in Europe/Paris with the given `chrono` format.
pub(crate) fn fmt_paris(dt: DateTime<Utc>, fmt: &str) -> String {
    dt.with_timezone(&DISPLAY_TZ).format(fmt).to_string()
}

/// UTC instant → the `datetime-local` string a form control pre-fills with.
pub(crate) fn to_datetime_local(dt: DateTime<Utc>) -> String {
    fmt_paris(dt, "%Y-%m-%dT%H:%M")
}

/// "Now" as a Paris civil date — the calendar's default focus and its
/// "aujourd'hui" reference.
pub(crate) fn today_paris() -> chrono::NaiveDate {
    Utc::now().with_timezone(&DISPLAY_TZ).date_naive()
}

// -- shared error/landing pages ---------------------------------------------

pub(crate) fn event_not_found_page() -> Html<String> {
    let body = view! {
        <h1>"Événement introuvable"</h1>
        <p>"Cet événement n'existe pas ou vous n'y avez pas accès."</p>
        <a class="button secondary" href="/agenda">"Retour à l'agenda"</a>
    };
    Html(shell("Événement introuvable", &body.to_html()))
}

pub(crate) fn service_unavailable_page() -> Html<String> {
    let body = view! {
        <h1>"Service momentanément indisponible"</h1>
        <p>"Merci de réessayer dans quelques instants."</p>
        <a class="button secondary" href="/agenda">"Retour à l'agenda"</a>
    };
    Html(shell("Service indisponible", &body.to_html()))
}

pub(crate) fn forbidden_page() -> Html<String> {
    let body = view! {
        <h1>"Action non autorisée"</h1>
        <p>"Vous n'avez pas les droits nécessaires sur cet événement."</p>
        <a class="button secondary" href="/agenda">"Retour à l'agenda"</a>
    };
    Html(shell("Action non autorisée", &body.to_html()))
}

/// Fetches `cookie_of` for an Agenda handler (re-exported for the submodules
/// so they don't each reach into `routes::groups`).
pub(crate) fn agenda_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_of(headers)
}
