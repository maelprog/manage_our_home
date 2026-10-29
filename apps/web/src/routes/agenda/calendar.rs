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
//!
//! The third concession of #71 — no member colour on an occurrence — was
//! taken back by #525, at the price #71 named: a second API call (the
//! family's roster, read alongside the events) and a per-chip cost on the
//! document. Each chip wears its member's tint as a class (`member_tint`,
//! no inline style) *and* their initials, since WCAG 1.4.1 forbids the hue
//! carrying the information alone.
//!
//! The month view caps a crowded day at `MONTH_CHIP_CAP` and counts the
//! rest into a "+N autres" link to the week view, which caps nothing.
//!
//! # The mock-up's head and day panel (#525)
//!
//! `‹` · « Aujourd'hui » · `›` step by the view on screen, and the
//! Semaine / Mois toggle sits beside them, on a phone as on a PC. A PC adds
//! the panel of the day in focus (`day_panel`, `.pc`): the day `?date=`
//! names, today when it names none.

use std::collections::BTreeMap;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::{Datelike, NaiveDate};
use leptos::prelude::*;
use manage_our_home_shared::dto::agenda::OccurrenceResponse;
use manage_our_home_shared::dto::groups::GroupMember;
use manage_our_home_shared::validation::agenda::{month_grid, week_days};

use crate::app::{html_escape, member_avatar, member_index, shell_with_header, Width};
use crate::icons::{icon, icon_link, Icon};
use crate::layout::CurrentUser;
use crate::routes::groups::members::fetch_group_detail;
use crate::state::{api_request_auth, AppState};

use super::{
    agenda_cookie, family_context, fmt_paris, occurrences_from_response, paris_local_to_utc,
    read_or_log, service_unavailable_page, today_paris, DISPLAY_TZ,
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
pub(crate) const FR_WEEKDAYS: [&str; 7] = ["Lun", "Mar", "Mer", "Jeu", "Ven", "Sam", "Dim"];

/// The classes that tint a chip in a member's hue (#525): `.m1` to `.m8`,
/// each 16 % of its ramp token mixed into `--surface`, the event ground of
/// DESIGN.md → Couleur. Written out, in the order of the ramp, because the
/// stylesheet's guards look for a class in the sources by its name.
const MEMBER_TINTS: [&str; 8] = ["m1", "m2", "m3", "m4", "m5", "m6", "m7", "m8"];

/// The tint class of a member's chips: the hue of their `app::member_colour`,
/// as a class rather than an inline style — a chip is drawn once per
/// occurrence, and its hue is the one thing it would otherwise need inline.
pub(crate) fn member_tint(user_id: uuid::Uuid) -> &'static str {
    MEMBER_TINTS[member_index(user_id)]
}

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
pub(crate) const MONTH_CHIP_CAP: usize = 3;

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

