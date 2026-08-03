//! `GET /agenda` — the hand-rolled month/week calendar (no calendar
//! library, the accepted Leptos trade-off from `architecture.md`). Events
//! and tasks-as-events render together; recurring series are expanded by
//! `GET /groups/:gid/events?from&to` so an occurrence appears in every
//! day-cell it falls on within the visible window. `?view=week` switches to
//! the 7-day view; `?date=YYYY-MM-DD` moves the focus.
//!
//! # One markup, two readings (#71)
//!
//! Seven columns on a 390px phone give ~50px a day, which no stylesheet
//! rescues, so DESIGN.md → Layout asks for a day view below 861px. This
//! route does **not** render one. It renders the grid, once, and
//! `style.css`'s `@media not all and (min-width: 861px)` block re-reads
//! those same cells as a list of the days that carry something.
//!
//! That is a deliberate trade, decided before the work started, and it is
//! worth stating what each side costs. The stylesheet is inlined into every
//! response and is bounded by two ceilings (`app.rs`, DESIGN.md → Livraison
//! du CSS); a reflow spends a handful of declarations there, **once**.
//! Rendering both a grid and a list and hiding one with CSS would instead
//! spend *document* bytes — paid on every page view, growing with how much
//! the family has stored, on `/agenda`, which is already the heaviest route
//! that still fits the response budget.
//!
//! What the trade concedes, so that nobody has to rediscover it:
//!
//! * The phone list is a projection of grid cells, not rows built for a
//!   phone. A day shows its number, not "lundi 15": the weekday lives in
//!   the table header, which the list hides, and putting it back in every
//!   cell is document bytes for a viewport half the visitors are not on.
//! * There is no `view=day`. The "Mois" button DESIGN.md describes would
//!   have nothing to switch back from, so it does not exist either; the
//!   month/week toggle keeps doing that job on both viewports.
//! * A member's colour on each occurrence is left to a later pass. WCAG
//!   1.4.1 forbids the hue carrying the information alone, so it needs the
//!   member's *name* next to it — a second API call and a per-chip cost on
//!   the document, i.e. exactly what this trade exists to avoid.
//!
//! The month view caps a crowded day at `MONTH_CHIP_CAP` and counts the
//! rest into a "+N autres" link to the week view, which caps nothing.

use std::collections::BTreeMap;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::{Datelike, NaiveDate};
use leptos::prelude::*;
use manage_our_home_shared::dto::agenda::{OccurrenceList, OccurrenceResponse};
use manage_our_home_shared::validation::agenda::{month_grid, week_days};

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{
    agenda_cookie, family_context, fmt_paris, paris_local_to_utc, service_unavailable_page,
    today_paris, DISPLAY_TZ,
};

const FR_MONTHS: [&str; 12] = [
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];
const FR_WEEKDAYS: [&str; 7] = ["Lun", "Mar", "Mer", "Jeu", "Ven", "Sam", "Dim"];

/// The most occurrences a month-grid cell lists before it stops listing and
/// starts counting. A cell is 6.5rem tall (104px) and that is what three
/// chips and a day number come to: 3 × (13px line + 2px padding × 2 + 2px
/// margin) + a 14px line + the cell's own 4px padding ≈ 105px.
///
/// The week view caps nothing — not because its cells are taller (since
/// #71 they are the same cells) but because there is only one row of them,
/// so a cell that grows past 6.5rem pushes nothing out of view. It is also
/// where the month grid's "+N autres" sends the reader, so capping there
/// would be a dead end.
const MONTH_CHIP_CAP: usize = 3;

#[derive(serde::Deserialize)]
pub struct CalendarQuery {
    view: Option<String>,
    date: Option<String>,
    notice: Option<String>,
}

