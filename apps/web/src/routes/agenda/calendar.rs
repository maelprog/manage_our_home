//! `GET /agenda` — the hand-rolled month/week calendar (no calendar
//! library, the accepted Leptos trade-off from `architecture.md`). Events
//! and tasks-as-events render together; recurring series are expanded by
//! `GET /groups/:gid/events?from&to` so an occurrence appears in every
//! day-cell it falls on within the visible window. `?view=week` switches to
//! the 7-day view; `?date=YYYY-MM-DD` moves the focus.

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
    let grid_html = if is_week {
        render_week(&days, &by_day, focus, today)
    } else {
        render_month(&days, &by_day, focus.month(), today)
    };
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

fn render_month(
    days: &[NaiveDate],
    by_day: &BTreeMap<NaiveDate, Vec<&OccurrenceResponse>>,
    focus_month: u32,
    today: NaiveDate,
) -> String {
    let headers: String = FR_WEEKDAYS
        .iter()
        .map(|d| format!(r#"<th>{d}</th>"#))
        .collect();

    let mut rows = String::new();
    for week in days.chunks(7) {
        rows.push_str("<tr>");
        for day in week {
            let in_month = day.month() == focus_month;
            let today_class = if *day == today { " current" } else { "" };
            let outside_class = if in_month { "" } else { " outside" };
            let chips = render_chips(by_day.get(day).map(|v| v.as_slice()).unwrap_or(&[]));
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

fn render_week(
    days: &[NaiveDate],
    by_day: &BTreeMap<NaiveDate, Vec<&OccurrenceResponse>>,
    _focus: NaiveDate,
    today: NaiveDate,
) -> String {
    let mut cols = String::new();
    for (i, day) in days.iter().enumerate() {
        let today_class = if *day == today { " current" } else { "" };
        let chips = render_chips(by_day.get(day).map(|v| v.as_slice()).unwrap_or(&[]));
        cols.push_str(&format!(
            r#"<div class="cal-col{today_class}">
<div class="cal-day">{wd} {num}</div>{chips}</div>"#,
            today_class = today_class,
            wd = FR_WEEKDAYS[i],
            num = day.day(),
            chips = chips,
        ));
    }
    format!(r#"<div class="cal-week">{cols}</div>"#)
}

/// One day-cell's occurrence chips. Tasks get a ☑/☐ marker and a
/// strike-through when that occurrence is completed.
fn render_chips(occs: &[&OccurrenceResponse]) -> String {
    occs.iter()
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
        .collect()
}
