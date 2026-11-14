//! `/agenda/choisir` (#795): choosing the day and the hour of an event in
//! the calendar, from the creation sheet or the edit form.

//!
//! The form's « Choisir dans le calendrier » (`pick_button`) sends it here
//! without JavaScript: a page with the month of the day of Début, each day
//! a button, and that day as the day view draws it (#793), each half hour
//! and its « Journée » banner a button `choix`. The page carries everything
//! the form had sent in hidden fields and posts it back to that form, which
//! comes back with Début and Fin replaced (`with_choice`) and nothing saved.
//! With JavaScript, `enhance.js` reads the same page (`GET`) into a
//! `<dialog>` and fills Début and Fin itself.

use std::collections::BTreeSet;

use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::{Datelike, NaiveDate};
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::validation::agenda::month_grid;
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::icons::{icon, Icon};
use crate::layout::CurrentUser;
use crate::routes::groups::members::fetch_group_detail;
use crate::state::{api_request_auth, AppState};

use super::calendar::FR_MONTHS;
use super::day::{self, day_name, Use};
use super::new::{
    chosen_file, create_body_kind, prefill_param, CreateBody, EventForm, Pick, Prefill, DAY_PARAM,
    FORM_BODY_LIMIT, START_PARAM,
};
use super::{
    agenda_cookie, date_only, family_context_for, form_bounds, occurrences_from_response,
    paris_local_to_utc, read_or_log, to_datetime_local, today_paris, with_family, EventFamily,
};

/// The « Choisir dans le calendrier » button of an event's form, beside
/// the « Journée entière » box: an icon, named by its `aria-label`. A
/// submit button that sends the form to the choice page (`formaction`)
/// without checking it (`formnovalidate`), as « Ajouter des lignes » does
/// on a recipe; `enhance.js` (`data-pick`) opens the choice in a `<dialog>`
/// instead.
pub(crate) fn pick_button(pick: &Pick) -> String {
    format!(
        r#"<button type="submit" class="secondary" formaction="{url}" formnovalidate data-pick aria-haspopup="dialog" aria-label="Choisir dans le calendrier" title="Choisir dans le calendrier">{icon}</button>"#,
        url = html_escape(&pick_url(pick.action, pick.family)),
        icon = icon(Icon::Calendar),
    )
}

/// The form a choice goes back to: the creation sheet or the edit form of
/// an event, as its `action` writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    New,
    Edit(Uuid),
}

/// The form `retour` names, `None` for anything but `/agenda/new` or
/// `/agenda/<id>/edit`, each with its query or none.
pub(crate) fn return_target(retour: &str) -> Option<Target> {
    let path = retour.split_once('?').map_or(retour, |(path, _)| path);
    if retour.contains('#') {
        return None;
    }
    if path == "/agenda/new" {
        return Some(Target::New);
    }
    let id = path.strip_prefix("/agenda/")?.strip_suffix("/edit")?;
    Uuid::parse_str(id).ok().map(Target::Edit)
}

/// What a choice button sent (`choix`): a half hour, a whole day, or
/// nothing — « Retour au formulaire sans changer », or a value that does
/// not read. A half hour reads as the sheet's `?debut=` does, a Paris wall
/// clock that exists (`prefill_param`).
pub(crate) fn read_choice(value: &str) -> Option<Prefill> {
    let key = if value.contains('T') {
        START_PARAM
    } else {
        DAY_PARAM
    };
    prefill_param(serde_urlencoded::to_string([(key, value)]).ok().as_deref())
}

/// The « Journée entière » box, Début and Fin once `choice` is made over
/// the form's `starts` and `ends`. A half hour keeps the length the form
/// already had, in elapsed time — an hour when it has none to keep: a Fin
/// not after Début, a field that does not read, a form of whole days. A day
/// ticks the box, Début and Fin on it.
pub(crate) fn picked_fields(choice: Prefill, starts: &str, ends: &str) -> (bool, String, String) {
    let at = match choice {
        Prefill::Day(day) => {
            let day = day.format("%Y-%m-%d").to_string();
            return (true, day.clone(), day);
        }
        Prefill::At(at) => at,
    };
    let hour = chrono::Duration::hours(1);
    let length = match (date_only(starts), form_bounds(starts, ends)) {
        (None, Some((s, e))) if e > s => e - s,
        _ => hour,
    };
    let starts = at.format("%Y-%m-%dT%H:%M").to_string();
    let ends = paris_local_to_utc(&starts)
        .map(|s| to_datetime_local(s + length))
        .filter(|ends| *ends > starts)
        .unwrap_or_else(|| (at + length).format("%Y-%m-%dT%H:%M").to_string());
    (false, starts, ends)
}

