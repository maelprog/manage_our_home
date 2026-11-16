//! `/recipes/week` — « Planning des repas » (#813): the planned recipes
//! (#469) that have a day (#811), one row per day, midi and soir in each;
//! those without a day under « À placer », with the « Choisir un jour »
//! form of the recipe page (#812).
//!
//! The window (arbitrage of the validated mock-up, 2026-11-14): without
//! `?date=`, yesterday, today and the seven days to come — nine days, a
//! single past one; with `?date=`, the seven days from that one. ‹ and ›
//! move the window's first day by seven days, so « précédent » from the
//! current window does not fall on a calendar week (a choice of the user).
//! A `date` that does not read as a day, or whose seven days hold none of
//! the 366 days either side of today that a plan may take, is the current
//! window.
//!
//! The summary counts the meals of the window and the ingredients they
//! lack (`missing`, #559), each name once. The grocery list itself is
//! still generated from every planned recipe, whatever its day.
//!
//! An empty slot links to the recipe list with its day and its meal
//! (`/recipes?date=…&repas=…`), where « À réaliser » plans a recipe on it —
//! except on a day beyond the horizon, which ‹ and › do not go past.

use std::collections::{HashMap, HashSet};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::{Datelike, Duration, NaiveDate};
use manage_our_home_shared::dto::recipes::{PlannedSelection, RecipeList};
use manage_our_home_shared::validation::budget::month_name_fr;
use manage_our_home_shared::validation::recipes::{duration_label, servings_label};
use uuid::Uuid;

use crate::app::{html_escape, kitchen_segment, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::lenient_query::{lenient, LenientQuery};
use crate::routes::agenda::calendar::short_weekday;
use crate::state::{api_request_auth, AppState};

use super::detail::{error_text, notice_text, plan_html};
use super::list::{fetch_suggestions, tabs_html, Tab};
use super::missing::{missing_badge, missing_dialog};
use super::{family_context, planned_with_stock, recipes_cookie};

/// The days a plan may lie from today, either side (the API's bound, #811).
const HORIZON_DAYS: i64 = 366;

/// The days a window shows: its first, and how many.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    pub start: NaiveDate,
    pub days: i64,
    /// The window of the page without `?date=`: from yesterday.
    pub current: bool,
}

impl Window {
    /// Its last day.
    pub(crate) fn end(&self) -> NaiveDate {
        self.start + Duration::days(self.days - 1)
    }

    pub(crate) fn contains(&self, day: NaiveDate) -> bool {
        day >= self.start && day <= self.end()
    }
}

/// Whether a plan may take `day`: at most [`HORIZON_DAYS`] from today.
pub(crate) fn within_horizon(day: NaiveDate, today: NaiveDate) -> bool {
    day.signed_duration_since(today).num_days().abs() <= HORIZON_DAYS
}

/// The window `?date=` asks for. `None`, yesterday, or a day whose seven
/// days hold none within [`HORIZON_DAYS`] of today: the current one, nine
/// days from yesterday. Any other day: the seven from it — so a window
/// may straddle the horizon, to show the days on its near side.
pub(crate) fn window(asked: Option<NaiveDate>, today: NaiveDate) -> Window {
    let yesterday = today - Duration::days(1);
    let earliest = today - Duration::days(HORIZON_DAYS + 6);
    let latest = today + Duration::days(HORIZON_DAYS);
    match asked {
        Some(start) if start != yesterday && (earliest..=latest).contains(&start) => Window {
            start,
            days: 7,
            current: false,
        },
        _ => Window {
            start: yesterday,
            days: 9,
            current: true,
        },
    }
}

/// The `?date=` of the window that shows `day`: `None` when the current
/// one does, otherwise the window of the seven-day grid anchored on
/// yesterday that holds it — the one ‹ and › reach.
pub(crate) fn window_for(day: NaiveDate, today: NaiveDate) -> Option<NaiveDate> {
    let current = window(None, today);
    if current.contains(day) {
        return None;
    }
    let weeks = day
        .signed_duration_since(current.start)
        .num_days()
        .div_euclid(7);
    Some(current.start + Duration::days(weeks * 7))
}

/// The page's address for a window's first day; the current window's
/// carries no parameter.
pub(crate) fn week_href(start: Option<NaiveDate>) -> String {
    match start {
        Some(day) => format!("/recipes/week?date={}", day.format("%Y-%m-%d")),
        None => "/recipes/week".to_string(),
    }
}

/// The `back` value that returns a form to this page on that window
/// (`recipes::back_path` reads it): `week`, or `week@AAAA-MM-JJ`.
pub(crate) fn week_back(start: Option<NaiveDate>) -> String {
    match start {
        Some(day) => format!("week@{}", day.format("%Y-%m-%d")),
        None => "week".to_string(),
    }
}

/// The window `back` names, when it names this page: `Some(None)` for
/// the current one, `Some(Some(start))` for another; `None` when `back`
/// is not this page's.
pub(crate) fn back_window(back: &str) -> Option<Option<NaiveDate>> {
    if back == "week" {
        return Some(None);
    }
    let day = back.strip_prefix("week@")?;
    Some(NaiveDate::parse_from_str(day, "%Y-%m-%d").ok())
}

