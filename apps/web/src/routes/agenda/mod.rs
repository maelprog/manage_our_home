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
//! into calendar day-cells by its Paris civil date. An all-day event's
//! `date` fields are Paris civil days too, its end inclusive (`form_bounds`).

pub mod attachments;
pub mod calendar;
pub mod detail;
pub mod edit;
pub mod imports;
pub mod new;
pub mod reminders;

use axum::http::HeaderMap;
use axum::response::Html;
use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Europe::Paris;
use chrono_tz::Tz;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::GroupSummary;
use manage_our_home_shared::validation::agenda::{normalize_all_day, paris_start_of_day};
use uuid::Uuid;

use crate::app::{shell, Width};
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

/// A browser `<input type="date">` value (`YYYY-MM-DD`), or `None` for
/// anything else — a `datetime-local` value included.
pub(crate) fn date_only(input: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(input, "%Y-%m-%d").ok()
}

/// The UTC bounds a submitted `Début`/`Fin` pair stands for.
///
/// Each field is read by its own shape, not by the "Journée entière" box
/// (#117). An all-day event is edited through two `<input type="date">`
/// fields, and **Fin names the last day the event covers, inclusively** —
/// what a reader expects, where the stored `ends_at` is the exclusive Paris
/// midnight opening the next day (`normalize_all_day`). So a date start is
/// the midnight opening its day, and a date end is the midnight opening the
/// day *after* it. The previous form showed that exclusive midnight in a
/// `datetime-local` field, and a user "correcting" `08 00:00` to `07 00:00`
/// lost a day without a word.
///
/// A `datetime-local` value keeps its wall-clock reading, as before: it is
/// what timed events submit, and what an all-day event submits when the box
/// was ticked without JS to swap the fields (the API then normalizes it).
///
/// `None` when a field is unreadable, or when either field is a date and the
/// end does not land strictly after the start. A date end naming the day
/// before a date start converts to an exclusive end *equal* to the start —
/// and so does a forged mixed pair such as Début = `07`, Fin = `07T00:00`.
/// `validate_event_form` accepts that zero-length span and the API would
/// silently widen it to one day. A pair of two timed fields keeps its
/// ordering check in `validate_event_form`, zero length included, as before.
pub(crate) fn form_bounds(starts: &str, ends: &str) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let starts_at = match date_only(starts) {
        Some(day) => paris_start_of_day(day),
        None => paris_local_to_utc(starts)?,
    };
    let ends_at = match date_only(ends) {
        Some(last_day) => paris_start_of_day(last_day.succ_opt()?),
        None => paris_local_to_utc(ends)?,
    };
    let has_date_field = date_only(starts).is_some() || date_only(ends).is_some();
    if has_date_field && ends_at <= starts_at {
        return None;
    }
    Some((starts_at, ends_at))
}

/// The two `<input type="date">` values an `all_day` event is edited with:
/// its first day and its **last** day, inclusive — the inverse of
/// `form_bounds` on date fields (#117).
///
/// Read through `normalize_all_day` first, so a row whose bounds were never
/// normalized (stored before #101) shows the days it will cover once saved,
/// and the exclusive end always sits on a midnight a day or more past the
/// start — its day before is then never earlier than the first day.
pub(crate) fn all_day_field_values(
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
) -> (String, String) {
    let (starts_at, exclusive_end) = normalize_all_day(starts_at, ends_at);
    let first_day = starts_at.with_timezone(&DISPLAY_TZ).date_naive();
    let exclusive_end_day = exclusive_end.with_timezone(&DISPLAY_TZ).date_naive();
    let last_day = exclusive_end_day.pred_opt().unwrap_or(exclusive_end_day);
    (
        first_day.format("%Y-%m-%d").to_string(),
        last_day.format("%Y-%m-%d").to_string(),
    )
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
        <a class="btn secondary" href="/agenda">"Retour à l'agenda"</a>
    };
    Html(shell(Width::Form, "Événement introuvable", &body.to_html()))
}

pub(crate) fn service_unavailable_page() -> Html<String> {
    let body = view! {
        <h1>"Service momentanément indisponible"</h1>
        <p>"Merci de réessayer dans quelques instants."</p>
        <a class="btn secondary" href="/agenda">"Retour à l'agenda"</a>
    };
    Html(shell(Width::Form, "Service indisponible", &body.to_html()))
}

pub(crate) fn forbidden_page() -> Html<String> {
    let body = view! {
        <h1>"Action non autorisée"</h1>
        <p>"Vous n'avez pas les droits nécessaires sur cet événement."</p>
        <a class="btn secondary" href="/agenda">"Retour à l'agenda"</a>
    };
    Html(shell(Width::Form, "Action non autorisée", &body.to_html()))
}