/// The form back from the choice page: what it carried, Début and Fin and
/// the « Journée entière » box replaced by the choice (`picked_fields`).
/// Untouched without a choice that reads.
pub(crate) fn with_choice(form: EventForm) -> EventForm {
    let Some(choice) = form.choix.as_deref().and_then(read_choice) else {
        return form;
    };
    let (all_day, starts_at, ends_at) = picked_fields(choice, &form.starts_at, &form.ends_at);
    EventForm {
        all_day: all_day.then(|| "on".to_string()),
        starts_at,
        ends_at,
        ..form
    }
}

/// Whether the form comes back from the choice page with a choice made.
pub(crate) fn was_picked(form: &EventForm) -> bool {
    form.choix.as_deref().and_then(read_choice).is_some()
}

/// What the form back from the choice page says above its fields: that
/// Début and Fin were replaced, and that the file chosen before was not
/// kept. Nothing for a form that does not come back from it.
pub(crate) fn returned_notices(form: &EventForm) -> String {
    let Some(choice) = form.choix.as_deref() else {
        return String::new();
    };
    let replaced = match read_choice(choice) {
        Some(Prefill::At(_)) => {
            r#"<p class="notice success" role="status">Début et Fin ont été remplacés par le créneau choisi.</p>"#
        }
        Some(Prefill::Day(_)) => {
            r#"<p class="notice success" role="status">Début et Fin ont été remplacés par le jour choisi.</p>"#
        }
        None => "",
    };
    let lost = if form.fichier_perdu.is_some() {
        r#"<p class="notice warning" role="status"><strong>Pièce jointe non conservée.</strong> Vous aviez joint un fichier : rechoisissez-le.</p>"#
    } else {
        ""
    };
    format!("{replaced}{lost}")
}

/// The choice page of the form whose `action` is `retour`, in `family`.
pub(crate) fn pick_url(retour: &str, family: Option<Uuid>) -> String {
    let query = serde_urlencoded::to_string([("retour", retour)]).unwrap_or_default();
    with_family(&format!("/agenda/choisir?{query}"), family)
}

/// The day the choice opens on: `jour` when it reads, else the day of
/// Début, else `today`.
pub(crate) fn opening_day(jour: Option<&str>, starts: &str, today: NaiveDate) -> NaiveDate {
    jour.and_then(date_only)
        .or_else(|| starts.get(..10).and_then(date_only))
        .unwrap_or(today)
}

// -- the page ------------------------------------------------------------------

/// The first day of the month before `day`'s and of the month after it.
fn neighbour_months(day: NaiveDate) -> (NaiveDate, NaiveDate) {
    let first = day.with_day(1).unwrap_or(day);
    let previous = (first - chrono::Duration::days(1))
        .with_day(1)
        .unwrap_or(first);
    let next = (first + chrono::Duration::days(31))
        .with_day(1)
        .unwrap_or(first);
    (previous, next)
}

/// What the month of a choice draws of each day.
pub(crate) struct MonthMarks<'a> {
    /// The day chosen: its recap is shown below (`aria-pressed`).
    pub chosen: NaiveDate,
    /// The day of the form's Début, outlined when another one is chosen.
    pub start: Option<NaiveDate>,
    pub today: NaiveDate,
    /// The days with at least one event: a dot, and words for a reader.
    pub busy: &'a BTreeSet<NaiveDate>,
    /// The choice page's own URL, which each day's button posts to.
    pub action: &'a str,
}