/// The meal of an « Ajouter » link (`repas=midi|soir`) as a selection says
/// it, `lunch` or `dinner`; the slot only when its day reads too, and is
/// one a plan may take — the API would refuse the others.
pub(crate) fn slot_from_query(
    date: Option<NaiveDate>,
    repas: Option<&str>,
    today: NaiveDate,
) -> Option<(NaiveDate, &'static str)> {
    let meal = match repas? {
        "midi" => "lunch",
        "soir" => "dinner",
        _ => return None,
    };
    let day = date.filter(|d| within_horizon(*d, today))?;
    Some((day, meal))
}

/// French month abbreviations, as a day's heading writes them: « 13 nov. ».
fn short_month_fr(month: u32) -> &'static str {
    const MONTHS: [&str; 12] = [
        "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.",
        "déc.",
    ];
    MONTHS[(month as usize).clamp(1, 12) - 1]
}

/// A day's number, « 1er » on the first of the month.
fn day_number(day: NaiveDate) -> String {
    match day.day() {
        1 => "1er".to_string(),
        n => n.to_string(),
    }
}

/// The window in words, under ‹ Aujourd'hui ›: « ven. 13 au sam. 21
/// novembre 2026 », the month or the year said on both ends when they
/// differ.
pub(crate) fn range_label(start: NaiveDate, end: NaiveDate) -> String {
    let first = format!("{} {}", short_weekday(start), day_number(start));
    let first = if start.year() != end.year() {
        format!("{first} {} {}", month_name_fr(start.month()), start.year())
    } else if start.month() != end.month() {
        format!("{first} {}", month_name_fr(start.month()))
    } else {
        first
    };
    format!(
        "{first} au {} {} {} {}",
        short_weekday(end),
        day_number(end),
        month_name_fr(end.month()),
        end.year()
    )
}

/// How many ingredients the recipes lack, each name once, whatever its
/// case and its surrounding spaces.
pub(crate) fn to_buy_count(planned: &[&PlannedSelection]) -> usize {
    planned
        .iter()
        .flat_map(|p| &p.missing)
        .map(|m| m.name.trim().to_lowercase())
        .collect::<HashSet<_>>()
        .len()
}

/// The summary above the days: the meals of the window and the
/// ingredients to buy for them, or a sentence when there is none.
pub(crate) fn summary_html(meals: usize, to_buy: usize) -> String {
    if meals == 0 {
        return r#"<p class="card summary"><span>Aucun repas planifié cette semaine.</span></p>"#
            .to_string();
    }
    let plural = |n: usize| if n > 1 { "s" } else { "" };
    format!(
        r#"<p class="card summary"><span><strong>{meals}</strong> repas planifié{m}</span><span><strong>{to_buy}</strong> ingrédient{i} à acheter</span></p>"#,
        m = plural(meals),
        i = plural(to_buy),
    )
}

/// The link of an empty slot, to the recipe list with its day and meal.
pub(crate) fn slot_target(day: NaiveDate, meal_slot: &str) -> String {
    let repas = if meal_slot == "lunch" { "midi" } else { "soir" };
    format!("/recipes?date={}&repas={repas}", day.format("%Y-%m-%d"))
}

/// A weekday as a day's heading writes it: « Samedi ».
fn weekday_name(day: NaiveDate) -> &'static str {
    const WEEKDAYS: [&str; 7] = [
        "Lundi", "Mardi", "Mercredi", "Jeudi", "Vendredi", "Samedi", "Dimanche",
    ];
    WEEKDAYS[day.weekday().num_days_from_monday() as usize]
}