/// Fetches `cookie_of` for an Agenda handler (re-exported for the submodules
/// so they don't each reach into `routes::groups`).
pub(crate) fn agenda_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_of(headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use manage_our_home_shared::validation::agenda::{validate_event_form, EventFormError};

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn days(start: &str, end: &str) -> (String, String) {
        (start.to_string(), end.to_string())
    }

    // -- date_only -----------------------------------------------------------

    #[test]
    fn a_date_field_value_is_recognised_and_a_datetime_one_is_not() {
        assert_eq!(date_only("2026-09-05"), NaiveDate::from_ymd_opt(2026, 9, 5));
        assert_eq!(date_only("2026-09-05T10:00"), None);
        assert_eq!(date_only("2026-09-05T00:00"), None);
        assert_eq!(date_only(""), None);
        assert_eq!(date_only("2026-13-01"), None);
    }

    // -- form_bounds: date fields --------------------------------------------

    /// The issue's own example: an event covering the 5th, 6th and 7th is
    /// entered as Début = 05, Fin = 07, and stored with its exclusive end,
    /// the Paris midnight opening the 8th (CEST, UTC+2).
    #[test]
    fn an_inclusive_end_date_is_stored_as_the_midnight_after_it() {
        assert_eq!(
            form_bounds("2026-09-05", "2026-09-07"),
            Some((utc(2026, 9, 4, 22, 0), utc(2026, 9, 7, 22, 0)))
        );
    }

    #[test]
    fn the_same_date_twice_covers_exactly_one_day() {
        let (s, e) = form_bounds("2026-09-05", "2026-09-05").unwrap();
        assert_eq!(s, utc(2026, 9, 4, 22, 0));
        assert_eq!(e - s, chrono::Duration::hours(24));
    }

    /// Winter dates anchor on CET midnight, not on the summer offset.
    #[test]
    fn a_winter_date_anchors_on_cet_midnight() {
        assert_eq!(
            form_bounds("2026-01-10", "2026-01-10"),
            Some((utc(2026, 1, 9, 23, 0), utc(2026, 1, 10, 23, 0)))
        );
    }

    /// Whatever the date fields produce must already be the API's fixed
    /// point, across both DST changes too: `update_event` re-normalizes on
    /// every save, and `normalize_all_day` reads an end sitting on Paris
    /// midnight as exclusive — the very reading #117 is about.
    #[test]
    fn date_bounds_are_already_what_the_api_normalizes_to() {
        for (start, end) in [
            ("2026-09-05", "2026-09-05"),
            ("2026-09-05", "2026-09-07"),
            ("2026-03-29", "2026-03-29"),
            ("2026-10-25", "2026-10-25"),
            ("2026-03-28", "2026-03-30"),
            ("2026-12-31", "2027-01-01"),
        ] {
            let (s, e) = form_bounds(start, end).unwrap();
            assert_eq!(normalize_all_day(s, e), (s, e), "{start} → {end}");
        }
    }

    /// The shared pre-validation still runs on the converted bounds; it must
    /// accept every well-ordered date pair, a single day included.
    #[test]
    fn the_shared_validation_accepts_well_ordered_dates() {
        for (start, end) in [("2026-09-05", "2026-09-05"), ("2026-09-05", "2026-09-07")] {
            let (s, e) = form_bounds(start, end).unwrap();
            assert_eq!(validate_event_form("Anniversaire", s, e), Ok(()));
        }
    }

    /// Fin the day before Début converts to an exclusive end *equal* to the
    /// start, which `validate_event_form` (`ends_at >= starts_at`) lets
    /// through and the API would silently widen to one day. A reversed date
    /// pair is refused here instead, so the form can say so.
    #[test]
    fn an_end_date_before_the_start_date_is_refused() {
        assert_eq!(form_bounds("2026-09-07", "2026-09-06"), None);
        assert_eq!(form_bounds("2026-09-07", "2026-09-05"), None);
    }

    // -- form_bounds: datetime-local fields ----------------------------------

    /// Timed fields keep their wall-clock reading, unchanged by #117 — and so
    /// does the JS-less path that ticks "Journée entière" while the fields
    /// are still `datetime-local`: the API normalizes those as before.
    #[test]
    fn datetime_values_keep_their_paris_wall_clock_reading() {
        assert_eq!(
            form_bounds("2026-09-05T10:00", "2026-09-05T11:30"),
            Some((utc(2026, 9, 5, 8, 0), utc(2026, 9, 5, 9, 30)))
        );
        // Ordering of timed fields stays `validate_event_form`'s call.
        assert_eq!(
            form_bounds("2026-09-05T10:00", "2026-09-05T09:00"),
            Some((utc(2026, 9, 5, 8, 0), utc(2026, 9, 5, 7, 0)))
        );
    }

    #[test]
    fn an_unreadable_field_is_refused() {
        assert_eq!(form_bounds("", "2026-09-05"), None);
        assert_eq!(form_bounds("2026-09-05", ""), None);
        assert_eq!(form_bounds("2026-09-05T10:00", "demain"), None);
    }

    /// Two timed fields out of order are still left to the shared
    /// validation, which reports them.
    #[test]
    fn a_reversed_timed_pair_still_goes_through_the_shared_validation() {
        let (s, e) = form_bounds("2026-09-07T10:00", "2026-09-07T09:00").unwrap();
        assert_eq!(
            validate_event_form("x", s, e),
            Err(EventFormError::EndsBeforeStarts)
        );
    }

    /// The same widening through a mixed pair (a forged POST): a date field
    /// on either side and an end landing exactly on the start converts to a
    /// zero-length span, which `validate_event_form` accepts and the API
    /// would widen to one day. As soon as a field is a date, the end must
    /// be strictly after the start.
    #[test]
    fn a_mixed_pair_ending_on_its_start_is_refused() {
        // Début = 07 (date), Fin = 07 00:00.
        assert_eq!(form_bounds("2026-09-07", "2026-09-07T00:00"), None);
        // Début = 07 00:00, Fin = 06 (date, i.e. up to 07 00:00).
        assert_eq!(form_bounds("2026-09-07T00:00", "2026-09-06"), None);
        // And a mixed pair plainly out of order.
        assert_eq!(form_bounds("2026-09-07T10:00", "2026-09-06"), None);
    }

    /// A mixed pair that does cover time is still read, not refused.
    #[test]
    fn a_well_ordered_mixed_pair_is_still_read() {
        assert_eq!(
            form_bounds("2026-09-07", "2026-09-07T10:00"),
            Some((utc(2026, 9, 6, 22, 0), utc(2026, 9, 7, 8, 0)))
        );
        assert_eq!(
            form_bounds("2026-09-07T10:00", "2026-09-07"),
            Some((utc(2026, 9, 7, 8, 0), utc(2026, 9, 7, 22, 0)))
        );
    }

    // -- all_day_field_values ------------------------------------------------

    /// #117 itself: stored 5th 00:00 → 8th 00:00 (exclusive) reads back as
    /// Fin = 07, the last day the event covers, not the 8th.
    #[test]
    fn the_edit_form_shows_the_last_day_not_the_exclusive_end() {
        assert_eq!(
            all_day_field_values(utc(2026, 9, 4, 22, 0), utc(2026, 9, 7, 22, 0)),
            days("2026-09-05", "2026-09-07")
        );
    }

    #[test]
    fn a_one_day_event_shows_the_same_date_twice() {
        assert_eq!(
            all_day_field_values(utc(2026, 9, 4, 22, 0), utc(2026, 9, 5, 22, 0)),
            days("2026-09-05", "2026-09-05")
        );
    }

    /// A row stored before #101 (or by a client that skipped the API's
    /// normalization) shows the days it will cover once saved, not a day
    /// early or late.
    #[test]
    fn unnormalized_bounds_show_the_days_they_cover() {
        // 08:00 → 09:00 Paris on the 5th.
        assert_eq!(
            all_day_field_values(utc(2026, 9, 5, 6, 0), utc(2026, 9, 5, 7, 0)),
            days("2026-09-05", "2026-09-05")
        );
        // 08:00 on the 5th → 10:00 on the 7th.
        assert_eq!(
            all_day_field_values(utc(2026, 9, 5, 6, 0), utc(2026, 9, 7, 8, 0)),
            days("2026-09-05", "2026-09-07")
        );
    }

    /// Opening the edit form and saving it untouched gives back exactly the
    /// stored bounds — the round trip whose failure lost #117's day.
    #[test]
    fn showing_then_submitting_unchanged_is_a_fixed_point() {
        for (s, e) in [
            (utc(2026, 9, 4, 22, 0), utc(2026, 9, 5, 22, 0)),
            (utc(2026, 9, 4, 22, 0), utc(2026, 9, 7, 22, 0)),
            // Spring-forward day (23 h) and fall-back day (25 h).
            (utc(2026, 3, 28, 23, 0), utc(2026, 3, 29, 22, 0)),
            (utc(2026, 10, 24, 22, 0), utc(2026, 10, 25, 23, 0)),
            // A span crossing New Year, in CET.
            (utc(2026, 12, 30, 23, 0), utc(2027, 1, 1, 23, 0)),
        ] {
            let (start, end) = all_day_field_values(s, e);
            assert_eq!(form_bounds(&start, &end), Some((s, e)), "{start} → {end}");
        }
    }
}