/// The month of `marks.chosen` (#795): ‹ and › to the months around it, its
/// name, then a button per day, Monday first, each sending the form back to
/// the choice page (`formaction`) on that day (`jour`). The week-end is
/// tinted as in the grid; today is outlined; the day of Début, when another
/// is chosen, outlined thicker. `enhance.js` moves through the days with
/// the arrow keys.
pub(crate) fn month_html(marks: &MonthMarks) -> String {
    let chosen = marks.chosen;
    let (previous, next) = neighbour_months(chosen);
    let action = html_escape(marks.action);
    let name = format!(
        "{} {}",
        FR_MONTHS[(chosen.month() - 1) as usize],
        chosen.year()
    );
    let arrow = |day: NaiveDate, label: &str, icon_of: Icon| {
        format!(
            r#"<button type="submit" class="secondary" formaction="{action}" name="jour" value="{day}" aria-label="{label}">{}</button>"#,
            icon(icon_of)
        )
    };
    let first = chosen.with_day(1).unwrap_or(chosen);
    let blanks = "<span></span>".repeat(first.weekday().num_days_from_monday() as usize);
    let days: String = first
        .iter_days()
        .take_while(|d| d.month() == chosen.month())
        .map(|d| {
            let mut class = Vec::new();
            if d.weekday().num_days_from_monday() >= 5 {
                class.push("we");
            }
            if marks.start == Some(d) && d != chosen {
                class.push("start");
            }
            let busy = marks.busy.contains(&d);
            if busy {
                class.push("busy");
            }
            let class = if class.is_empty() {
                String::new()
            } else {
                format!(r#" class="{}""#, class.join(" "))
            };
            let mut words = String::new();
            if busy {
                words.push_str(", des événements");
            }
            if marks.start == Some(d) {
                words.push_str(", jour du début");
            }
            let words = if words.is_empty() {
                words
            } else {
                format!(r#"<span class="sr-only">{words}</span>"#)
            };
            format!(
                r#"<button type="submit"{class} formaction="{action}" name="jour" value="{d}"{pressed}{today}>{n}{words}</button>"#,
                pressed = if d == chosen { r#" aria-pressed="true""# } else { "" },
                today = if d == marks.today { r#" aria-current="date""# } else { "" },
                n = d.day(),
            )
        })
        .collect();
    format!(
        r#"<div class="split">{prev}<h2>{name}</h2>{next}</div>
<div class="mm" role="group" aria-label="{name}"><span aria-hidden="true">L</span><span aria-hidden="true">M</span><span aria-hidden="true">M</span><span aria-hidden="true">J</span><span aria-hidden="true">V</span><span aria-hidden="true">S</span><span aria-hidden="true">D</span>{blanks}{days}</div>"#,
        prev = arrow(previous, "Mois précédent", Icon::Previous),
        next = arrow(next, "Mois suivant", Icon::Next),
    )
}

/// What the recap of a day says under its name: how many events it holds,
/// the one the form edits left out — it is no conflict with itself.
pub(crate) fn count_line(count: usize) -> String {
    match count {
        0 => "Rien de prévu".to_string(),
        n => format!("{n} au programme"),
    }
}

/// The page's form (#795): it posts to the form it goes back to (`retour`),
/// carrying everything that form had sent in hidden fields (`carried`).
/// `chooser` is what goes inside — the part a `<dialog>` shows with
/// JavaScript (`data-pick-body`).
fn page_form(retour: &str, carried: &[(String, String)], chooser: &str) -> String {
    let hidden: String = carried
        .iter()
        .map(|(name, value)| {
            format!(
                r#"<input type="hidden" name="{}" value="{}"/>"#,
                html_escape(name),
                html_escape(value)
            )
        })
        .collect();
    format!(
        r#"<form method="post" action="{retour}" class="card sheet">{hidden}
{chooser}
</form>"#,
        retour = html_escape(retour),
    )
}

/// Everything the choice shows of a day and its month.
struct Chooser<'a> {
    family: &'a str,
    month: String,
    day: NaiveDate,
    count: usize,
    load_error: bool,
    banner: String,
    hours: String,
}

/// The inside of the page (#795), which `enhance.js` puts in a `<dialog>`:
/// the family over the title and a cross, the month, the day's name and
/// how many events it holds, its « Journée » banner and its hours (as the
/// day view draws them, `day::Use::Pick`), a line `enhance.js` fills with
/// what a half hour would give, and the way back without a change. The
/// cross is that way back too.
fn chooser_html(c: &Chooser) -> String {
    let error = if c.load_error {
        r#"<p class="notice error">Impossible de charger les événements pour le moment.</p>"#
    } else {
        ""
    };
    format!(
        r#"<div class="chooser" data-pick-body>
<div class="page-header"><hgroup><p class="eyebrow">{family}</p><h1>Choisir dans le calendrier</h1></hgroup><button type="submit" class="secondary" name="choix" value="" aria-label="Fermer">{close}</button></div>
{error}{month}
<div class="split"><h2>{name}</h2><p class="muted">{count}</p></div>
{banner}{hours}
<p class="muted" data-pick-status aria-live="polite"></p>
<div class="actions"><button type="submit" class="secondary" name="choix" value="">Retour au formulaire sans changer</button></div>
</div>"#,
        family = html_escape(c.family),
        close = icon(Icon::Close),
        month = c.month,
        name = day_name(c.day),
        count = count_line(c.count),
        banner = c.banner,
        hours = c.hours,
    )
}

/// What the page is asked: the form it goes back to, the day to show, the
/// form's Début (a date or a wall clock, for the outline of its day).
#[derive(serde::Deserialize, Default)]
struct PickQuery {
    #[serde(default)]
    retour: String,
    #[serde(default)]
    jour: Option<String>,
    #[serde(default)]
    debut: Option<String>,
}

fn pick_query(raw: Option<&str>) -> PickQuery {
    raw.and_then(|q| serde_urlencoded::from_str(q).ok())
        .unwrap_or_default()
}

/// `GET /agenda/choisir`: what `enhance.js` reads into the `<dialog>`, the
/// day and the Début it is asked in the URL, nothing carried.
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    EventFamily(requested): EventFamily,
    RawQuery(raw): RawQuery,
) -> Response {
    let query = pick_query(raw.as_deref());
    let starts = query.debut.clone().unwrap_or_default();
    choice_page(
        &state,
        &headers,
        &me,
        requested,
        &query,
        query.jour.as_deref(),
        &starts,
        &[],
    )
    .await
}

/// `POST /agenda/choisir`: the form an event's « Choisir dans le
/// calendrier » sent without JavaScript — multipart from the sheet, a plain
/// form from the edit page or from this page's own days. Its fields are
/// carried on; a file is not, and the form back says so (`fichier_perdu`).
pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    EventFamily(requested): EventFamily,
    RawQuery(raw): RawQuery,
    request: axum::extract::Request,
) -> Response {
    let query = pick_query(raw.as_deref());
    let Some(kind) = create_body_kind(&headers) else {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    };
    let (mut pairs, had_file) = match read_fields(kind, &state, request).await {
        Ok(read) => read,
        Err(FieldsError::TooLarge) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        Err(FieldsError::Broken) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let jour = pairs
        .iter()
        .find(|(k, _)| k == "jour")
        .map(|(_, v)| v.clone());
    pairs.retain(|(k, _)| k != "jour" && k != "choix");
    if had_file && !pairs.iter().any(|(k, _)| k == "fichier_perdu") {
        pairs.push(("fichier_perdu".to_string(), "1".to_string()));
    }
    let starts = pairs
        .iter()
        .find(|(k, _)| k == "starts_at")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    choice_page(
        &state,
        &headers,
        &me,
        requested,
        &query,
        jour.as_deref(),
        &starts,
        &pairs,
    )
    .await
}

/// Why a body could not be read.
enum FieldsError {
    TooLarge,
    Broken,
}

/// The text fields of a body, in order, and whether it carried a file. The
/// file's bytes are read and dropped as they come: the choice page keeps
/// no file. The text fields together are held to the plain form's limit.
async fn read_fields(
    kind: CreateBody,
    state: &AppState,
    request: axum::extract::Request,
) -> Result<(Vec<(String, String)>, bool), FieldsError> {
    use axum::extract::FromRequest;
    if kind == CreateBody::Form {
        let bytes = axum::body::to_bytes(request.into_body(), FORM_BODY_LIMIT)
            .await
            .map_err(|_| FieldsError::TooLarge)?;
        let pairs = serde_urlencoded::from_bytes(&bytes).map_err(|_| FieldsError::Broken)?;
        return Ok((pairs, false));
    }
    let broken = |_| FieldsError::Broken;
    let mut multipart = axum::extract::Multipart::from_request(request, state)
        .await
        .map_err(|_| FieldsError::Broken)?;
    let mut pairs = Vec::new();
    let mut had_file = false;
    let mut text_left = FORM_BODY_LIMIT;
    while let Some(mut field) = multipart.next_field().await.map_err(broken)? {
        let name = field.name().unwrap_or_default().to_string();
        if name == "file" {
            had_file |= chosen_file(field.file_name()).is_some();
            while field.chunk().await.map_err(broken)?.is_some() {}
            continue;
        }
        text_left = text_left
            .checked_sub(name.len())
            .ok_or(FieldsError::TooLarge)?;
        let mut value = Vec::new();
        while let Some(chunk) = field.chunk().await.map_err(broken)? {
            text_left = text_left
                .checked_sub(chunk.len())
                .ok_or(FieldsError::TooLarge)?;
            value.extend_from_slice(&chunk);
        }
        pairs.push((
            name,
            String::from_utf8(value).map_err(|_| FieldsError::Broken)?,
        ));
    }
    Ok((pairs, had_file))
}

/// The page: the month of the day asked, its events of the form's family —
/// and of it alone —, read from a week before the month's grid, as the day
/// view reads them (`DAY_LOOKBACK`).
#[allow(clippy::too_many_arguments)]
async fn choice_page(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    requested: Option<Uuid>,
    query: &PickQuery,
    jour: Option<&str>,
    starts: &str,
    carried: &[(String, String)],
) -> Response {
    let Some(target) = return_target(&query.retour) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(fam) = family_context_for(state, headers, me, "/agenda/choisir", requested).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let today = today_paris();
    let day = opening_day(jour, starts, today);
    let grid = month_grid(day.year(), day.month());
    let (Some(first), Some(last)) = (grid.first(), grid.last()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (Some(from), Some(to)) = (
        paris_local_to_utc(&format!("{}T00:00", *first - chrono::Duration::days(7))),
        paris_local_to_utc(&format!("{last}T23:59:59")),
    ) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let cookie = agenda_cookie(headers);
    let path = format!(
        "/groups/{}/events?from={}&to={}",
        fam.gid,
        from.format("%Y-%m-%dT%H:%M:%SZ"),
        to.format("%Y-%m-%dT%H:%M:%SZ"),
    );
    let (events, roster) = tokio::join!(
        api_request_auth(state, reqwest::Method::GET, &path, cookie.as_deref(), None),
        fetch_group_detail(state, cookie.as_deref(), fam.gid),
    );
    let occurrences = events.ok().and_then(|resp| {
        read_or_log(
            occurrences_from_response(resp.status, resp.body),
            fam.gid,
            "agenda choice",
        )
    });
    let members = roster.ok().flatten().map(|g| g.members).unwrap_or_default();
    let all = occurrences.clone().unwrap_or_default();

    let editing = match target {
        Target::New => None,
        Target::Edit(id) => Some(id),
    };
    let usage = Use::Pick(editing);
    let busy: BTreeSet<NaiveDate> = grid
        .iter()
        .copied()
        .filter(|d| d.month() == day.month())
        .filter(|d| {
            all.iter().any(|o| {
                day::place(
                    o.event.all_day,
                    o.occurrence_starts_at,
                    o.occurrence_ends_at,
                    *d,
                )
                .is_some()
            })
        })
        .collect();
    let mut banner = Vec::new();
    let mut blocks = Vec::new();
    for occ in &all {
        match day::place(
            occ.event.all_day,
            occ.occurrence_starts_at,
            occ.occurrence_ends_at,
            day,
        ) {
            Some(day::Place::Banner) => banner.push(occ),
            Some(day::Place::Hours(clip)) => blocks.push((occ, clip)),
            None => {}
        }
    }
    let count = banner
        .iter()
        .copied()
        .chain(blocks.iter().map(|(o, _)| *o))
        .filter(|o| Some(o.event.id) != editing)
        .count();
    let action = pick_url(&query.retour, fam.carried);
    let chooser = chooser_html(&Chooser {
        family: &fam.name,
        month: month_html(&MonthMarks {
            chosen: day,
            start: starts.get(..10).and_then(date_only),
            today,
            busy: &busy,
            action: &action,
        }),
        day,
        count,
        load_error: occurrences.is_none(),
        banner: day::banner_for(day, &banner, &[], &members, &[], usage),
        hours: day::hours_for(
            day,
            &blocks,
            &members,
            &[],
            Some(chrono::Utc::now()),
            occurrences.is_some(),
            usage,
        ),
    });
    Html(shell_with_header(
        Width::Form,
        "Choisir dans le calendrier",
        &fam.header,
        &page_form(&query.retour, carried, &chooser),
    ))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;

    fn at(value: &str) -> Prefill {
        Prefill::At(NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M").unwrap())
    }

    fn day(value: &str) -> NaiveDate {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
    }

    // -- return_target ------------------------------------------------------

    #[test]
    fn the_sheet_and_an_edit_form_are_targets() {
        assert_eq!(return_target("/agenda/new"), Some(Target::New));
        assert_eq!(
            return_target("/agenda/new?debut=2026-11-12T09:30&famille=1"),
            Some(Target::New)
        );
        let id = Uuid::from_u128(9);
        assert_eq!(
            return_target(&format!("/agenda/{id}/edit")),
            Some(Target::Edit(id))
        );
        assert_eq!(
            return_target(&format!("/agenda/{id}/edit?famille={id}")),
            Some(Target::Edit(id))
        );
    }

    #[test]
    fn anything_else_is_no_target() {
        let id = Uuid::from_u128(9);
        for retour in [
            "",
            "/agenda",
            "/agenda/newer",
            "/agenda/new/../../logout",
            "//evil.example/agenda/new",
            "https://evil.example/agenda/new",
            "/agenda/not-an-id/edit",
            &format!("/agenda/{id}/delete"),
            &format!("/agenda/{id}/edit/x"),
            "/agenda/new#x",
        ] {
            assert_eq!(return_target(retour), None, "{retour}");
        }
    }

    // -- read_choice ----------------------------------------------------------

    #[test]
    fn a_half_hour_or_a_day_is_a_choice() {
        assert_eq!(
            read_choice("2026-11-12T14:00"),
            Some(at("2026-11-12T14:00"))
        );
        assert_eq!(
            read_choice("2026-11-12"),
            Some(Prefill::Day(day("2026-11-12")))
        );
    }

    #[test]
    fn nothing_or_an_unreadable_value_changes_nothing() {
        for value in [
            "",
            "x",
            "2026-13-01",
            "2026-11-12T25:00",
            "2026-11-12 14:00",
        ] {
            assert_eq!(read_choice(value), None, "{value}");
        }
        // An hour the clocks skip, as `prefill_param` reads it.
        assert_eq!(read_choice("2026-03-29T02:30"), None);
    }

    // -- picked_fields ----------------------------------------------------------

    #[test]
    fn a_half_hour_keeps_the_length_already_typed() {
        // Two hours, moved to 14:00 another day.
        assert_eq!(
            picked_fields(
                at("2026-11-20T14:00"),
                "2026-11-12T09:00",
                "2026-11-12T11:00"
            ),
            (
                false,
                "2026-11-20T14:00".to_string(),
                "2026-11-20T16:00".to_string()
            )
        );
        // Across midnight.
        assert_eq!(
            picked_fields(
                at("2026-11-20T23:30"),
                "2026-11-12T09:00",
                "2026-11-12T10:30"
            ),
            (
                false,
                "2026-11-20T23:30".to_string(),
                "2026-11-21T01:00".to_string()
            )
        );
    }

    #[test]
    fn a_half_hour_lasts_an_hour_without_a_length_to_keep() {
        let hour = |starts: &str, ends: &str| picked_fields(at("2026-11-20T14:00"), starts, ends);
        let expected = (
            false,
            "2026-11-20T14:00".to_string(),
            "2026-11-20T15:00".to_string(),
        );
        // Fin before Début, equal to it, empty, unreadable.
        assert_eq!(hour("2026-11-12T09:00", "2026-11-12T08:00"), expected);
        assert_eq!(hour("2026-11-12T09:00", "2026-11-12T09:00"), expected);
        assert_eq!(hour("2026-11-12T09:00", ""), expected);
        assert_eq!(hour("", "x"), expected);
        // A form of whole days: an hour, the box unticked.
        assert_eq!(hour("2026-11-12", "2026-11-14"), expected);
    }

    #[test]
    fn a_half_hour_counts_its_length_in_elapsed_time() {
        // The night the clocks go forward, two hours from 01:00 end at
        // 04:00 by the clock.
        assert_eq!(
            picked_fields(
                at("2026-03-29T01:00"),
                "2026-11-12T09:00",
                "2026-11-12T11:00"
            ),
            (
                false,
                "2026-03-29T01:00".to_string(),
                "2026-03-29T04:00".to_string()
            )
        );
    }

    #[test]
    fn a_day_ticks_the_box_on_that_day() {
        assert_eq!(
            picked_fields(
                Prefill::Day(day("2026-11-20")),
                "2026-11-12T09:00",
                "2026-11-12T11:00"
            ),
            (true, "2026-11-20".to_string(), "2026-11-20".to_string())
        );
    }

    // -- with_choice, was_picked, returned_notices ------------------------------

    fn returned(body: &str) -> EventForm {
        serde_urlencoded::from_str(body).unwrap()
    }

    const TYPED: &str = "title=Dentiste&description=Carnet&starts_at=2026-11-12T09:00\
        &ends_at=2026-11-12T11:00&freq=weekly&reminder=60&assignee_ids=x";

    #[test]
    fn a_half_hour_chosen_replaces_debut_and_fin_and_keeps_the_rest() {
        let form = with_choice(returned(&format!("{TYPED}&choix=2026-11-20T14%3A00")));
        assert_eq!(form.starts_at, "2026-11-20T14:00");
        assert_eq!(form.ends_at, "2026-11-20T16:00");
        assert!(form.all_day.is_none());
        assert_eq!(form.title, "Dentiste");
        assert_eq!(form.description, "Carnet");
        assert_eq!(form.freq, "weekly");
        assert_eq!(form.reminder, "60");
    }

    #[test]
    fn a_day_chosen_ticks_the_box() {
        let form = with_choice(returned(&format!("{TYPED}&choix=2026-11-20")));
        assert_eq!(form.all_day.as_deref(), Some("on"));
        assert_eq!(
            (form.starts_at.as_str(), form.ends_at.as_str()),
            ("2026-11-20", "2026-11-20")
        );
    }

    #[test]
    fn coming_back_without_a_choice_changes_nothing() {
        for body in [
            format!("{TYPED}&choix="),
            format!("{TYPED}&choix=x"),
            TYPED.to_string(),
        ] {
            let form = with_choice(returned(&body));
            assert_eq!(form.starts_at, "2026-11-12T09:00", "{body}");
            assert_eq!(form.ends_at, "2026-11-12T11:00", "{body}");
            assert!(!was_picked(&form), "{body}");
        }
        assert!(was_picked(&returned(&format!("{TYPED}&choix=2026-11-20"))));
    }

    #[test]
    fn the_form_back_says_what_changed_and_the_file_lost() {
        let picked = returned_notices(&returned(&format!("{TYPED}&choix=2026-11-20T14%3A00")));
        assert!(
            picked.contains("Début et Fin ont été remplacés par le créneau choisi."),
            "{picked}"
        );
        assert!(!picked.contains("Pièce jointe"), "{picked}");
        let day = returned_notices(&returned(&format!("{TYPED}&choix=2026-11-20")));
        assert!(day.contains("par le jour choisi"), "{day}");
        let lost = returned_notices(&returned(&format!("{TYPED}&choix=&fichier_perdu=1")));
        assert!(!lost.contains("remplacés"), "{lost}");
        assert!(lost.contains(r#"class="notice warning""#), "{lost}");
        assert!(lost.contains("rechoisissez-le"), "{lost}");
        // A form that does not come back from the page says nothing, a
        // forged marker included.
        assert_eq!(returned_notices(&returned(TYPED)), "");
        assert_eq!(
            returned_notices(&returned(&format!("{TYPED}&fichier_perdu=1"))),
            ""
        );
    }

    // -- pick_url -----------------------------------------------------------------

    #[test]
    fn the_choice_page_carries_the_form_back_and_its_family() {
        assert_eq!(
            pick_url("/agenda/new?debut=2026-11-12T09:30", None),
            "/agenda/choisir?retour=%2Fagenda%2Fnew%3Fdebut%3D2026-11-12T09%3A30"
        );
        let g = Uuid::from_u128(2);
        assert_eq!(
            pick_url("/agenda/new", Some(g)),
            format!("/agenda/choisir?retour=%2Fagenda%2Fnew&famille={g}")
        );
    }

    // -- the page --------------------------------------------------------------

    fn month(chosen: &str, start: Option<&str>, busy: &[&str]) -> String {
        let busy: BTreeSet<NaiveDate> = busy.iter().map(|d| day(d)).collect();
        month_html(&MonthMarks {
            chosen: day(chosen),
            start: start.map(day),
            today: day("2026-11-14"),
            busy: &busy,
            action: "/agenda/choisir?retour=%2Fagenda%2Fnew",
        })
    }

    #[test]
    fn the_month_is_a_button_per_day_from_monday() {
        let html = month("2026-11-12", Some("2026-11-12"), &[]);
        // November 2026 opens on a Sunday: six blanks after the weekdays.
        assert!(
            html.contains(r#"<span aria-hidden="true">D</span><span></span><span></span><span></span><span></span><span></span><span></span><button"#),
            "{html}"
        );
        assert_eq!(html.matches(r#"name="jour""#).count(), 32, "{html}");
        assert!(
            html.contains(r#"<button type="submit" class="secondary" formaction="/agenda/choisir?retour=%2Fagenda%2Fnew" name="jour" value="2026-10-01" aria-label="Mois précédent">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"value="2026-12-01" aria-label="Mois suivant""#),
            "{html}"
        );
        assert!(html.contains("<h2>novembre 2026</h2>"), "{html}");
        assert!(
            html.contains(r#"role="group" aria-label="novembre 2026""#),
            "{html}"
        );
    }

    #[test]
    fn the_month_marks_the_chosen_day_today_the_start_and_the_busy_days() {
        let html = month("2026-11-20", Some("2026-11-12"), &["2026-11-03"]);
        assert!(
            html.contains(r#"value="2026-11-20" aria-pressed="true">20</button>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<button type="submit" class="start" formaction="/agenda/choisir?retour=%2Fagenda%2Fnew" name="jour" value="2026-11-12">12<span class="sr-only">, jour du début</span></button>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<button type="submit" class="we" formaction="/agenda/choisir?retour=%2Fagenda%2Fnew" name="jour" value="2026-11-14" aria-current="date">14</button>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"class="busy" formaction="/agenda/choisir?retour=%2Fagenda%2Fnew" name="jour" value="2026-11-03">3<span class="sr-only">, des événements</span>"#),
            "{html}"
        );
        assert_eq!(html.matches("aria-pressed").count(), 1, "{html}");
        // The start chosen is filled, not outlined.
        let same = month("2026-11-12", Some("2026-11-12"), &[]);
        assert!(!same.contains("start"), "{same}");
    }

    #[test]
    fn the_month_goes_to_the_months_around_it_across_a_year() {
        let html = month("2027-01-31", None, &[]);
        assert!(
            html.contains(r#"value="2026-12-01" aria-label="Mois précédent""#),
            "{html}"
        );
        assert!(
            html.contains(r#"value="2027-02-01" aria-label="Mois suivant""#),
            "{html}"
        );
        assert_eq!(html.matches(r#"name="jour""#).count(), 33, "{html}");
    }

    #[test]
    fn the_page_carries_what_the_form_sent_back_to_it() {
        let html = page_form(
            "/agenda/new?debut=2026-11-12T09:30&famille=x",
            &[
                ("title".to_string(), r#"Dîner "chez" <Léa>"#.to_string()),
                ("assignee_ids".to_string(), "a".to_string()),
                ("assignee_ids".to_string(), "b".to_string()),
            ],
            "<div></div>",
        );
        assert!(
            html.starts_with(r#"<form method="post" action="/agenda/new?debut=2026-11-12T09:30&amp;famille=x" class="card sheet">"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<input type="hidden" name="title" value="Dîner &quot;chez&quot; &lt;Léa&gt;"/>"#
            ),
            "{html}"
        );
        assert_eq!(html.matches(r#"name="assignee_ids""#).count(), 2, "{html}");
    }

    #[test]
    fn the_day_says_how_many_events_it_holds() {
        assert_eq!(count_line(0), "Rien de prévu");
        assert_eq!(count_line(3), "3 au programme");
    }

    #[test]
    fn the_button_sits_beside_the_all_day_box_and_marks_a_choice_made() {
        use super::super::new::schedule_fields;
        let pick = |picked| Pick {
            action: "/agenda/new",
            family: None,
            picked,
        };
        let html = schedule_fields(
            false,
            "2026-11-12T09:00",
            "2026-11-12T10:00",
            Some(&pick(false)),
        );
        assert!(
            html.starts_with(r#"<div class="split"><label class="field inline">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"Journée entière</label><button type="submit""#),
            "{html}"
        );
        assert!(!html.contains("picked"), "{html}");
        let html = schedule_fields(
            false,
            "2026-11-12T09:00",
            "2026-11-12T10:00",
            Some(&pick(true)),
        );
        assert_eq!(html.matches(r#" class="picked""#).count(), 2, "{html}");
    }

    #[test]
    fn the_enter_key_still_saves() {
        // The first submit button of the form, before « Choisir dans le
        // calendrier », saves.
        let html = super::super::new::default_button("Enregistrer");
        assert_eq!(html, r#"<button type="submit" hidden>Enregistrer</button>"#);
    }

    #[test]
    fn the_button_sends_the_form_to_the_choice_without_checking_it() {
        let html = pick_button(&Pick {
            action: "/agenda/new?debut=2026-11-12T09:30",
            family: None,
            picked: false,
        });
        assert!(
            html.starts_with(r#"<button type="submit" class="secondary" formaction="/agenda/choisir?retour=%2Fagenda%2Fnew%3Fdebut%3D2026-11-12T09%3A30" formnovalidate data-pick aria-haspopup="dialog" aria-label="Choisir dans le calendrier""#),
            "{html}"
        );
        assert!(html.contains(r#"<svg class="icon""#), "{html}");
    }

    // -- opening_day ----------------------------------------------------------

    #[test]
    fn the_choice_opens_on_the_day_asked_else_on_debut_else_today() {
        let today = day("2026-11-14");
        assert_eq!(
            opening_day(Some("2026-12-01"), "2026-11-12T09:00", today),
            day("2026-12-01")
        );
        assert_eq!(
            opening_day(None, "2026-11-12T09:00", today),
            day("2026-11-12")
        );
        assert_eq!(
            opening_day(Some("x"), "2026-11-12", today),
            day("2026-11-12")
        );
        assert_eq!(opening_day(None, "", today), today);
        assert_eq!(opening_day(Some(""), "garbage", today), today);
    }
}