/// One recipe in a slot: its name to its page, then its duration and what
/// it lacks — the badge, and the dialog it opens.
fn meal_html(p: &PlannedSelection, durations: &HashMap<Uuid, i32>) -> String {
    let s = &p.selection;
    let duration = durations
        .get(&s.recipe_id)
        .map(|m| format!("<span>{}</span>", duration_label(*m)))
        .unwrap_or_default();
    let missing = missing_badge(s.recipe_id, p.missing.len());
    let meta = if duration.is_empty() && missing.is_empty() {
        String::new()
    } else {
        format!(r#"<span class="meta">{duration}{missing}</span>"#)
    };
    format!(
        r#"<a href="/recipes/{id}">{name}</a>{meta}{dialog}"#,
        id = s.recipe_id,
        name = html_escape(&s.recipe_name),
        dialog = missing_dialog(p),
    )
}

/// A slot of a day: its meal in a pill, then its recipes, or « Ajouter »
/// when it has none — « À plus d'un an » on a day no plan may take. `meal`
/// is `lunch`, `dinner`, or `None` for the dated recipes whose meal is not
/// said, which have no « Ajouter ».
fn slot_html(
    day: NaiveDate,
    today: NaiveDate,
    meal: Option<&str>,
    recipes: &[&PlannedSelection],
    durations: &HashMap<Uuid, i32>,
) -> String {
    let label = match meal {
        Some("lunch") => "Midi",
        Some(_) => "Soir",
        None => "Non précisé",
    };
    if recipes.is_empty() && !within_horizon(day, today) {
        return format!(
            r#"<li class="empty"><span class="when">{label}</span><span class="muted">À plus d'un an</span></li>"#
        );
    }
    if recipes.is_empty() {
        let meal = meal.unwrap_or("dinner");
        return format!(
            r#"<li class="empty"><span class="when">{label}</span><a href="{href}" aria-label="Ajouter un repas, {weekday} {date}, {word}">Ajouter</a></li>"#,
            href = html_escape(&slot_target(day, meal)),
            weekday = weekday_name(day).to_lowercase(),
            date = long_day(day),
            word = label.to_lowercase(),
        );
    }
    let what: String = recipes.iter().map(|p| meal_html(p, durations)).collect();
    format!(r#"<li><span class="when">{label}</span><div class="what">{what}</div></li>"#)
}

/// « 14 novembre », « 1er décembre ».
fn long_day(day: NaiveDate) -> String {
    format!("{} {}", day_number(day), month_name_fr(day.month()))
}

/// A day of the window: its heading — the weekday, the date said in full
/// to the ear and short to the eye, « Aujourd'hui » in a badge on today,
/// which `.current` tints too — then midi and soir.
fn day_html(
    day: NaiveDate,
    today: NaiveDate,
    planned: &[&PlannedSelection],
    durations: &HashMap<Uuid, i32>,
) -> String {
    let on = |meal: Option<&str>| -> Vec<&PlannedSelection> {
        planned
            .iter()
            .copied()
            .filter(|p| p.selection.planned_on == Some(day))
            .filter(|p| match meal {
                Some(m) => p.selection.meal_slot.as_deref() == Some(m),
                None => !matches!(p.selection.meal_slot.as_deref(), Some("lunch" | "dinner")),
            })
            .collect()
    };
    let unsaid = on(None);
    let mut slots = slot_html(day, today, Some("lunch"), &on(Some("lunch")), durations);
    slots += &slot_html(day, today, Some("dinner"), &on(Some("dinner")), durations);
    if !unsaid.is_empty() {
        slots += &slot_html(day, today, None, &unsaid, durations);
    }
    let (class, badge) = if day == today {
        (
            r#" class="current""#,
            r#"<span class="badge accent">Aujourd'hui</span>"#,
        )
    } else {
        ("", "")
    };
    format!(
        r#"<li{class}><h2>{weekday} <span class="sr-only">{long}</span><span class="muted" aria-hidden="true">{short}</span>{badge}</h2><ul>{slots}</ul></li>"#,
        weekday = weekday_name(day),
        long = long_day(day),
        short = format_args!("{} {}", day.day(), short_month_fr(day.month())),
    )
}

/// « À placer »: the planned recipes without a day, each with its number
/// of people, its duration, what it lacks, and « Choisir un jour » (#812),
/// which comes back to this window.
fn to_place_html(
    undated: &[&PlannedSelection],
    durations: &HashMap<Uuid, i32>,
    today: NaiveDate,
    back: &str,
) -> String {
    if undated.is_empty() {
        return String::new();
    }
    let rows: String = undated
        .iter()
        .map(|p| {
            let s = &p.selection;
            let about: Vec<String> = s
                .servings
                .map(servings_label)
                .into_iter()
                .chain(durations.get(&s.recipe_id).map(|m| duration_label(*m)))
                .collect();
            let about = if about.is_empty() {
                String::new()
            } else {
                format!(r#" <span class="muted">{}</span>"#, about.join(" · "))
            };
            format!(
                r#"<li class="list-row stacked"><span><a href="/recipes/{id}"><strong>{name}</strong></a>{about}{missing}</span>{form}{dialog}</li>"#,
                id = s.recipe_id,
                name = html_escape(&s.recipe_name),
                missing = missing_badge(s.recipe_id, p.missing.len()),
                form = plan_html(s.recipe_id, Some(s), false, today, back),
                dialog = missing_dialog(p),
            )
        })
        .collect();
    format!(
        r#"<section aria-labelledby="a-placer"><h2 id="a-placer">À placer</h2><p class="muted">Recettes prévues, sans jour : choisissez-en un pour les ranger dans la semaine.</p><ul class="list">{rows}</ul></section>"#
    )
}

/// What a page of the planning shows: the window, the planned recipes
/// (`None` when they could not be read), the duration of each recipe.
pub(crate) struct WeekView<'a> {
    pub window: Window,
    pub today: NaiveDate,
    pub planned: Option<&'a [PlannedSelection]>,
    pub durations: &'a HashMap<Uuid, i32>,
}

/// The planning below the title: ‹ Aujourd'hui ›, then the summary, the
/// days and « À placer » — or the error in their place.
pub(crate) fn week_html(view: &WeekView) -> String {
    let w = view.window;
    let step = Duration::days(7);
    // An arrow to the window from `start`; a disabled button when that one
    // holds no day a plan may take (`window` would give the current one).
    let arrow = |start: NaiveDate, label: &str, sign: &str| {
        let to = window(Some(start), view.today);
        if to.current && start != view.today - Duration::days(1) {
            return format!(
                r#"<button type="button" class="secondary" aria-label="{label}" disabled>{sign}</button>"#
            );
        }
        format!(
            r#"<a class="btn secondary" href="{href}" aria-label="{label}">{sign}</a>"#,
            href = week_href((!to.current).then_some(start)),
        )
    };
    let nav = format!(
        r#"<nav class="week-nav" aria-label="Période">{prev}<a class="btn secondary" href="/recipes/week">Aujourd'hui</a>{next}<p class="range"><span class="sr-only">Du </span>{range}</p></nav>"#,
        prev = arrow(w.start - step, "7 jours précédents", "‹"),
        next = arrow(w.start + step, "7 jours suivants", "›"),
        range = range_label(w.start, w.end()),
    );
    let Some(planned) = view.planned else {
        return format!(
            r#"{nav}<p class="notice error" role="alert">Impossible de charger les repas de la semaine pour le moment.</p>"#
        );
    };
    let shown: Vec<&PlannedSelection> = planned
        .iter()
        .filter(|p| p.selection.planned_on.is_some_and(|d| w.contains(d)))
        .collect();
    let undated: Vec<&PlannedSelection> = planned
        .iter()
        .filter(|p| p.selection.planned_on.is_none())
        .collect();
    let days: String = (0..w.days)
        .map(|i| {
            day_html(
                w.start + Duration::days(i),
                view.today,
                &shown,
                view.durations,
            )
        })
        .collect();
    let back = week_back((!w.current).then_some(w.start));
    format!(
        r#"{nav}{summary}<ol class="meals">{days}</ol>{place}"#,
        summary = summary_html(shown.len(), to_buy_count(&shown)),
        place = to_place_html(&undated, view.durations, view.today, &back),
    )
}

#[derive(serde::Deserialize)]
pub struct WeekQuery {
    #[serde(default, deserialize_with = "lenient")]
    date: Option<NaiveDate>,
    notice: Option<String>,
    error: Option<String>,
}

/// `GET /recipes/week`.
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    LenientQuery(query): LenientQuery<WeekQuery>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/recipes/week").await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);
    let today = crate::routes::agenda::today_paris();

    // The recipes give each one its duration, and bound the « Possibles »
    // asked for, as on the list. A non-200 reads as no recipe; a transport
    // error, like the selections failing, is the page's error state.
    let recipes = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/recipes", fam.gid),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            Ok(serde_json::from_value::<RecipeList>(resp.body)
                .map(|l| l.recipes)
                .unwrap_or_default())
        }
        Ok(_) => Ok(Vec::new()),
        Err(_) => Err(()),
    };
    let planned = match recipes {
        Ok(_) => planned_with_stock(&state, fam.gid, cookie.as_deref())
            .await
            .ok(),
        Err(()) => None,
    };
    let recipes = recipes.unwrap_or_default();
    let possible = match (&planned, recipes.is_empty()) {
        (Some(_), false) => {
            let path = format!(
                "/groups/{}/recipes/suggestions?possible=true&limit={}",
                fam.gid,
                recipes.len()
            );
            fetch_suggestions(&state, &path, cookie.as_deref())
                .await
                .ok()
                .map(|l| l.len())
        }
        (Some(_), true) => Some(0),
        (None, _) => None,
    };
    let durations: HashMap<Uuid, i32> = recipes
        .iter()
        .filter_map(|r| r.duration_minutes.map(|m| (r.id, m)))
        .collect();
    let counts = planned.as_ref().zip(possible).map(|(p, n)| (p.len(), n));

    let banner = |text: Option<&str>, class: &str| {
        text.map(|t| format!(r#"<p class="notice {class}">{}</p>"#, html_escape(t)))
            .unwrap_or_default()
    };
    let notice = banner(query.notice.as_deref().and_then(notice_text), "success");
    let error = banner(query.error.as_deref().and_then(error_text), "error");
    let week = week_html(&WeekView {
        window: window(query.date, today),
        today,
        planned: planned.as_deref(),
        durations: &durations,
    });
    let body = format!(
        r#"{segment}{tabs}<h1>Planning des repas</h1>
{notice}{error}{week}"#,
        segment = kitchen_segment("/recipes"),
        tabs = tabs_html(Tab::Week, counts),
    );
    Html(shell_with_header(
        Width::Read,
        "Planning des repas",
        &fam.header,
        &body,
    ))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use manage_our_home_shared::dto::recipes::{MissingIngredient, RecipeSelectionResponse};

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// The mock-up's today: Saturday 14 November 2026.
    fn today() -> NaiveDate {
        day(2026, 11, 14)
    }

    fn planned(
        id: u128,
        name: &str,
        on: Option<NaiveDate>,
        meal: Option<&str>,
        missing: &[&str],
    ) -> PlannedSelection {
        PlannedSelection {
            selection: RecipeSelectionResponse {
                id: Uuid::from_u128(id + 1000),
                recipe_id: Uuid::from_u128(id),
                recipe_name: name.into(),
                status: "planned".into(),
                servings: Some(4),
                planned_on: on,
                meal_slot: meal.map(str::to_string),
                created_by: Uuid::from_u128(200),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            missing: missing
                .iter()
                .map(|n| MissingIngredient {
                    name: n.to_string(),
                    quantity: None,
                    unit: None,
                })
                .collect(),
            use_by: None,
            days_left: None,
        }
    }

    // -- the window ---------------------------------------------------------

    #[test]
    fn without_a_date_the_window_is_yesterday_today_and_seven_days() {
        let w = window(None, today());
        assert_eq!(w.start, day(2026, 11, 13));
        assert_eq!(w.days, 9);
        assert_eq!(w.end(), day(2026, 11, 21));
        assert!(w.current);
    }

    #[test]
    fn a_date_opens_the_seven_days_from_it() {
        let w = window(Some(day(2026, 11, 6)), today());
        assert_eq!(w.start, day(2026, 11, 6));
        assert_eq!(w.days, 7);
        assert_eq!(w.end(), day(2026, 11, 12));
        assert!(!w.current);
    }

    #[test]
    fn yesterday_as_a_date_is_the_current_window() {
        assert_eq!(
            window(Some(day(2026, 11, 13)), today()),
            window(None, today())
        );
    }

    #[test]
    fn a_window_without_a_day_within_the_horizon_is_the_current_one() {
        // The horizon: 13 November 2025 to 15 November 2027.
        assert_eq!(
            window(Some(day(2027, 11, 16)), today()),
            window(None, today())
        );
        assert_eq!(
            window(Some(day(2025, 11, 6)), today()),
            window(None, today())
        );
        assert_eq!(window(Some(NaiveDate::MAX), today()), window(None, today()));
        assert_eq!(window(Some(NaiveDate::MIN), today()), window(None, today()));
        // One day within it is enough for a window of its own.
        assert!(!window(Some(day(2027, 11, 15)), today()).current);
        assert!(!window(Some(day(2025, 11, 7)), today()).current);
    }

    #[test]
    fn every_day_a_plan_may_take_has_a_window_that_shows_it() {
        for n in -HORIZON_DAYS..=HORIZON_DAYS {
            let d = today() + Duration::days(n);
            let w = window(window_for(d, today()), today());
            assert!(w.contains(d), "{d}: {w:?}");
        }
        assert_eq!(
            window_for(day(2025, 11, 13), today()),
            Some(day(2025, 11, 7))
        );
    }

    #[test]
    fn previous_and_next_move_the_window_by_seven_days() {
        // From the current window: the 6th to the 12th, then the 20th on.
        let w = window(None, today());
        assert_eq!(
            week_href(Some(w.start - Duration::days(7))),
            "/recipes/week?date=2026-11-06"
        );
        assert_eq!(
            week_href(Some(w.start + Duration::days(7))),
            "/recipes/week?date=2026-11-20"
        );
        // Back to yesterday is the current window, without a parameter.
        assert_eq!(week_href(None), "/recipes/week");
    }

    #[test]
    fn the_window_of_a_day_is_the_current_one_or_the_grid_from_yesterday() {
        for d in 13..=21 {
            assert_eq!(window_for(day(2026, 11, d), today()), None, "{d}");
        }
        assert_eq!(
            window_for(day(2026, 11, 22), today()),
            Some(day(2026, 11, 20))
        );
        assert_eq!(
            window_for(day(2026, 11, 26), today()),
            Some(day(2026, 11, 20))
        );
        assert_eq!(
            window_for(day(2026, 11, 27), today()),
            Some(day(2026, 11, 27))
        );
        assert_eq!(
            window_for(day(2026, 11, 12), today()),
            Some(day(2026, 11, 6))
        );
        assert_eq!(
            window_for(day(2026, 11, 6), today()),
            Some(day(2026, 11, 6))
        );
        assert_eq!(
            window_for(day(2026, 11, 5), today()),
            Some(day(2026, 10, 30))
        );
    }

    #[test]
    fn the_way_back_names_the_window() {
        assert_eq!(week_back(None), "week");
        assert_eq!(week_back(Some(day(2026, 11, 20))), "week@2026-11-20");
        assert_eq!(back_window("week"), Some(None));
        assert_eq!(
            back_window("week@2026-11-20"),
            Some(Some(day(2026, 11, 20)))
        );
        // A day that does not read is the current window, still this page.
        assert_eq!(back_window("week@demain"), Some(None));
        assert_eq!(back_window("list"), None);
        assert_eq!(back_window("detail"), None);
        assert_eq!(back_window("weekend"), None);
    }

    #[test]
    fn an_ajouter_link_names_a_day_and_midi_or_soir() {
        assert_eq!(
            slot_from_query(Some(day(2026, 11, 14)), Some("soir"), today()),
            Some((day(2026, 11, 14), "dinner"))
        );
        assert_eq!(
            slot_from_query(Some(day(2026, 11, 14)), Some("midi"), today()),
            Some((day(2026, 11, 14), "lunch"))
        );
        assert_eq!(
            slot_from_query(Some(day(2026, 11, 14)), Some("lunch"), today()),
            None
        );
        assert_eq!(
            slot_from_query(Some(day(2026, 11, 14)), None, today()),
            None
        );
        assert_eq!(slot_from_query(None, Some("soir"), today()), None);
        // A day no plan may take: no slot, so no selection the API refuses.
        assert_eq!(
            slot_from_query(Some(day(2027, 11, 15)), Some("soir"), today()),
            Some((day(2027, 11, 15), "dinner"))
        );
        assert_eq!(
            slot_from_query(Some(day(2027, 11, 16)), Some("soir"), today()),
            None
        );
        assert_eq!(
            slot_from_query(Some(day(2025, 11, 12)), Some("midi"), today()),
            None
        );
        assert_eq!(
            slot_target(day(2026, 11, 14), "dinner"),
            "/recipes?date=2026-11-14&repas=soir"
        );
        assert_eq!(
            slot_target(day(2026, 11, 3), "lunch"),
            "/recipes?date=2026-11-03&repas=midi"
        );
    }

    // -- words --------------------------------------------------------------

    #[test]
    fn the_range_says_the_month_and_the_year_once_when_they_are_shared() {
        assert_eq!(
            range_label(day(2026, 11, 13), day(2026, 11, 21)),
            "ven. 13 au sam. 21 novembre 2026"
        );
        assert_eq!(
            range_label(day(2026, 11, 27), day(2026, 12, 3)),
            "ven. 27 novembre au jeu. 3 décembre 2026"
        );
        assert_eq!(
            range_label(day(2026, 12, 28), day(2027, 1, 3)),
            "lun. 28 décembre 2026 au dim. 3 janvier 2027"
        );
        assert_eq!(
            range_label(day(2026, 10, 1), day(2026, 10, 7)),
            "jeu. 1er au mer. 7 octobre 2026"
        );
    }

    #[test]
    fn the_short_months_are_the_french_abbreviations() {
        let all: Vec<&str> = (1..=12).map(short_month_fr).collect();
        assert_eq!(
            all,
            [
                "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.",
                "nov.", "déc."
            ]
        );
    }

    #[test]
    fn an_ingredient_lacking_in_two_recipes_is_bought_once() {
        let a = planned(1, "Gratin", None, None, &["Crème", "Lait", "Gruyère"]);
        let b = planned(2, "Quiche", None, None, &[" crème ", "Poireau"]);
        let c = planned(3, "Poulet", None, None, &[]);
        assert_eq!(to_buy_count(&[&a, &b, &c]), 4);
        assert_eq!(to_buy_count(&[&c]), 0);
        assert_eq!(to_buy_count(&[]), 0);
    }

    #[test]
    fn the_summary_counts_the_meals_and_what_to_buy() {
        let html = summary_html(7, 6);
        assert!(
            html.contains("<strong>7</strong> repas planifiés"),
            "{html}"
        );
        assert!(
            html.contains("<strong>6</strong> ingrédients à acheter"),
            "{html}"
        );
        let html = summary_html(1, 1);
        assert!(
            html.contains("<strong>1</strong> repas planifié<"),
            "{html}"
        );
        assert!(
            html.contains("<strong>1</strong> ingrédient à acheter"),
            "{html}"
        );
        let html = summary_html(2, 0);
        assert!(
            html.contains("<strong>0</strong> ingrédient à acheter"),
            "{html}"
        );
        let html = summary_html(0, 0);
        assert!(
            html.contains("Aucun repas planifié cette semaine."),
            "{html}"
        );
        assert!(!html.contains("<strong>"), "{html}");
    }

    // -- the page -----------------------------------------------------------

    fn render(planned: Option<&[PlannedSelection]>, asked: Option<NaiveDate>) -> String {
        let durations: HashMap<Uuid, i32> =
            [(Uuid::from_u128(1), 70), (Uuid::from_u128(2), 50)].into();
        week_html(&WeekView {
            window: window(asked, today()),
            today: today(),
            planned,
            durations: &durations,
        })
    }

    #[test]
    fn an_empty_window_offers_ajouter_in_every_slot_and_says_so() {
        let html = render(Some(&[]), None);
        assert_eq!(html.matches("<h2>").count(), 9, "{html}");
        assert_eq!(html.matches(">Ajouter</a>").count(), 18, "{html}");
        assert!(
            html.contains("Aucun repas planifié cette semaine."),
            "{html}"
        );
        assert!(
            html.contains(r#"<a href="/recipes?date=2026-11-14&amp;repas=soir" aria-label="Ajouter un repas, samedi 14 novembre, soir">Ajouter</a>"#),
            "{html}"
        );
        // Nothing to place: no « À placer ».
        assert!(!html.contains("À placer"), "{html}");
    }

    #[test]
    fn the_days_run_from_yesterday_and_today_is_said_in_words() {
        let html = render(Some(&[]), None);
        let friday = html
            .find("Vendredi <span class=\"sr-only\">13 novembre</span>")
            .unwrap();
        let saturday = html
            .find("Samedi <span class=\"sr-only\">14 novembre</span>")
            .unwrap();
        assert!(friday < saturday, "{html}");
        assert!(
            html.contains(r#"<span class="muted" aria-hidden="true">14 nov.</span>"#),
            "{html}"
        );
        assert_eq!(html.matches("Aujourd'hui</span>").count(), 1, "{html}");
        assert_eq!(html.matches(r#"<li class="current">"#).count(), 1, "{html}");
        assert!(!html.contains("12 novembre"), "{html}");
        assert!(html.contains("21 novembre"), "{html}");
        assert!(!html.contains("22 novembre"), "{html}");
        // Another window: seven days, and no today.
        let html = render(Some(&[]), Some(day(2026, 11, 20)));
        assert_eq!(html.matches("<h2>").count(), 7, "{html}");
        assert!(!html.contains("Aujourd'hui</span>"), "{html}");
    }

    #[test]
    fn the_navigation_moves_by_seven_days_and_names_its_arrows() {
        let html = render(Some(&[]), None);
        assert!(
            html.contains(r#"<a class="btn secondary" href="/recipes/week?date=2026-11-06" aria-label="7 jours précédents">‹</a>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<a class="btn secondary" href="/recipes/week">Aujourd'hui</a>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"href="/recipes/week?date=2026-11-20" aria-label="7 jours suivants""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="sr-only">Du </span>ven. 13 au sam. 21 novembre 2026"#),
            "{html}"
        );
        // From the 6th, › comes back to the current window.
        let html = render(Some(&[]), Some(day(2026, 11, 6)));
        assert!(
            html.contains(r#"href="/recipes/week" aria-label="7 jours suivants""#),
            "{html}"
        );
    }

    #[test]
    fn the_navigation_stops_at_the_horizon_and_so_does_ajouter() {
        // The last window: the 12th to the 18th of November 2027, the
        // horizon ending on the 15th.
        let html = render(Some(&[]), Some(day(2027, 11, 5)));
        assert!(
            html.contains(r#"href="/recipes/week?date=2027-11-12" aria-label="7 jours suivants""#),
            "{html}"
        );
        let html = render(Some(&[]), Some(day(2027, 11, 12)));
        assert!(
            html.contains(r#"<button type="button" class="secondary" aria-label="7 jours suivants" disabled>›</button>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"href="/recipes/week?date=2027-11-05" aria-label="7 jours précédents""#
            ),
            "{html}"
        );
        assert_eq!(html.matches(">Ajouter</a>").count(), 8, "{html}");
        assert!(html.contains("2027-11-15&amp;repas=soir"), "{html}");
        assert!(!html.contains("2027-11-16&amp;"), "{html}");
        assert_eq!(
            html.matches(r#"<span class="muted">À plus d'un an</span>"#)
                .count(),
            6,
            "{html}"
        );
        // The first window: the 7th to the 13th of November 2025, the
        // horizon starting on the 13th.
        let html = render(Some(&[]), Some(day(2025, 11, 7)));
        assert!(
            html.contains(r#"<button type="button" class="secondary" aria-label="7 jours précédents" disabled>‹</button>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"href="/recipes/week?date=2025-11-14" aria-label="7 jours suivants""#),
            "{html}"
        );
        assert_eq!(html.matches(">Ajouter</a>").count(), 2, "{html}");
        assert!(html.contains("2025-11-13&amp;repas=midi"), "{html}");
        // Within it, both arrows are links.
        let html = render(Some(&[]), None);
        assert!(!html.contains("disabled"), "{html}");
    }

    #[test]
    fn a_partial_week_places_each_recipe_on_its_day_and_meal() {
        let planned = [
            planned(
                1,
                "Gratin <dauphinois>",
                Some(day(2026, 11, 13)),
                Some("dinner"),
                &[],
            ),
            planned(2, "Quiche", Some(day(2026, 11, 14)), Some("lunch"), &[]),
            // Outside the window: not drawn, not counted.
            planned(3, "Tarte", Some(day(2026, 11, 22)), Some("lunch"), &[]),
        ];
        let html = render(Some(&planned), None);
        assert!(html.contains("Gratin &lt;dauphinois&gt;"), "{html}");
        assert!(!html.contains("Tarte"), "{html}");
        assert!(
            html.contains("<strong>2</strong> repas planifiés"),
            "{html}"
        );
        assert_eq!(html.matches(">Ajouter</a>").count(), 16, "{html}");
        // The Friday: midi empty, then soir the gratin, its duration.
        let friday = &html[html.find("13 novembre").unwrap()..html.find("14 novembre").unwrap()];
        let lunch = friday.find(r#"<span class="when">Midi</span><a href="/recipes?date=2026-11-13&amp;repas=midi""#).unwrap();
        let dinner = friday.find(r#"<span class="when">Soir</span>"#).unwrap();
        assert!(lunch < dinner, "{friday}");
        assert!(
            friday.contains(&format!(
                r#"<a href="/recipes/{}">Gratin &lt;dauphinois&gt;</a>"#,
                Uuid::from_u128(1)
            )),
            "{friday}"
        );
        assert!(friday.contains("<span>1 h 10</span>"), "{friday}");
        assert!(!friday.contains("Il manque"), "{friday}");
    }

    #[test]
    fn a_recipe_lacking_ingredients_carries_the_missing_badge_and_its_dialog() {
        let planned = [
            planned(
                1,
                "Gratin",
                Some(day(2026, 11, 13)),
                Some("dinner"),
                &["Crème", "Lait", "Gruyère"],
            ),
            planned(
                2,
                "Quiche",
                Some(day(2026, 11, 14)),
                Some("lunch"),
                &["crème"],
            ),
            planned(4, "Pizza", Some(day(2026, 11, 21)), Some("dinner"), &[]),
        ];
        let html = render(Some(&planned), None);
        assert!(html.contains("Il manque 3 éléments</a>"), "{html}");
        assert!(html.contains("Il manque 1 élément</a>"), "{html}");
        assert_eq!(
            html.matches("Il manque").count(),
            2 + 2,
            "badges and dialog titles: {html}"
        );
        assert!(
            html.contains(&format!(r#"<dialog id="missing-{}""#, Uuid::from_u128(1))),
            "{html}"
        );
        assert!(
            html.contains("<strong>3</strong> repas planifiés"),
            "{html}"
        );
        assert!(
            html.contains("<strong>3</strong> ingrédients à acheter"),
            "{html}"
        );
    }

    #[test]
    fn a_dated_recipe_without_a_meal_stays_on_its_day() {
        let planned = [planned(1, "Gratin", Some(day(2026, 11, 15)), None, &[])];
        let html = render(Some(&planned), None);
        let sunday = &html[html.find("15 novembre").unwrap()..html.find("16 novembre").unwrap()];
        assert!(
            sunday.contains(r#"<span class="when">Midi</span><a href"#),
            "{sunday}"
        );
        assert!(
            sunday.contains(r#"<span class="when">Soir</span><a href"#),
            "{sunday}"
        );
        assert!(
            sunday.contains(r#"<span class="when">Non précisé</span>"#),
            "{sunday}"
        );
        assert!(sunday.contains(">Gratin</a>"), "{sunday}");
        assert!(
            html.contains("<strong>1</strong> repas planifié<"),
            "{html}"
        );
    }

    #[test]
    fn two_recipes_on_one_meal_are_both_drawn_and_counted() {
        let planned = [
            planned(1, "Gratin", Some(day(2026, 11, 15)), Some("lunch"), &[]),
            planned(2, "Salade", Some(day(2026, 11, 15)), Some("lunch"), &[]),
        ];
        let html = render(Some(&planned), None);
        assert!(
            html.contains(">Gratin</a>") && html.contains(">Salade</a>"),
            "{html}"
        );
        assert_eq!(
            html.matches(r#"<span class="when">Midi</span>"#).count(),
            9,
            "{html}"
        );
        assert!(
            html.contains("<strong>2</strong> repas planifiés"),
            "{html}"
        );
    }

    #[test]
    fn an_undated_recipe_waits_under_a_placer_with_the_choisir_un_jour_form() {
        let planned = [
            planned(5, "Tarte <aux pommes>", None, None, &[]),
            planned(6, "Chili", None, None, &["Haricots", "Maïs"]),
        ];
        let html = render(Some(&planned), Some(day(2026, 11, 20)));
        assert!(
            html.contains(r#"<h2 id="a-placer">À placer</h2>"#),
            "{html}"
        );
        let place = &html[html.find("À placer").unwrap()..];
        assert!(place.contains("Tarte &lt;aux pommes&gt;"), "{place}");
        assert!(place.contains("4 personnes"), "{place}");
        assert!(place.contains("Il manque 2 éléments</a>"), "{place}");
        assert_eq!(
            place.matches("<summary>Choisir un jour</summary>").count(),
            2,
            "{place}"
        );
        assert!(
            place.contains(&format!(
                r#"action="/recipes/{}/planning""#,
                Uuid::from_u128(5)
            )),
            "{place}"
        );
        // The form comes back to this window.
        assert!(
            place.contains(r#"name="back" value="week@2026-11-20""#),
            "{place}"
        );
        // An undated recipe is no meal of the window, nor of its summary.
        assert!(
            html.contains("Aucun repas planifié cette semaine."),
            "{html}"
        );
    }

    #[test]
    fn a_planning_that_could_not_be_read_is_an_error_not_an_empty_week() {
        let html = render(None, None);
        assert!(
            html.contains(r#"<p class="notice error" role="alert">Impossible de charger les repas de la semaine pour le moment.</p>"#),
            "{html}"
        );
        assert!(!html.contains("Aucun repas planifié"), "{html}");
        assert!(!html.contains("Ajouter"), "{html}");
        // The way around stays.
        assert!(html.contains("7 jours suivants"), "{html}");
    }

    #[test]
    fn the_headings_are_one_per_day_then_a_placer() {
        let planned = [planned(5, "Tarte", None, None, &[])];
        let html = render(Some(&planned), None);
        assert_eq!(html.matches("<h2").count(), 10, "{html}");
        assert!(
            !html.contains("<h1"),
            "the page's title is the handler's: {html}"
        );
        assert!(!html.contains("<h3"), "{html}");
    }
}
