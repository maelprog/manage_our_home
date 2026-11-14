//! `/agenda/choisir` (#795): choosing the day and the hour of an event in
//! the calendar, from the creation sheet or the edit form.
//!
//! The form's « Choisir dans le calendrier » (`pick_button`) sends it here
//! without JavaScript: a page with the month of the day of Début, each day
//! a link, then that day's « Journée » banner, each entry a button `choix`
//! for the whole day, and a button `choix` per half hour, the events of the
//! day as lines among them. The page carries everything the form had sent,
//! in hidden fields and in the links of its days, and posts it back to that
//! form, which comes back with Début and Fin replaced (`with_choice`) and
//! nothing saved. With JavaScript, `enhance.js` reads the page as a
//! `<dialog>` draws it (`GET`, `dialogue=1`) — the day as the day view
//! draws it (#793), each half hour a button — and fills Début and Fin
//! itself.

use std::collections::BTreeSet;

use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::{Datelike, NaiveDate};
use manage_our_home_shared::dto::agenda::OccurrenceResponse;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::GroupMember;
use manage_our_home_shared::validation::agenda::month_grid;
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::family::accent_of;
use crate::icons::{icon, icon_link, Icon};
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
    agenda_cookie, date_only, family_context_choosing, form_bounds, occurrences_from_response,
    paris_local_to_utc, read_or_log, to_datetime_local, today_paris, with_family, EventFamily,
    DISPLAY_TZ,
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
            r#"<p class="notice" role="status">Début et Fin ont été remplacés par le créneau choisi.</p>"#
        }
        Some(Prefill::Day(_)) => {
            r#"<p class="notice" role="status">Début et Fin ont été remplacés par le jour choisi.</p>"#
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

/// How the choice is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    /// The `<dialog>` of `enhance.js`: a head and a foot that stay, the
    /// day as the day view draws it, a line that says what a half hour
    /// would give; its days and months are buttons the script reads.
    Dialog,
    /// The page without JavaScript: its days and months are links, its
    /// half hours a list of buttons of 44 px.
    Page,
}

/// What the month of a choice draws of each day.
pub(crate) struct MonthMarks<'a> {
    /// The day chosen: its recap is shown below.
    pub chosen: NaiveDate,
    /// The day of the form's Début, outlined when another one is chosen.
    pub start: Option<NaiveDate>,
    pub today: NaiveDate,
    /// The days with at least one event: a dot, and words for a reader.
    pub busy: &'a BTreeSet<NaiveDate>,
    /// The choice page's own URL, which each day's link goes to.
    pub action: &'a str,
    /// The form's fields, which each day's link carries (`day_link`).
    pub carried: &'a [(String, String)],
}