fn notice_text(code: &str) -> Option<&'static str> {
    match code {
        "event_created" => Some("Événement créé."),
        "event_deleted" => Some("Événement supprimé."),
        _ => None,
    }
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CalendarQuery>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/agenda").await else {
        return Redirect::to("/groups/new").into_response();
    };

    let is_week = query.view.as_deref() == Some("week");
    let focus = query
        .date
        .as_deref()
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .unwrap_or_else(today_paris);

    // The civil dates the grid shows, and the [from, to] UTC window covering
    // them (Paris start-of-first-day .. Paris end-of-last-day).
    let days: Vec<NaiveDate> = if is_week {
        week_days(focus)
    } else {
        month_grid(focus.year(), focus.month())
    };
    let (Some(first), Some(last)) = (days.first(), days.last()) else {
        return service_unavailable_page().into_response();
    };
    let (Some(from), Some(to)) = (
        paris_local_to_utc(&format!("{first}T00:00")),
        paris_local_to_utc(&format!("{last}T23:59:59")),
    ) else {
        return service_unavailable_page().into_response();
    };

    let cookie = agenda_cookie(&headers);
    let path = format!(
        "/groups/{}/events?from={}&to={}",
        fam.gid,
        from.format("%Y-%m-%dT%H:%M:%SZ"),
        to.format("%Y-%m-%dT%H:%M:%SZ"),
    );
    let occurrences: Vec<OccurrenceResponse> = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &path,
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<OccurrenceList>(resp.body)
                .map(|l| l.occurrences)
                .unwrap_or_default()
        }
        // A 400 (impossible via the UI) or anything else: render an empty
        // calendar rather than leaking JSON.
        Ok(_) => Vec::new(),
        Err(_) => return service_unavailable_page().into_response(),
    };

    // Bucket each occurrence into its Paris civil date.
    let mut by_day: BTreeMap<NaiveDate, Vec<&OccurrenceResponse>> = BTreeMap::new();
    for occ in &occurrences {
        let day = occ
            .occurrence_starts_at
            .with_timezone(&DISPLAY_TZ)
            .date_naive();
        by_day.entry(day).or_default().push(occ);
    }
    for list in by_day.values_mut() {
        list.sort_by_key(|o| o.occurrence_starts_at);
    }

    let today = today_paris();
    // One grid for both views: the month shows six weeks of it, the week
    // one. What differs is how many days it is handed, whether any of them
    // is muted as belonging to another month, and whether a crowded cell
    // may stop listing and start counting — the week view is where the
    // month's "+N autres" sends the reader, so it caps nothing.
    let (focus_month, cap) = if is_week {
        (None, None)
    } else {
        (Some(focus.month()), Some(MONTH_CHIP_CAP))
    };
    let grid_html = render_grid(&days, &by_day, focus_month, today, cap);
    let nav_html = render_nav(is_week, focus);
    let notice_html = query
        .notice
        .as_deref()
        .and_then(notice_text)
        .map(|n| format!(r#"<p class="notice success">{}</p>"#, html_escape(n)))
        .unwrap_or_default();

    let body = view! {
        <div inner_html=nav_html></div>
        <div inner_html=notice_html></div>
        <div inner_html=grid_html></div>
    };
    Html(shell_with_header(
        Width::Full,
        "Agenda",
        &fam.header,
        &body.to_html(),
    ))
    .into_response()
}

/// Prev/next/today navigation, the month/week toggle, and the "new event"
/// button.
fn render_nav(is_week: bool, focus: NaiveDate) -> String {
    let (prev, next, title) = if is_week {
        (
            focus - chrono::Duration::days(7),
            focus + chrono::Duration::days(7),
            format!(
                "Semaine du {} {}",
                focus.day(),
                FR_MONTHS[(focus.month() - 1) as usize]
            ),
        )
    } else {
        let (py, pm) = if focus.month() == 1 {
            (focus.year() - 1, 12)
        } else {
            (focus.year(), focus.month() - 1)
        };
        let (ny, nm) = if focus.month() == 12 {
            (focus.year() + 1, 1)
        } else {
            (focus.year(), focus.month() + 1)
        };
        (
            NaiveDate::from_ymd_opt(py, pm, 1).unwrap(),
            NaiveDate::from_ymd_opt(ny, nm, 1).unwrap(),
            format!(
                "{} {}",
                FR_MONTHS[(focus.month() - 1) as usize],
                focus.year()
            ),
        )
    };
    let view_q = if is_week { "week" } else { "month" };
    let today = today_paris();
    format!(
        r#"<div class="page-header">
<h1>{title}</h1>
<span class="actions">
<a class="btn secondary" href="/agenda?view={view_q}&date={prev}">"◀"</a>
<a class="btn secondary" href="/agenda?view={view_q}&date={today}">Aujourd'hui</a>
<a class="btn secondary" href="/agenda?view={view_q}&date={next}">"▶"</a>
<a class="btn secondary" href="/agenda?view=month&date={focus}">Mois</a>
<a class="btn secondary" href="/agenda?view=week&date={focus}">Semaine</a>
<a class="btn secondary" href="/agenda/imports">Agendas Google</a>
<a class="btn" href="/agenda/new">Nouvel événement</a>
</span>
</div>"#,
        title = html_escape(&title),
        view_q = view_q,
        prev = prev,
        next = next,
        today = today,
        focus = focus,
    )
}

/// The calendar, as many whole weeks as `days` holds: six for the month
/// view, one for the week view.
///
/// The week used to be its own markup — a `flex` row of `.cal-col` divs that
/// asked for 8rem a column and wrapped into a vertical list as soon as the
/// container was narrower than 56rem (issue #71). Making it a one-row table
/// of the same cells costs nothing extra, gives it the same seven real
/// columns as the month, and — the reason it matters here — leaves the
/// stylesheet **one** calendar to lay out instead of two, which is what lets
/// the narrow-viewport reading of it be a handful of declarations.
///
/// `focus_month` is the month whose days are the point of the grid; the
/// others are muted as `outside`. The week view passes `None`: a month grid
/// carries days it did not ask for, because it is built of whole weeks, but
/// every day of a week view is the week the reader asked for — muting the
/// three that fall after the 31st would say something false about them.
fn render_grid(
    days: &[NaiveDate],
    by_day: &BTreeMap<NaiveDate, Vec<&OccurrenceResponse>>,
    focus_month: Option<u32>,
    today: NaiveDate,
    cap: Option<usize>,
) -> String {
    let headers: String = FR_WEEKDAYS
        .iter()
        .map(|d| format!(r#"<th>{d}</th>"#))
        .collect();

    let mut rows = String::new();
    for week in days.chunks(7) {
        rows.push_str("<tr>");
        for day in week {
            let in_focus = focus_month.is_none_or(|m| day.month() == m);
            let today_class = if *day == today { " current" } else { "" };
            let outside_class = if in_focus { "" } else { " outside" };
            let chips = render_chips(
                by_day.get(day).map(|v| v.as_slice()).unwrap_or(&[]),
                cap,
                *day,
            );
            rows.push_str(&format!(
                r#"<td class="cal-cell{today_class}">
<div class="cal-day{outside_class}">{num}</div>{chips}</td>"#,
                today_class = today_class,
                outside_class = outside_class,
                num = day.day(),
                chips = chips,
            ));
        }
        rows.push_str("</tr>");
    }
    format!(r#"<table class="cal"><thead><tr>{headers}</tr></thead><tbody>{rows}</tbody></table>"#,)
}

/// How a day's occurrences split between `(listed, counted)`: the chips the
/// cell shows, and the ones its "+N autres" indicator stands for. `cap` is
/// the rows the cell has room for, `None` for the week view.
///
/// When a day does not fit, the indicator takes one of those rows rather
/// than being added on top of them — a cell capped at three that listed
/// three chips *and* a "+1 autre" would be four rows tall, which is the
/// overflow the cap exists to prevent (issue #71: the fixed cell height
/// truncated crowded days with nothing to say so).
fn day_split(total: usize, cap: Option<usize>) -> (usize, usize) {
    match cap {
        Some(cap) if total > cap => {
            let listed = cap.saturating_sub(1);
            (listed, total - listed)
        }
        _ => (total, 0),
    }
}

/// The indicator's label, empty when nothing is hidden so the caller can
/// append it unconditionally. The plural agrees: "+1 autre", "+2 autres".
fn overflow_label(hidden: usize) -> String {
    match hidden {
        0 => String::new(),
        1 => "+1 autre".to_string(),
        n => format!("+{n} autres"),
    }
}

/// One day-cell's occurrence chips. Tasks get a ☑/☐ marker and a
/// strike-through when that occurrence is completed.
fn render_chips(occs: &[&OccurrenceResponse], cap: Option<usize>, day: NaiveDate) -> String {
    let (shown, hidden) = day_split(occs.len(), cap);
    let mut out: String = occs[..shown]
        .iter()
        .map(|occ| {
            let e = &occ.event;
            let time = if e.all_day {
                "journée".to_string()
            } else {
                fmt_paris(occ.occurrence_starts_at, "%H:%M")
            };
            let occ_param = occ.occurrence_starts_at.format("%Y-%m-%dT%H:%M:%SZ");
            let (marker, done) = if e.is_task {
                if e.completed_at.is_some() {
                    ("☑ ", " done")
                } else {
                    ("☐ ", "")
                }
            } else {
                ("", "")
            };
            format!(
                r#"<a href="/agenda/{id}?occ={occ}" class="chip{done}">{marker}<strong>{time}</strong> {title}</a>"#,
                id = e.id,
                occ = occ_param,
                done = done,
                marker = marker,
                time = html_escape(&time),
                title = html_escape(&e.title),
            )
        })
        .collect();
    if hidden > 0 {
        out.push_str(&format!(
            r#"<a href="/agenda?view=week&date={day}" class="chip">{label}</a>"#,
            day = day,
            label = html_escape(&overflow_label(hidden)),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use manage_our_home_shared::dto::agenda::EventResponse;
    use uuid::Uuid;

    // -- day_split -----------------------------------------------------
    //
    // A month cell is 6.5rem tall and fits three rows. What it does with a
    // fourth is the whole decision: listing three *and* an indicator is
    // four rows, i.e. the overflow the cap exists to prevent.

    #[test]
    fn a_day_that_fits_lists_everything_and_counts_nothing() {
        assert_eq!(day_split(3, Some(3)), (3, 0));
        assert_eq!(day_split(1, Some(3)), (1, 0));
        assert_eq!(day_split(0, Some(3)), (0, 0));
    }

    #[test]
    fn the_indicator_takes_one_of_the_capped_rows_not_an_extra_one() {
        // Four occurrences in a cell of three: two chips and "+2 autres",
        // which is three rows — not three chips and a fourth row.
        assert_eq!(day_split(4, Some(3)), (2, 2));
        assert_eq!(day_split(9, Some(3)), (2, 7));
        // A cap of one leaves no room for a chip at all.
        assert_eq!(day_split(2, Some(1)), (0, 2));
    }

    #[test]
    fn the_week_view_caps_nothing() {
        // It is one row of cells with nothing under it to push out of view,
        // and it is where the month grid's indicator sends the reader — an
        // indicator there would be a dead end.
        assert_eq!(day_split(9, None), (9, 0));
        assert_eq!(day_split(0, None), (0, 0));
    }

    // -- overflow_label ------------------------------------------------

    #[test]
    fn the_overflow_label_agrees_in_number() {
        assert_eq!(overflow_label(1), "+1 autre");
        assert_eq!(overflow_label(2), "+2 autres");
        assert_eq!(overflow_label(7), "+7 autres");
    }

    #[test]
    fn nothing_hidden_is_no_label_at_all() {
        // The caller concatenates unconditionally, so a "+0 autres" chip is
        // exactly what an empty string here prevents.
        assert_eq!(overflow_label(0), "");
    }

    // -- render_chips --------------------------------------------------

    fn day(n: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 7, n).unwrap()
    }

    /// One occurrence at `hour` on 2026-07-15, with a distinguishable title.
    fn occurrence(hour: u32, title: &str) -> OccurrenceResponse {
        let starts = Utc.with_ymd_and_hms(2026, 7, 15, hour, 0, 0).unwrap();
        OccurrenceResponse {
            event: EventResponse {
                id: Uuid::from_u128(u128::from(hour)),
                group_id: Uuid::nil(),
                created_by: Uuid::nil(),
                title: title.to_string(),
                description: None,
                location: None,
                starts_at: starts,
                ends_at: starts,
                all_day: false,
                is_task: false,
                completed_at: None,
                rrule: None,
            },
            occurrence_starts_at: starts,
            occurrence_ends_at: starts,
        }
    }

    #[test]
    fn a_crowded_month_cell_lists_the_cap_and_sends_the_rest_to_the_week() {
        let occs: Vec<OccurrenceResponse> = (0..5)
            .map(|i| occurrence(8 + i, &format!("Événement {i}")))
            .collect();
        let refs: Vec<&OccurrenceResponse> = occs.iter().collect();
        let html = render_chips(&refs, Some(MONTH_CHIP_CAP), day(15));
        assert!(html.contains("Événement 0"), "{html}");
        assert!(html.contains("Événement 1"), "{html}");
        assert!(!html.contains("Événement 2"), "{html}");
        assert!(html.contains("+3 autres"), "{html}");
        // The indicator is a destination, not a dead end: the week view of
        // that very day, which caps nothing.
        assert!(
            html.contains(r#"href="/agenda?view=week&date=2026-07-15""#),
            "{html}"
        );
    }

    #[test]
    fn an_uncrowded_month_cell_carries_no_indicator() {
        let occs = [occurrence(9, "Dentiste")];
        let refs: Vec<&OccurrenceResponse> = occs.iter().collect();
        let html = render_chips(&refs, Some(MONTH_CHIP_CAP), day(15));
        assert!(html.contains("Dentiste"), "{html}");
        assert!(!html.contains("autre"), "{html}");
        assert!(!html.contains("view=week"), "{html}");
    }

    #[test]
    fn the_week_view_renders_every_occurrence() {
        let occs: Vec<OccurrenceResponse> = (0..5)
            .map(|i| occurrence(8 + i, &format!("Événement {i}")))
            .collect();
        let refs: Vec<&OccurrenceResponse> = occs.iter().collect();
        let html = render_chips(&refs, None, day(15));
        for i in 0..5 {
            assert!(html.contains(&format!("Événement {i}")), "{html}");
        }
        assert!(!html.contains("autres"), "{html}");
    }

    // -- the markup the phone reading depends on -----------------------

    #[test]
    fn a_week_is_the_same_grid_as_a_month_one_row_shorter() {
        // The narrow-viewport block of style.css re-reads these very cells
        // as a list. Two markups for two views would mean writing that
        // block twice — and one of them silently not reflowing — which is
        // why the week view stopped being a `flex` row of its own divs.
        let occs = [occurrence(9, "Dentiste")];
        let by_day: BTreeMap<NaiveDate, Vec<&OccurrenceResponse>> =
            [(day(15), occs.iter().collect())].into_iter().collect();
        let week: Vec<NaiveDate> = (13..=19).map(day).collect();
        let html = render_grid(&week, &by_day, None, day(1), None);
        assert!(html.starts_with(r#"<table class="cal">"#), "{html}");
        // Seven weekday headers, and seven cells in one row: a day with
        // nothing on it still renders its cell, because that is what makes
        // the thing a grid. Dropping it is the phone reading's job.
        assert_eq!(html.matches("<th>").count(), 7, "{html}");
        assert_eq!(html.matches("<tr>").count(), 2, "{html}");
        assert_eq!(html.matches(r#"class="cal-cell"#).count(), 7, "{html}");
        assert!(html.contains(r#"class="chip"#), "{html}");
    }

    #[test]
    fn a_week_that_straddles_two_months_mutes_none_of_its_days() {
        // `outside` means "shown only because the month grid is made of
        // whole weeks". A week view has no such day: all seven are the week
        // the reader asked for, so muting the ones that happen to belong to
        // the next month says something false about them.
        let by_day: BTreeMap<NaiveDate, Vec<&OccurrenceResponse>> = BTreeMap::new();
        // 2026-07-27 (Monday) .. 2026-08-02: four July days, three August.
        let week: Vec<NaiveDate> = week_days(day(29));
        let html = render_grid(&week, &by_day, None, day(29), None);
        assert_eq!(html.matches("cal-day outside").count(), 0, "{html}");
    }

    #[test]
    fn the_month_grid_marks_today_and_the_days_of_the_neighbouring_months() {
        let by_day: BTreeMap<NaiveDate, Vec<&OccurrenceResponse>> = BTreeMap::new();
        let days: Vec<NaiveDate> = month_grid(2026, 7);
        let html = render_grid(&days, &by_day, Some(7), day(15), None);
        assert_eq!(html.matches("cal-cell current").count(), 1, "{html}");
        // July 2026 starts on a Wednesday: two June days lead the grid, and
        // the six weeks run into August.
        assert!(html.matches("cal-day outside").count() >= 2, "{html}");
    }
}