/// The notices above the grid: the outcome of the action that led here
/// (`?notice=`), then, when the events could not be read (`load_error`),
/// the error that keeps an empty grid from reading as "nothing planned"
/// (#455).
fn notices_html(notice: Option<&str>, load_error: bool) -> String {
    let success = notice
        .and_then(notice_text)
        .map(|n| format!(r#"<p class="notice success">{}</p>"#, html_escape(n)))
        .unwrap_or_default();
    let error = if load_error {
        r#"<p class="notice error">Impossible de charger les événements pour le moment.</p>"#
    } else {
        ""
    };
    success + error
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
    // An answer we cannot read (a 400, impossible via the UI, a 500, or a
    // body that is not an occurrence list) still renders the grid, so the
    // navigation stays usable, but under an error notice: an empty grid alone
    // would say "nothing planned" (#455).
    //
    // The roster is read alongside, for the chips' tints and initials
    // (#525). It is decoration: a roster that cannot be read leaves every
    // chip neutral rather than taking the page down.
    let (events, roster) = tokio::join!(
        api_request_auth(&state, reqwest::Method::GET, &path, cookie.as_deref(), None,),
        fetch_group_detail(&state, cookie.as_deref(), fam.gid),
    );
    let members: Vec<GroupMember> = roster.ok().flatten().map(|g| g.members).unwrap_or_default();
    let (occurrences, load_error): (Vec<OccurrenceResponse>, bool) = match events {
        Ok(resp) => match read_or_log(
            occurrences_from_response(resp.status, resp.body),
            fam.gid,
            "agenda",
        ) {
            Some(occurrences) => (occurrences, false),
            None => (Vec::new(), true),
        },
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
    // The page's `<h1>` and the grid's `<caption>` (#145) say the same
    // thing, so they are computed once.
    let title = view_title(is_week, focus);
    let grid_html = render_grid(&days, &by_day, focus_month, today, cap, &title, &members);
    let panel_html = day_panel(
        focus,
        by_day.get(&focus).map(|v| v.as_slice()).unwrap_or(&[]),
        &members,
    );
    let nav_html = render_nav(is_week, focus, &title);
    let notice_html = notices_html(query.notice.as_deref(), load_error);

    let body = view! {
        <div inner_html=nav_html></div>
        <div inner_html=notice_html></div>
        <div class="agenda" inner_html=grid_html + &panel_html></div>
    };
    Html(shell_with_header(
        Width::Full,
        "Agenda",
        &fam.header,
        &body.to_html(),
    ))
    .into_response()
}

/// What the view is called: "juillet 2026" for a month, "Semaine du 13
/// juillet" for a week. Unescaped — each caller escapes it where it lands.
///
/// A week is named by its Monday, whichever of its days is in focus: the
/// arrows and « Aujourd'hui » keep the day they were given, and naming the
/// week after a Wednesday said it started that day.
fn view_title(is_week: bool, focus: NaiveDate) -> String {
    if is_week {
        let monday = week_days(focus)[0];
        let month = FR_MONTHS[(monday.month() - 1) as usize];
        format!("Semaine du {} {month}", monday.day())
    } else {
        let month = FR_MONTHS[(focus.month() - 1) as usize];
        format!("{month} {}", focus.year())
    }
}

/// The head of the page (#525): the view's name, `‹` · « Aujourd'hui » ·
/// `›` stepping by that view, the Semaine / Mois toggle — on a phone as on
/// a PC — then the imports, and a new event: the page's button on a PC, the
/// floating "+" of the home page on a phone (`.fab`, #524).
///
/// The toggle is a `<nav>` of two links, `.segment` like the kitchen's
/// (#523); the view on screen carries `aria-current`, so its position is
/// said in markup and not only drawn.
fn render_nav(is_week: bool, focus: NaiveDate, title: &str) -> String {
    let (prev, next) = if is_week {
        (
            focus - chrono::Duration::days(7),
            focus + chrono::Duration::days(7),
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
        )
    };
    let view_q = if is_week { "week" } else { "month" };
    // The arrows are an icon alone: their accessible name is what they step
    // to, which is a week in the week view, not a month.
    let (prev_label, next_label) = if is_week {
        ("Semaine précédente", "Semaine suivante")
    } else {
        ("Mois précédent", "Mois suivant")
    };
    let arrow = |date: NaiveDate, label: &str, icon: Icon| {
        icon_link(
            "btn secondary",
            &format!("/agenda?view={view_q}&date={date}"),
            label,
            icon,
        )
    };
    let prev_link = arrow(prev, prev_label, Icon::Previous);
    let next_link = arrow(next, next_label, Icon::Next);
    let today = today_paris();
    let view_link = |view: &str, label: &str| {
        let current = if view == view_q {
            r#" aria-current="true""#
        } else {
            ""
        };
        format!(r#"<a href="/agenda?view={view}&date={focus}"{current}>{label}</a>"#)
    };
    format!(
        r#"<div class="page-header">
<h1>{title}</h1>
<span class="actions">
<span class="actions">{prev_link}<a class="btn secondary" href="/agenda?view={view_q}&date={today}">Aujourd'hui</a>{next_link}</span>
<nav class="segment views" aria-label="Affichage">{week}{month}</nav>
<a class="btn secondary" href="/agenda/imports">Agendas Google</a>
<a class="btn pc" href="/agenda/new">{plus} Nouvel événement</a>
</span>
</div>
{fab}"#,
        title = html_escape(title),
        week = view_link("week", "Semaine"),
        month = view_link("month", "Mois"),
        plus = icon(Icon::Plus),
        fab = icon_link(
            "btn fab phone",
            "/agenda/new",
            "Ajouter un événement",
            Icon::Plus
        ),
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
    caption: &str,
    members: &[GroupMember],
) -> String {
    let headers: String = FR_WEEKDAYS
        .iter()
        .map(|d| format!(r#"<th scope="col">{d}</th>"#))
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
                members,
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
    // The caption (#145) names the table itself — the `<h1>` above it is a
    // neighbour, which table navigation does not announce. The sheet keeps
    // it out of sight, since that heading already shows it.
    format!(
        r#"<table class="cal"><caption>{caption}</caption><thead><tr>{headers}</tr></thead><tbody>{rows}</tbody></table>"#,
        caption = html_escape(caption),
    )
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

/// The members an occurrence belongs to, in the order the API lists its
/// assignees: the creator when it lists none (`EventResponse::assignee_ids`
/// can arrive empty, see `home.rs::row_assignee_ids`). An id the roster does
/// not know — a member who left, a roster that could not be read — names
/// nobody and is dropped.
fn people<'a>(occ: &OccurrenceResponse, members: &'a [GroupMember]) -> Vec<&'a GroupMember> {
    let e = &occ.event;
    let ids = if e.assignee_ids.is_empty() {
        std::slice::from_ref(&e.created_by)
    } else {
        e.assignee_ids.as_slice()
    };
    ids.iter()
        .filter_map(|id| members.iter().find(|m| m.user_id == *id))
        .collect()
}

/// Their initials, each the member's `.avatar` (`aria-hidden`: the chip is
/// named by its time and title, and the detail page names the people).
fn initials(people: &[&GroupMember]) -> String {
    people
        .iter()
        .map(|m| member_avatar(m.user_id, &m.display_name, None))
        .collect()
}

/// One day-cell's occurrence chips. Tasks get a ☑/☐ marker and a
/// strike-through when that occurrence is completed.
///
/// A chip wears its first member's tint (#525, `member_tint`) and the
/// initials of every member it belongs to: the hue is never alone (WCAG
/// 1.4.1), so a chip that names nobody stays neutral. A done task keeps its
/// initials and loses its tint: `--muted`, the colour of what is done, falls
/// under AA on the darker tints of the dark theme.
pub(crate) fn render_chips(
    occs: &[&OccurrenceResponse],
    cap: Option<usize>,
    day: NaiveDate,
    members: &[GroupMember],
) -> String {
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
            let people = people(occ, members);
            let tint = match people.first() {
                Some(first) if done.is_empty() => format!(" {}", member_tint(first.user_id)),
                _ => String::new(),
            };
            format!(
                r#"<a href="/agenda/{id}?occ={occ}" class="chip{done}{tint}">{initials}{marker}<strong>{time}</strong> {title}</a>"#,
                id = e.id,
                occ = occ_param,
                done = done,
                initials = initials(&people),
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

/// "Mardi 14 juillet": the day panel's heading.
fn long_day(day: NaiveDate) -> String {
    const WEEKDAYS: [&str; 7] = [
        "Lundi", "Mardi", "Mercredi", "Jeudi", "Vendredi", "Samedi", "Dimanche",
    ];
    format!(
        "{} {} {}",
        WEEKDAYS[day.weekday().num_days_from_monday() as usize],
        day.day(),
        FR_MONTHS[(day.month() - 1) as usize],
    )
}

/// The panel beside the grid on a PC (#525, `.pc`): the day in focus, one
/// row per occurrence — its time in the home page's pill (`.when`), the
/// title over the names of the people, their initials at the end.
///
/// The rows are not links. The chip of the same occurrence is one, right
/// beside it in the grid: a second link would be a second tab stop and a
/// second "Dentiste" for whoever looks the event up by name.
fn day_panel(day: NaiveDate, occs: &[&OccurrenceResponse], members: &[GroupMember]) -> String {
    let body = if occs.is_empty() {
        r#"<p class="muted">Rien de prévu ce jour-là.</p>"#.to_string()
    } else {
        let rows: String = occs
            .iter()
            .map(|occ| {
                let when = if occ.event.all_day {
                    "Journée".to_string()
                } else {
                    fmt_paris(occ.occurrence_starts_at, "%H:%M")
                };
                let people = people(occ, members);
                let names: Vec<&str> = people.iter().map(|m| m.display_name.as_str()).collect();
                format!(
                    r#"<li class="list-row"><span class="when">{when}</span><span class="what"><strong>{title}</strong><span class="muted">{names}</span></span>{initials}</li>"#,
                    when = html_escape(&when),
                    title = html_escape(&occ.event.title),
                    names = html_escape(&names.join(", ")),
                    initials = initials(&people),
                )
            })
            .collect();
        format!(r#"<ul class="list">{rows}</ul>"#)
    };
    format!(
        r#"<section class="card pc"><h2>{heading}</h2>{body}</section>"#,
        heading = html_escape(&long_day(day)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use manage_our_home_shared::dto::agenda::EventResponse;
    use uuid::Uuid;

    // -- notices_html (#455) -------------------------------------------

    #[test]
    fn unreadable_events_put_an_error_above_the_grid() {
        let html = notices_html(None, true);
        assert!(html.contains(r#"class="notice error""#), "{html}");
        assert!(
            html.contains("Impossible de charger les événements"),
            "{html}"
        );
    }

    #[test]
    fn readable_events_put_no_error_above_the_grid() {
        assert_eq!(notices_html(None, false), "");
    }

    /// An action's outcome and a failed read can come together: both show.
    #[test]
    fn the_action_notice_and_the_error_both_show() {
        let html = notices_html(Some("event_created"), true);
        assert!(html.contains("Événement créé."), "{html}");
        assert!(html.contains(r#"class="notice error""#), "{html}");
    }

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
                assignee_ids: vec![Uuid::nil()],
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
        let html = render_chips(&refs, Some(MONTH_CHIP_CAP), day(15), &[]);
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
        let html = render_chips(&refs, Some(MONTH_CHIP_CAP), day(15), &[]);
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
        let html = render_chips(&refs, None, day(15), &[]);
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
        let html = render_grid(
            &week,
            &by_day,
            None,
            day(1),
            None,
            "Semaine du 13 juillet",
            &[],
        );
        assert!(html.starts_with(r#"<table class="cal">"#), "{html}");
        // Seven weekday headers, and seven cells in one row: a day with
        // nothing on it still renders its cell, because that is what makes
        // the thing a grid. Dropping it is the phone reading's job.
        assert_eq!(html.matches(r#"<th scope="col">"#).count(), 7, "{html}");
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
        let html = render_grid(
            &week,
            &by_day,
            None,
            day(29),
            None,
            "Semaine du 27 juillet",
            &[],
        );
        assert_eq!(html.matches("cal-day outside").count(), 0, "{html}");
    }

    #[test]
    fn the_month_grid_marks_today_and_the_days_of_the_neighbouring_months() {
        let by_day: BTreeMap<NaiveDate, Vec<&OccurrenceResponse>> = BTreeMap::new();
        let days: Vec<NaiveDate> = month_grid(2026, 7);
        let html = render_grid(&days, &by_day, Some(7), day(15), None, "juillet 2026", &[]);
        assert_eq!(html.matches("cal-cell current").count(), 1, "{html}");
        // July 2026 starts on a Wednesday: two June days lead the grid, and
        // the six weeks run into August.
        assert!(html.matches("cal-day outside").count() >= 2, "{html}");
    }

    // -- what the grid is called (#145) ----------------------------------

    #[test]
    fn the_grid_carries_its_own_name_in_a_caption() {
        // The month used to live only in the `<h1>` above the table: a
        // screen reader entering the table by table navigation heard seven
        // weekdays and no month. The caption is the table's own name.
        let by_day: BTreeMap<NaiveDate, Vec<&OccurrenceResponse>> = BTreeMap::new();
        let days: Vec<NaiveDate> = month_grid(2026, 7);
        let html = render_grid(&days, &by_day, Some(7), day(15), None, "juillet 2026", &[]);
        assert!(
            html.starts_with(r#"<table class="cal"><caption>juillet 2026</caption><thead>"#),
            "{html}"
        );
    }

    #[test]
    fn the_caption_is_escaped_like_the_heading() {
        let by_day: BTreeMap<NaiveDate, Vec<&OccurrenceResponse>> = BTreeMap::new();
        let week: Vec<NaiveDate> = (13..=19).map(day).collect();
        let html = render_grid(&week, &by_day, None, day(1), None, "<b>", &[]);
        assert!(html.contains("<caption>&lt;b&gt;</caption>"), "{html}");
    }

    #[test]
    fn every_weekday_header_heads_a_column() {
        let by_day: BTreeMap<NaiveDate, Vec<&OccurrenceResponse>> = BTreeMap::new();
        let week: Vec<NaiveDate> = (13..=19).map(day).collect();
        let html = render_grid(&week, &by_day, None, day(1), None, "x", &[]);
        // Every `<th` but the one opening `<thead>` is a weekday header.
        assert_eq!(html.matches("<th").count(), 7 + 1, "{html}");
        assert_eq!(html.matches(r#"<th scope="col">"#).count(), 7, "{html}");
    }

    #[test]
    fn the_month_view_is_named_by_month_and_year() {
        assert_eq!(view_title(false, day(15)), "juillet 2026");
    }

    #[test]
    fn the_week_view_is_named_by_its_first_day() {
        assert_eq!(view_title(true, day(13)), "Semaine du 13 juillet");
    }

    #[test]
    fn a_week_is_named_by_its_monday_whatever_day_it_was_opened_on() {
        // « Aujourd'hui » and the arrows of the week view keep the day they
        // were given, a Wednesday as often as a Monday: the title has to
        // name the week, not that day.
        assert_eq!(view_title(true, day(15)), "Semaine du 13 juillet");
        assert_eq!(view_title(true, day(19)), "Semaine du 13 juillet");
        // 2026-08-02 is a Sunday: its week starts in July.
        let sunday = NaiveDate::from_ymd_opt(2026, 8, 2).unwrap();
        assert_eq!(view_title(true, sunday), "Semaine du 27 juillet");
    }

    // -- render_nav ----------------------------------------------------
    //
    // The prev/next arrows are an icon alone. Before #320 the template wrote
    // them inside a raw string as `"◀"`, so the quotes reached the page, and
    // nothing gave the links a name a screen reader could announce. Since
    // #522 the glyphs are the icon set's chevrons; the names are unchanged.

    /// The `<a …>…</a>` whose `href` targets `date`, as rendered.
    fn nav_link(html: &str, view: &str, date: &str) -> String {
        let href = format!(r#"href="/agenda?view={view}&date={date}""#);
        let start = html[..html.find(&href).expect("link is rendered")]
            .rfind("<a ")
            .unwrap();
        let end = start + html[start..].find("</a>").unwrap() + "</a>".len();
        html[start..end].to_string()
    }

    #[test]
    fn the_month_arrows_are_an_icon_named_for_the_month_they_reach() {
        let html = render_nav(false, day(15), "juillet 2026");
        let prev = nav_link(&html, "month", "2026-06-01");
        let next = nav_link(&html, "month", "2026-08-01");
        assert!(prev.contains(r#"aria-label="Mois précédent""#), "{prev}");
        assert!(next.contains(r#"aria-label="Mois suivant""#), "{next}");
        assert!(prev.ends_with(&arrow(Icon::Previous)), "{prev}");
        assert!(next.ends_with(&arrow(Icon::Next)), "{next}");
    }

    /// The end of an arrow link: its icon and nothing else, no glyph left
    /// beside it.
    fn arrow(icon: Icon) -> String {
        format!(">{}</a>", crate::icons::icon(icon))
    }

    #[test]
    fn the_week_arrows_are_named_for_the_week_they_reach() {
        // They step seven days: calling them "Mois précédent" would announce
        // a jump the link does not make.
        let html = render_nav(true, day(15), "Semaine du 13 juillet");
        let prev = nav_link(&html, "week", "2026-07-08");
        let next = nav_link(&html, "week", "2026-07-22");
        assert!(
            prev.contains(r#"aria-label="Semaine précédente""#),
            "{prev}"
        );
        assert!(next.contains(r#"aria-label="Semaine suivante""#), "{next}");
        assert!(prev.ends_with(&arrow(Icon::Previous)), "{prev}");
        assert!(next.ends_with(&arrow(Icon::Next)), "{next}");
    }

    #[test]
    fn no_literal_quote_reaches_the_nav_text() {
        // Attribute values are quoted; text between tags never is.
        let html = render_nav(false, day(15), "juillet 2026");
        for text in html.split('>').filter_map(|s| s.split('<').next()) {
            assert!(!text.contains('"'), "quoted text node: {text:?}");
        }
    }

    // -- the head of the page (#525) -----------------------------------

    /// The `<nav class="segment views">` of the rendered head.
    fn views_toggle(html: &str) -> &str {
        let start = html
            .find(r#"<nav class="segment views" aria-label="Affichage">"#)
            .expect("the view toggle is rendered");
        let end = start + html[start..].find("</nav>").unwrap();
        &html[start..end]
    }

    #[test]
    fn the_view_toggle_marks_the_view_on_screen() {
        // Mois and Semaine are one control with two positions, on a phone
        // as on a PC: the one on screen says so, in markup and not only in
        // colour.
        let month = render_nav(false, day(15), "juillet 2026");
        let toggle = views_toggle(&month);
        assert!(
            toggle.contains(
                r#"<a href="/agenda?view=month&date=2026-07-15" aria-current="true">Mois</a>"#
            ),
            "{toggle}"
        );
        assert!(
            toggle.contains(r#"<a href="/agenda?view=week&date=2026-07-15">Semaine</a>"#),
            "{toggle}"
        );
        let week = render_nav(true, day(15), "Semaine du 13 juillet");
        let toggle = views_toggle(&week);
        assert!(
            toggle.contains(
                r#"<a href="/agenda?view=week&date=2026-07-15" aria-current="true">Semaine</a>"#
            ),
            "{toggle}"
        );
        assert_eq!(toggle.matches("aria-current").count(), 1, "{toggle}");
    }

    #[test]
    fn today_keeps_the_view_it_was_pressed_in() {
        let today = today_paris();
        for (is_week, view) in [(false, "month"), (true, "week")] {
            let html = render_nav(is_week, day(15), "x");
            assert!(
                html.contains(&format!(
                    r#"href="/agenda?view={view}&date={today}">Aujourd'hui</a>"#
                )),
                "{html}"
            );
        }
    }

    #[test]
    fn a_new_event_is_a_button_on_a_pc_and_the_floating_plus_on_a_phone() {
        // The home's floating "+" (#524), the same `.fab`: the mock-up of
        // the phone agenda draws it where the PC has its button.
        let html = render_nav(false, day(15), "juillet 2026");
        assert!(
            html.contains(r#"<a class="btn pc" href="/agenda/new">"#),
            "{html}"
        );
        assert!(
            html.contains(&icon_link(
                "btn fab phone",
                "/agenda/new",
                "Ajouter un événement",
                Icon::Plus
            )),
            "{html}"
        );
        // The imports stay reachable on both: a phone has no other way in
        // from the agenda.
        assert!(
            html.contains(r#"<a class="btn secondary" href="/agenda/imports">Agendas Google</a>"#),
            "{html}"
        );
    }

    // -- chips coloured by member (#525) -------------------------------

    #[test]
    fn a_member_tint_is_the_class_of_their_own_ramp_hue() {
        // The chip's ground and the avatar's initial have to name the same
        // member: one hash, two spellings of its result.
        for i in 0u8..64 {
            let id = Uuid::from_bytes([i, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 1, 2, 3, 4, 5]);
            assert_eq!(
                format!("--{}", member_tint(id)),
                crate::app::member_colour(id),
                "{id}"
            );
        }
    }

    fn named(n: u128, name: &str) -> GroupMember {
        GroupMember {
            user_id: Uuid::from_u128(n),
            display_name: name.to_string(),
            email: format!("{n}@example.test"),
            has_avatar: false,
            role: "member".to_string(),
        }
    }

    fn assigned(hour: u32, title: &str, ids: &[u128]) -> OccurrenceResponse {
        let mut occ = occurrence(hour, title);
        occ.event.assignee_ids = ids.iter().map(|n| Uuid::from_u128(*n)).collect();
        occ
    }

    #[test]
    fn a_chip_wears_the_tint_of_its_member_and_their_initial() {
        let members = [named(7, "Chloé"), named(8, "Alex")];
        let occs = [assigned(9, "Piscine", &[7])];
        let refs: Vec<&OccurrenceResponse> = occs.iter().collect();
        let html = render_chips(&refs, None, day(15), &members);
        let tint = member_tint(Uuid::from_u128(7));
        assert!(html.contains(&format!(r#"class="chip {tint}">"#)), "{html}");
        // The hue is never alone (WCAG 1.4.1): the initial rides with it.
        assert!(
            html.contains(&crate::app::member_avatar(
                Uuid::from_u128(7),
                "Chloé",
                None
            )),
            "{html}"
        );
        assert!(!html.contains(">A</span>"), "{html}");
    }

    #[test]
    fn a_shared_chip_takes_its_first_member_tint_and_every_initial() {
        let members = [named(7, "Chloé"), named(8, "Alex")];
        let occs = [assigned(9, "Pédiatre", &[8, 7])];
        let refs: Vec<&OccurrenceResponse> = occs.iter().collect();
        let html = render_chips(&refs, None, day(15), &members);
        let tint = member_tint(Uuid::from_u128(8));
        assert!(html.contains(&format!(r#"class="chip {tint}">"#)), "{html}");
        let alex = html.find(">A</span>").expect("Alex's initial");
        let chloe = html.find(">C</span>").expect("Chloé's initial");
        assert!(alex < chloe, "initials follow the assignees' order: {html}");
    }

    #[test]
    fn a_chip_with_no_known_member_stays_neutral() {
        // Nobody to name, so no hue: a colour without its initial is the
        // WCAG 1.4.1 failure the initial exists to prevent. Same when the
        // roster could not be read at all.
        let occs = [assigned(9, "Dentiste", &[99])];
        let refs: Vec<&OccurrenceResponse> = occs.iter().collect();
        for members in [vec![named(7, "Chloé")], vec![]] {
            let html = render_chips(&refs, None, day(15), &members);
            assert!(html.contains(r#"class="chip">"#), "{html}");
            assert!(!html.contains("avatar"), "{html}");
        }
    }

    #[test]
    fn a_chip_with_no_assignee_is_its_creators() {
        // `assignee_ids` can arrive empty (home.rs, `row_assignee_ids`):
        // the creator is who the event belongs to.
        let members = [named(7, "Chloé")];
        let mut occ = assigned(9, "Courses", &[]);
        occ.event.created_by = Uuid::from_u128(7);
        let occs = [occ];
        let refs: Vec<&OccurrenceResponse> = occs.iter().collect();
        let html = render_chips(&refs, None, day(15), &members);
        assert!(html.contains(">C</span>"), "{html}");
    }

    #[test]
    fn a_done_task_keeps_its_initial_and_loses_its_tint() {
        // --muted, the colour of what is done, measures 4.19:1 on the
        // darkest tint in the dark theme: under AA. The neutral ground is
        // where it holds.
        let members = [named(7, "Chloé")];
        let mut occ = assigned(9, "Poubelles", &[7]);
        occ.event.is_task = true;
        occ.event.completed_at = Some(occ.occurrence_starts_at);
        let occs = [occ];
        let refs: Vec<&OccurrenceResponse> = occs.iter().collect();
        let html = render_chips(&refs, None, day(15), &members);
        assert!(html.contains(r#"class="chip done">"#), "{html}");
        assert!(html.contains(">C</span>"), "{html}");
    }

    // -- the day panel of a PC (#525) ----------------------------------

    #[test]
    fn the_day_panel_is_named_by_the_day_it_shows() {
        let html = day_panel(day(14), &[], &[]);
        assert!(html.contains("<h2>Mardi 14 juillet</h2>"), "{html}");
        assert!(html.starts_with(r#"<section class="card pc">"#), "{html}");
    }

    #[test]
    fn the_day_panel_lists_each_occurrence_with_its_time_and_people() {
        let members = [named(7, "Chloé"), named(8, "Alex")];
        let occs = [
            assigned(9, "Dépôt <école>", &[7]),
            assigned(14, "Pédiatre", &[8, 7]),
        ];
        let refs: Vec<&OccurrenceResponse> = occs.iter().collect();
        let html = day_panel(day(15), &refs, &members);
        assert_eq!(
            html.matches(r#"<li class="list-row">"#).count(),
            2,
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"<span class="when">{}</span>"#,
                fmt_paris(occs[0].occurrence_starts_at, "%H:%M")
            )),
            "{html}"
        );
        assert!(
            html.contains("<strong>Dépôt &lt;école&gt;</strong>"),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="muted">Alex, Chloé</span>"#),
            "{html}"
        );
        // The rows are not links: the chip beside them in the grid is, and
        // a second link per occurrence would be a second tab stop for it.
        assert!(!html.contains("<a "), "{html}");
    }

    #[test]
    fn a_whole_day_event_in_the_panel_says_so() {
        let mut occ = assigned(0, "Anniversaire", &[]);
        occ.event.all_day = true;
        let occs = [occ];
        let refs: Vec<&OccurrenceResponse> = occs.iter().collect();
        let html = day_panel(day(15), &refs, &[]);
        assert!(
            html.contains(r#"<span class="when">Journée</span>"#),
            "{html}"
        );
    }

    #[test]
    fn an_empty_day_panel_says_nothing_is_planned() {
        let html = day_panel(day(15), &[], &[]);
        assert!(
            html.contains(r#"<p class="muted">Rien de prévu ce jour-là.</p>"#),
            "{html}"
        );
        assert!(!html.contains("<ul"), "{html}");
    }
}