/// The month of `marks.chosen` (#795): ‹ and › to the months around it, its
/// name, then the days, Monday first, those of the month before written in
/// `--muted`. Each day of the month is a link to the choice on that day
/// (`day_link`) on the page, a button `jour` that `enhance.js` reads in the
/// dialog, where it moves through them with the arrow keys. The week-end
/// is tinted as in the grid; today is outlined; the day of Début, when
/// another is chosen, outlined thicker; the day chosen filled.
pub(crate) fn month_html(marks: &MonthMarks, shape: Shape) -> String {
    let chosen = marks.chosen;
    let (previous, next) = neighbour_months(chosen);
    let link = |day: NaiveDate| html_escape(&day_link(marks.action, marks.carried, day));
    let name = format!(
        "{} {}",
        FR_MONTHS[(chosen.month() - 1) as usize],
        chosen.year()
    );
    let arrow = |day: NaiveDate, label: &str, icon_of: Icon| match shape {
        Shape::Page => icon_link("btn secondary", &link(day), label, icon_of),
        Shape::Dialog => format!(
            r#"<button type="button" class="secondary" name="jour" value="{day}" aria-label="{label}">{}</button>"#,
            icon(icon_of)
        ),
    };
    let first = chosen.with_day(1).unwrap_or(chosen);
    let before: String = (1..=i64::from(first.weekday().num_days_from_monday()))
        .rev()
        .map(|back| {
            let d = first - chrono::Duration::days(back);
            format!(r#"<span aria-hidden="true">{}</span>"#, d.day())
        })
        .collect();
    let days: String = first
        .iter_days()
        .take_while(|d| d.month() == chosen.month())
        .map(|d| {
            let busy = marks.busy.contains(&d);
            let class: Vec<&str> = [
                (d.weekday().num_days_from_monday() >= 5, "we"),
                (d == marks.today, "today"),
                (marks.start == Some(d) && d != chosen, "start"),
                (d == chosen, "sel"),
                (busy, "busy"),
            ]
            .into_iter()
            .filter_map(|(on, name)| on.then_some(name))
            .collect();
            let class = if class.is_empty() {
                String::new()
            } else {
                format!(r#" class="{}""#, class.join(" "))
            };
            let words: String = [
                (d == marks.today && shape == Shape::Page, ", aujourd'hui"),
                (busy, ", des événements"),
                (marks.start == Some(d), ", jour du début"),
            ]
            .into_iter()
            .filter_map(|(on, said)| on.then_some(said))
            .collect();
            let words = if words.is_empty() {
                words
            } else {
                format!(r#"<span class="sr-only">{words}</span>"#)
            };
            let n = d.day();
            match shape {
                Shape::Page => format!(
                    r#"<a href="{href}"{class}{current}>{n}{words}</a>"#,
                    href = link(d),
                    current = if d == chosen {
                        r#" aria-current="true""#
                    } else {
                        ""
                    },
                ),
                Shape::Dialog => format!(
                    r#"<button type="button"{class} name="jour" value="{d}"{pressed}{today}>{n}{words}</button>"#,
                    pressed = if d == chosen {
                        r#" aria-pressed="true""#
                    } else {
                        ""
                    },
                    today = if d == marks.today {
                        r#" aria-current="date""#
                    } else {
                        ""
                    },
                ),
            }
        })
        .collect();
    let heading = match shape {
        Shape::Page => "h2",
        Shape::Dialog => "h3",
    };
    let dow: String = ["L", "M", "M", "J", "V", "S", "D"]
        .iter()
        .map(|l| format!(r#"<span class="dow" aria-hidden="true">{l}</span>"#))
        .collect();
    format!(
        r#"<div class="split">{prev}<{heading}>{name}</{heading}>{next}</div>
<div class="mm" role="group" aria-label="{name}">{dow}{before}{days}</div>"#,
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

/// The half hours of `day` for a choice without JavaScript (#795): a button
/// `choix` each, 44 px tall, « Début 14:00 » and the Fin it would give
/// beside it (`picked_fields` over the form's `starts` and `ends`); each
/// timed event of the day as a line (`day::hour_line`) before the first
/// half hour from its start.
pub(crate) fn slot_list(
    day: NaiveDate,
    blocks: &[(&OccurrenceResponse, day::Clip)],
    members: &[GroupMember],
    usage: Use,
    starts: &str,
    ends: &str,
) -> String {
    let mut lines: Vec<(i64, String)> = blocks
        .iter()
        .map(|(occ, clip)| (clip.from, day::hour_line(occ, members, usage)))
        .collect();
    lines.sort_by_key(|(from, _)| *from);
    let mut lines = lines.into_iter().peekable();
    let mut out = String::new();
    for (k, at) in day::half_hours(day).into_iter().enumerate() {
        let minute = k as i64 * day::HALF_HOUR;
        while let Some((_, line)) = lines.next_if(|(from, _)| *from <= minute) {
            out.push_str(&line);
        }
        let wall = at.with_timezone(&DISPLAY_TZ).naive_local();
        let (_, _, fin) = picked_fields(Prefill::At(wall), starts, ends);
        out.push_str(&format!(
            r#"<button type="submit" class="secondary sm" name="choix" value="{value}"><span>Début {time}</span> <span class="muted">Fin {end}</span></button>"#,
            value = wall.format("%Y-%m-%dT%H:%M"),
            time = wall.format("%H:%M"),
            end = fin.get(11..).unwrap_or_default(),
        ));
    }
    for (_, line) in lines {
        out.push_str(&line);
    }
    format!(r#"<div class="pick-list">{out}</div>"#)
}

/// The page's form (#795): it posts to the form it goes back to (`retour`),
/// carrying everything that form had sent in hidden fields (`carried`).
/// `chooser` is what goes inside.
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
    /// The family's colour (`data-accent`), its swatch's.
    accent: &'a str,
    month: String,
    day: NaiveDate,
    count: usize,
    load_error: bool,
    banner: String,
    /// The hours as the day view draws them (dialog), or the list of half
    /// hours (page).
    hours: String,
}

/// The inside of the choice (#795). In the dialog (`data-pick-body`): its
/// title and the family, swatch and name, beside a cross; then, in a body
/// that scrolls, the month, the day's name and how many events it holds,
/// its « Journée » banner, its hours, and a line `enhance.js` fills with
/// what a half hour would give; then a foot that says Échap and « Fermer ».
/// On the page: the title and the family, the month, the day, its banner,
/// its half hours, and the way back without a change.
fn chooser_html(c: &Chooser, shape: Shape) -> String {
    let error = if c.load_error {
        r#"<p class="notice error">Impossible de charger les événements pour le moment.</p>"#
    } else {
        ""
    };
    let family = format!(
        r#"<p class="pick-fam" data-accent="{}"><span class="swatch"></span>{}</p>"#,
        c.accent,
        html_escape(c.family)
    );
    let name = day_name(c.day);
    let back = r#"<button type="submit" class="secondary" name="choix" value="""#;
    match shape {
        Shape::Dialog => format!(
            r#"<div class="chooser" data-pick-body>
<div class="split pick-h"><hgroup><h2>Choisir dans le calendrier</h2>{family}</hgroup>{back} aria-label="Fermer">{close}</button></div>
<div class="pick-b">{error}{month}
<div class="split" data-pick-day><h3>{name}</h3><p class="muted">{count}</p></div>
{banner}{hours}
<p data-pick-status aria-live="polite">Survolez ou parcourez un créneau : il devient le Début, la Fin suit la durée saisie.</p>
</div>
<div class="split pick-f"><span class="muted">Échap : fermer sans rien changer</span>{back}>Fermer</button></div>
</div>"#,
            close = icon(Icon::Close),
            month = c.month,
            count = count_line(c.count),
            banner = c.banner,
            hours = c.hours,
        ),
        Shape::Page => format!(
            r#"<div class="chooser">
<hgroup><h1>Choisir dans le calendrier</h1>{family}</hgroup>
{error}{month}
<h2>{name}</h2>
{banner}<h2 class="eyebrow">Créneaux de 30 min</h2>{hours}
<div class="actions">{back}>Retour au formulaire sans changer</button></div>
</div>"#,
            month = c.month,
            banner = c.banner,
            hours = c.hours,
        ),
    }
}

/// What the page is asked: the form it goes back to, the day to show, the
/// form's Début as `enhance.js` sends it (a date or a wall clock, for the
/// outline of its day), and whether it is drawn for the `<dialog>`.
#[derive(Default)]
pub(crate) struct PickQuery {
    retour: String,
    jour: Option<String>,
    debut: Option<String>,
    /// Asked by `enhance.js`: the choice as its `<dialog>` shows it.
    dialogue: bool,
}

/// What the choice page's URL asks (`retour`, `jour`, `debut`, `dialogue`),
/// and the fields of the form it carries — every other pair, in order,
/// `famille` (read by `EventFamily`) and `choix` left out.
pub(crate) fn split_query(raw: Option<&str>) -> (PickQuery, Vec<(String, String)>) {
    let pairs: Vec<(String, String)> = raw
        .and_then(|q| serde_urlencoded::from_str(q).ok())
        .unwrap_or_default();
    let mut query = PickQuery::default();
    let mut carried = Vec::new();
    for (name, value) in pairs {
        match name.as_str() {
            "retour" => query.retour = value,
            "jour" => query.jour = Some(value),
            "debut" => query.debut = Some(value),
            "dialogue" => query.dialogue = true,
            "famille" | "choix" => {}
            _ => carried.push((name, value)),
        }
    }
    (query, carried)
}

/// A day of the choice page without JavaScript (#795): a link to the page
/// on that day (`action`, `jour`), carrying the form's fields (`carried`).
pub(crate) fn day_link(action: &str, carried: &[(String, String)], day: NaiveDate) -> String {
    let day = day.to_string();
    let pairs: Vec<(&str, &str)> = carried
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .chain([("jour", day.as_str())])
        .collect();
    format!(
        "{action}&{}",
        serde_urlencoded::to_string(pairs).unwrap_or_default()
    )
}

/// `GET /agenda/choisir`: the choice on the day its URL asks. With
/// `dialogue`, what `enhance.js` reads into the `<dialog>`, nothing
/// carried; without, a day or a month of the page without JavaScript, the
/// form's fields carried in the URL (`day_link`).
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    EventFamily(requested): EventFamily,
    RawQuery(raw): RawQuery,
) -> Response {
    let (query, carried) = split_query(raw.as_deref());
    let shape = if query.dialogue {
        Shape::Dialog
    } else {
        Shape::Page
    };
    choice_page(&state, &headers, &me, requested, &query, &carried, shape).await
}

/// `POST /agenda/choisir`: the form an event's « Choisir dans le
/// calendrier » sent without JavaScript — multipart from the sheet, a plain
/// form from the edit page. Its fields are carried on; a file is not, and
/// the form back says so (`fichier_perdu`).
pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    EventFamily(requested): EventFamily,
    RawQuery(raw): RawQuery,
    request: axum::extract::Request,
) -> Response {
    let (query, _) = split_query(raw.as_deref());
    let Some(kind) = create_body_kind(&headers) else {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    };
    let (mut pairs, had_file) = match read_fields(kind, &state, request).await {
        Ok(read) => read,
        Err(FieldsError::TooLarge) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        Err(FieldsError::Broken) => return StatusCode::BAD_REQUEST.into_response(),
    };
    pairs.retain(|(k, _)| k != "jour" && k != "choix");
    if had_file && !pairs.iter().any(|(k, _)| k == "fichier_perdu") {
        pairs.push(("fichier_perdu".to_string(), "1".to_string()));
    }
    choice_page(
        &state,
        &headers,
        &me,
        requested,
        &query,
        &pairs,
        Shape::Page,
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
async fn choice_page(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    requested: Option<Uuid>,
    query: &PickQuery,
    carried: &[(String, String)],
    shape: Shape,
) -> Response {
    let Some(target) = return_target(&query.retour) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some((fam, groups)) =
        family_context_choosing(state, headers, me, "/agenda/choisir", |_, _| requested).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let field = |name: &str| {
        carried
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let starts = field("starts_at")
        .or_else(|| query.debut.clone())
        .unwrap_or_default();
    let ends = field("ends_at").unwrap_or_default();
    let today = today_paris();
    let day = opening_day(query.jour.as_deref(), &starts, today);
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
    let accent = groups
        .iter()
        .find(|g| g.group_id == fam.gid)
        .map(accent_of)
        .unwrap_or_default();
    let hours = match shape {
        Shape::Dialog => day::hours_for(
            day,
            &blocks,
            &members,
            &[],
            Some(chrono::Utc::now()),
            occurrences.is_some(),
            usage,
        ),
        Shape::Page => slot_list(day, &blocks, &members, usage, &starts, &ends),
    };
    let chooser = chooser_html(
        &Chooser {
            family: &fam.name,
            accent: accent.as_str(),
            month: month_html(
                &MonthMarks {
                    chosen: day,
                    start: starts.get(..10).and_then(date_only),
                    today,
                    busy: &busy,
                    action: &action,
                    carried,
                },
                shape,
            ),
            day,
            count,
            load_error: occurrences.is_none(),
            banner: day::banner_for(day, &banner, &[], &members, &[], usage),
            hours,
        },
        shape,
    );
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
    fn a_half_hour_the_night_the_clocks_go_back_ends_later_by_the_clock() {
        // An hour after the first 02:00 or 02:30 of 25 October 2026 reads
        // 02:00 or 02:30 again, a Fin that is not after Début: an hour
        // later by the clock instead, as the day view's half hours do.
        let hour = |starts: &str| picked_fields(at(starts), "2026-11-12T09:00", "2026-11-12T10:00");
        assert_eq!(
            hour("2026-10-25T02:00"),
            (
                false,
                "2026-10-25T02:00".to_string(),
                "2026-10-25T03:00".to_string()
            )
        );
        assert_eq!(
            hour("2026-10-25T02:30"),
            (
                false,
                "2026-10-25T02:30".to_string(),
                "2026-10-25T03:30".to_string()
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
        assert!(
            picked.starts_with(r#"<p class="notice" role="status">"#),
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

    const ACTION: &str = "/agenda/choisir?retour=%2Fagenda%2Fnew";

    fn month(chosen: &str, start: Option<&str>, busy: &[&str], shape: Shape) -> String {
        let busy: BTreeSet<NaiveDate> = busy.iter().map(|d| day(d)).collect();
        let carried = pairs(&[("title", "Goûter")]);
        month_html(
            &MonthMarks {
                chosen: day(chosen),
                start: start.map(day),
                today: day("2026-11-14"),
                busy: &busy,
                action: ACTION,
                carried: &carried,
            },
            shape,
        )
    }

    #[test]
    fn the_month_is_a_link_per_day_from_monday_after_the_month_before() {
        let html = month("2026-11-12", Some("2026-11-12"), &[], Shape::Page);
        // November 2026 opens on a Sunday: the six days of October before
        // it, in --muted and hidden from a reader.
        assert!(
            html.contains(r#"<span class="dow" aria-hidden="true">D</span><span aria-hidden="true">26</span><span aria-hidden="true">27</span><span aria-hidden="true">28</span><span aria-hidden="true">29</span><span aria-hidden="true">30</span><span aria-hidden="true">31</span><a "#),
            "{html}"
        );
        assert_eq!(html.matches("<a href=").count(), 30, "{html}");
        assert!(!html.contains("<button"), "{html}");
        assert!(
            html.contains(r#"<a class="btn secondary" href="/agenda/choisir?retour=%2Fagenda%2Fnew&amp;title=Go%C3%BBter&amp;jour=2026-10-01" aria-label="Mois précédent">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"jour=2026-12-01" aria-label="Mois suivant""#),
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
        let html = month(
            "2026-11-20",
            Some("2026-11-12"),
            &["2026-11-03"],
            Shape::Page,
        );
        assert!(
            html.contains(r#"jour=2026-11-20" class="sel" aria-current="true">20</a>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"jour=2026-11-12" class="start">12<span class="sr-only">, jour du début</span></a>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"jour=2026-11-14" class="we today">14<span class="sr-only">, aujourd'hui</span></a>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"jour=2026-11-03" class="busy">3<span class="sr-only">, des événements</span></a>"#),
            "{html}"
        );
        assert_eq!(html.matches("aria-current").count(), 1, "{html}");
        // The start chosen is filled, not outlined.
        let same = month("2026-11-12", Some("2026-11-12"), &[], Shape::Page);
        assert!(!same.contains("start\""), "{same}");
    }

    #[test]
    fn the_dialogs_month_is_buttons_the_script_reads() {
        let html = month("2026-11-20", Some("2026-11-12"), &[], Shape::Dialog);
        assert!(!html.contains("<a "), "{html}");
        assert!(!html.contains("Goûter"), "{html}");
        assert_eq!(html.matches(r#"name="jour""#).count(), 32, "{html}");
        assert!(
            html.contains(r#"<button type="button" class="sel" name="jour" value="2026-11-20" aria-pressed="true">20</button>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<button type="button" class="we today" name="jour" value="2026-11-14" aria-current="date">14</button>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<button type="button" class="secondary" name="jour" value="2026-10-01" aria-label="Mois précédent">"#),
            "{html}"
        );
        assert!(html.contains("<h3>novembre 2026</h3>"), "{html}");
    }

    #[test]
    fn the_month_goes_to_the_months_around_it_across_a_year() {
        let html = month("2027-01-31", None, &[], Shape::Page);
        assert!(
            html.contains(r#"jour=2026-12-01" aria-label="Mois précédent""#),
            "{html}"
        );
        assert!(
            html.contains(r#"jour=2027-02-01" aria-label="Mois suivant""#),
            "{html}"
        );
        // January 2027 opens on a Friday: four days of December before it.
        assert_eq!(
            html.matches(r#"<span aria-hidden="true">"#).count(),
            4,
            "{html}"
        );
        assert_eq!(html.matches("<a href=").count(), 31, "{html}");
    }

    #[test]
    fn a_month_opening_on_a_monday_writes_no_day_before_it() {
        // February 2027 opens on a Monday.
        let html = month("2027-02-10", None, &[], Shape::Page);
        assert!(!html.contains(r#"<span aria-hidden="true">"#), "{html}");
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
    fn without_javascript_each_half_hour_is_a_button_saying_its_fin() {
        let html = slot_list(
            day("2026-11-12"),
            &[],
            &[],
            Use::Pick(None),
            "2026-11-12T09:00",
            "2026-11-12T10:30",
        );
        assert!(html.starts_with(r#"<div class="pick-list">"#), "{html}");
        assert_eq!(html.matches("<button ").count(), 48, "{html}");
        assert!(
            html.contains(r#"<button type="submit" class="secondary sm" name="choix" value="2026-11-12T14:00"><span>Début 14:00</span> <span class="muted">Fin 15:30</span></button>"#),
            "{html}"
        );
        // Across midnight, the Fin of the day after.
        assert!(
            html.contains(r#"value="2026-11-12T23:30"><span>Début 23:30</span> <span class="muted">Fin 01:00</span>"#),
            "{html}"
        );
        // A day the clocks go forward has no 02:00.
        let short = slot_list(day("2026-03-29"), &[], &[], Use::Pick(None), "", "");
        assert_eq!(short.matches("<button ").count(), 46, "{short}");
        // An hour after 01:30 that night is 03:30 by the clock.
        assert!(
            short.contains(r#"<span>Début 01:30</span> <span class="muted">Fin 03:30</span>"#),
            "{short}"
        );
    }

    /// A timed event of one member, between two Paris wall clocks.
    fn timed(title: &str, starts: &str, ends: &str) -> OccurrenceResponse {
        let utc = |at: &str| paris_local_to_utc(at).unwrap();
        OccurrenceResponse {
            event: manage_our_home_shared::dto::agenda::EventResponse {
                id: Uuid::from_u128(1),
                group_id: Uuid::nil(),
                created_by: Uuid::from_u128(7),
                title: title.to_string(),
                description: None,
                location: None,
                starts_at: utc(starts),
                ends_at: utc(ends),
                all_day: false,
                is_task: false,
                completed_at: None,
                rrule: None,
                assignee_ids: vec![Uuid::from_u128(7)],
                is_private: false,
            },
            occurrence_starts_at: utc(starts),
            occurrence_ends_at: utc(ends),
        }
    }

    #[test]
    fn without_javascript_the_events_are_lines_before_their_half_hour() {
        let pharmacy = timed("Pharmacie", "2026-11-12T12:15", "2026-11-12T12:45");
        let late = timed("Veille", "2026-11-11T23:00", "2026-11-12T01:00");
        let d = day("2026-11-12");
        let clip = |o: &OccurrenceResponse| {
            day::clip(o.occurrence_starts_at, o.occurrence_ends_at, d).unwrap()
        };
        let html = slot_list(
            d,
            &[(&pharmacy, clip(&pharmacy)), (&late, clip(&late))],
            &[],
            Use::Pick(None),
            "",
            "",
        );
        let at = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("{needle}: {html}"))
        };
        // Begun the day before: first of all.
        assert!(at("Veille") < at("Début 00:00"), "{html}");
        assert!(at("Début 12:00") < at("Pharmacie"), "{html}");
        assert!(at("Pharmacie") < at("Début 12:30"), "{html}");
    }

    fn chooser(shape: Shape) -> String {
        chooser_html(
            &Chooser {
                family: "Famille <Durand>",
                accent: "ocean",
                month: "<month/>".to_string(),
                day: day("2026-11-12"),
                count: 2,
                load_error: false,
                banner: "<banner/>".to_string(),
                hours: "<hours/>".to_string(),
            },
            shape,
        )
    }

    #[test]
    fn the_dialog_names_the_family_beside_its_title_and_keeps_a_foot() {
        let html = chooser(Shape::Dialog);
        assert!(
            html.starts_with(r#"<div class="chooser" data-pick-body>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<hgroup><h2>Choisir dans le calendrier</h2><p class="pick-fam" data-accent="ocean"><span class="swatch"></span>Famille &lt;Durand&gt;</p></hgroup>"#),
            "{html}"
        );
        assert!(!html.contains("eyebrow"), "{html}");
        assert!(
            html.contains(r#"<h3>jeudi 12 novembre</h3><p class="muted">2 au programme</p>"#),
            "{html}"
        );
        // The line under the hours says something before any half hour.
        assert!(
            html.contains(r#"<p data-pick-status aria-live="polite">Survolez ou parcourez un créneau : il devient le Début, la Fin suit la durée saisie.</p>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<div class="split pick-f"><span class="muted">Échap : fermer sans rien changer</span><button type="submit" class="secondary" name="choix" value="">Fermer</button></div>"#),
            "{html}"
        );
        let body = html.find(r#"<div class="pick-b">"#).unwrap();
        assert!(html.find("<month/>").unwrap() > body, "{html}");
        assert!(
            html.find("<hours/>").unwrap() < html.find(r#"class="split pick-f""#).unwrap(),
            "{html}"
        );
    }

    #[test]
    fn the_page_lists_its_half_hours_and_goes_back_without_a_change() {
        let html = chooser(Shape::Page);
        assert!(!html.contains("data-pick"), "{html}");
        assert!(
            html.contains("<hgroup><h1>Choisir dans le calendrier</h1><p class=\"pick-fam\""),
            "{html}"
        );
        assert!(
            html.contains("<h2>jeudi 12 novembre</h2>\n<banner/><h2 class=\"eyebrow\">Créneaux de 30 min</h2><hours/>"),
            "{html}"
        );
        assert!(
            html.contains(r#"<button type="submit" class="secondary" name="choix" value="">Retour au formulaire sans changer</button>"#),
            "{html}"
        );
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

    // -- split_query, day_link ------------------------------------------------

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_url_says_what_the_page_reads_and_carries_the_rest() {
        let (query, carried) = split_query(Some(
            "retour=%2Fagenda%2Fnew&famille=x&title=D%C3%AEner&jour=2026-11-20\
             &assignee_ids=a&choix=2026-11-20&assignee_ids=b&debut=2026-11-12T09%3A00\
             &dialogue=1",
        ));
        assert_eq!(query.retour, "/agenda/new");
        assert_eq!(query.jour.as_deref(), Some("2026-11-20"));
        assert_eq!(query.debut.as_deref(), Some("2026-11-12T09:00"));
        assert!(query.dialogue);
        assert_eq!(
            carried,
            pairs(&[
                ("title", "Dîner"),
                ("assignee_ids", "a"),
                ("assignee_ids", "b")
            ])
        );
        let (query, carried) = split_query(Some("retour=%2Fagenda%2Fnew"));
        assert!(!query.dialogue);
        assert!(query.jour.is_none());
        assert!(carried.is_empty());
        let (query, carried) = split_query(None);
        assert_eq!(query.retour, "");
        assert!(carried.is_empty());
    }

    #[test]
    fn a_day_is_a_link_carrying_what_was_typed() {
        let carried = pairs(&[
            ("title", "Dîner & co"),
            ("assignee_ids", "a"),
            ("assignee_ids", "b"),
            ("fichier_perdu", "1"),
        ]);
        let link = day_link(
            "/agenda/choisir?retour=%2Fagenda%2Fnew",
            &carried,
            day("2026-11-20"),
        );
        assert_eq!(
            link,
            "/agenda/choisir?retour=%2Fagenda%2Fnew&title=D%C3%AEner+%26+co\
             &assignee_ids=a&assignee_ids=b&fichier_perdu=1&jour=2026-11-20"
        );
        // Read back, the page finds the same fields and the day.
        let (query, back) = split_query(link.split_once('?').map(|(_, q)| q));
        assert_eq!(back, carried);
        assert_eq!(query.jour.as_deref(), Some("2026-11-20"));
        assert_eq!(query.retour, "/agenda/new");
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
