//! `/agenda/new` — create an event or a task (`is_task`). Houses the RRULE
//! v1 picker (also reused by `edit.rs`) and the optional at-creation
//! reminder. Error table: title empty / `ends_at < starts_at` are
//! pre-validated (`validate_event_form`) with an inline error and no API
//! round-trip; the backend's matching 400s (`ends_at_before_starts_at`,
//! `invalid_rrule`) are mapped defensively. 403 (non-member) → forbidden
//! page. Success (201) → PRG `/agenda?notice=event_created`.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::NaiveDate;
use leptos::prelude::*;
use manage_our_home_shared::dto::agenda::{
    CreateEventRequest, CreateReminderRequest, EventResponse,
};
use manage_our_home_shared::dto::groups::GroupMember;
use manage_our_home_shared::validation::agenda::{
    build_rrule, validate_event_form, EventFormError, Freq, Recurrence, RecurrenceEnd,
};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::routes::groups::members::fetch_group_detail;
use crate::state::{api_request_auth, AppState};

use super::{agenda_cookie, family_context, forbidden_page, paris_local_to_utc, to_datetime_local};

/// Reminder offset presets exposed by the "Rappel" select — key → minutes
/// before the occurrence. Shared with the detail page's add-reminder form.
pub(crate) const REMINDER_OPTIONS: [(&str, &str, i32); 5] = [
    ("0", "À l'heure de l'événement", 0),
    ("10", "10 minutes avant", 10),
    ("60", "1 heure avant", 60),
    ("1440", "1 jour avant", 1440),
    ("10080", "1 semaine avant", 10080),
];

/// Whether a request body may be read as an HTML form submission.
///
/// `/agenda/new` and `/agenda/:id/edit` read their body as raw `Bytes`
/// rather than through `axum::Form`, because `Form`'s deserializer cannot
/// express the repeated `assignee_ids` key a checkbox group submits
/// (`assignee_ids_from_raw_form` below). Taking `Bytes` also dropped the
/// media-type check `Form` performs, so a `text/plain` POST created an
/// event where every one of the 31 other handlers in this folder answers
/// 415 (#98 verification, round 2). This restores that check explicitly.
///
/// Matches on the media type alone: parameters (`; charset=UTF-8`, which
/// some clients append) and case are not part of the decision, per RFC 9110
/// §8.3.
pub(crate) fn is_form_urlencoded(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("application/x-www-form-urlencoded")
        })
        .unwrap_or(false)
}

/// Hidden field the assignee fieldset carries when it actually listed the
/// family's members. Its absence from a submitted body means the picker was
/// rendered blind — `fetch_group_detail` had failed and the fieldset came
/// out empty — and the receiving handler must then leave the assignment
/// alone instead of reading "no box checked" as "assign to nobody"
/// (#98 verification, round 2: a decorative degradation was driving a
/// destructive write).
pub(crate) const ASSIGNEES_PRESENT_FIELD: &str = "assignees_present";

/// Every `assignee_ids=<uuid>` pair in a raw `application/x-www-form-urlencoded`
/// body. `axum::Form`'s deserializer (`serde_urlencoded`) has no support for
/// several values under one key — exactly what a set of same-named
/// checkboxes (or a `<select multiple>`) submits — so the assignee
/// checkboxes are read straight from the raw body instead of through
/// `EventForm`. No percent-decoding pass is needed: every character in a
/// canonical UUID (hex digits and hyphens) is unreserved in this encoding.
pub(crate) fn assignee_ids_from_raw_form(raw: &str) -> Vec<Uuid> {
    raw.split('&')
        .filter_map(|pair| pair.split_once('='))
        .filter(|(k, _)| *k == "assignee_ids")
        .filter_map(|(_, v)| Uuid::parse_str(v).ok())
        .collect()
}

/// Renders one checkbox per family member — the "who is this for" picker
/// (issue #73). Same `field inline` markup as the recurrence picker's day
/// checkboxes just below, one input per option rather than a dynamic
/// multi-select widget, so it needs no JS and degrades to plain checkboxes
/// with JS disabled like every other form on this app.
pub(crate) fn assignee_checkboxes(members: &[GroupMember], selected: &[Uuid]) -> String {
    if members.is_empty() {
        // The roster couldn't be loaded (a `fetch_group_detail` failure —
        // a family always has at least its creator). Say so, and emit no
        // `ASSIGNEES_PRESENT_FIELD`: the submitted form then carries no
        // opinion about assignment at all.
        return r#"<fieldset class="card">
<legend>Assigné à</legend>
<p class="muted">La liste des membres n'a pas pu être chargée&nbsp;; l'assignation reste inchangée.</p>
</fieldset>"#
            .to_string();
    }
    let boxes: String = members
        .iter()
        .map(|m| {
            let checked = if selected.contains(&m.user_id) {
                " checked"
            } else {
                ""
            };
            format!(
                r#"<label class="field inline"><input type="checkbox" name="assignee_ids" value="{id}"{checked}/>{name}</label>"#,
                id = m.user_id,
                name = html_escape(&m.display_name),
            )
        })
        .collect();
    format!(
        r#"<fieldset class="card">
<legend>Assigné à</legend>
<input type="hidden" name="{marker}" value="1"/>
{boxes}
</fieldset>"#,
        marker = ASSIGNEES_PRESENT_FIELD,
    )
}

/// Renders the reminder `<select>` (with a leading "no reminder" option).
pub(crate) fn reminder_select(name: &str, selected: Option<&str>) -> String {
    let mut opts = format!(
        r#"<option value=""{sel}>Aucun rappel</option>"#,
        sel = if selected.is_none() { " selected" } else { "" }
    );
    for (key, label, _) in REMINDER_OPTIONS {
        let sel = if selected == Some(key) {
            " selected"
        } else {
            ""
        };
        opts.push_str(&format!(r#"<option value="{key}"{sel}>{label}</option>"#));
    }
    format!(r#"<select name="{name}">{opts}</select>"#)
}

/// The shared event form fields, used by both create and edit.
#[derive(serde::Deserialize, Default)]
pub struct EventForm {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub location: String,
    pub starts_at: String,
    pub ends_at: String,
    #[serde(default)]
    pub all_day: Option<String>,
    #[serde(default)]
    pub is_task: Option<String>,
    // RRULE picker
    #[serde(default)]
    pub freq: String,
    #[serde(default)]
    pub interval: String,
    #[serde(default)]
    pub byday_mo: Option<String>,
    #[serde(default)]
    pub byday_tu: Option<String>,
    #[serde(default)]
    pub byday_we: Option<String>,
    #[serde(default)]
    pub byday_th: Option<String>,
    #[serde(default)]
    pub byday_fr: Option<String>,
    #[serde(default)]
    pub byday_sa: Option<String>,
    #[serde(default)]
    pub byday_su: Option<String>,
    #[serde(default)]
    pub end_kind: String,
    #[serde(default)]
    pub count: String,
    #[serde(default)]
    pub until: String,
    // Create-only: optional reminder at creation.
    #[serde(default)]
    pub reminder: String,
    /// Present iff the assignee fieldset listed real members — see
    /// `ASSIGNEES_PRESENT_FIELD`.
    #[serde(default)]
    pub assignees_present: Option<String>,
}

/// Builds the RRULE string from the picker fields, or `Ok(None)` for a
/// one-off event. `Err(())` on an out-of-range value the UI shouldn't emit.
pub(crate) fn rrule_from_form(form: &EventForm) -> Result<Option<String>, ()> {
    use chrono::Weekday;
    let freq = match form.freq.as_str() {
        "" | "none" => return Ok(None),
        "daily" => Freq::Daily,
        "weekly" => Freq::Weekly,
        "monthly" => Freq::Monthly,
        "yearly" => Freq::Yearly,
        _ => return Err(()),
    };
    let interval = form
        .interval
        .trim()
        .parse::<u32>()
        .unwrap_or(1)
        .clamp(1, 99);
    let mut byday = Vec::new();
    if freq == Freq::Weekly {
        for (flag, wd) in [
            (&form.byday_mo, Weekday::Mon),
            (&form.byday_tu, Weekday::Tue),
            (&form.byday_we, Weekday::Wed),
            (&form.byday_th, Weekday::Thu),
            (&form.byday_fr, Weekday::Fri),
            (&form.byday_sa, Weekday::Sat),
            (&form.byday_su, Weekday::Sun),
        ] {
            if flag.is_some() {
                byday.push(wd);
            }
        }
    }
    let end = match form.end_kind.as_str() {
        "count" => {
            let n = form.count.trim().parse::<u32>().map_err(|_| ())?;
            if n == 0 || n > 730 {
                return Err(());
            }
            RecurrenceEnd::Count(n)
        }
        "until" => {
            let d = NaiveDate::parse_from_str(form.until.trim(), "%Y-%m-%d").map_err(|_| ())?;
            RecurrenceEnd::Until(d)
        }
        _ => RecurrenceEnd::Never,
    };
    Ok(Some(build_rrule(&Recurrence {
        freq,
        interval,
        byday,
        end,
    })))
}

/// Renders the recurrence picker, pre-selecting `current` (the parsed
/// existing rule on the edit page, `None` for a fresh create). Progressive
/// enhancement: with JS the day/end sub-controls could hide until relevant;
/// without it they're always visible and simply ignored server-side when
/// the frequency doesn't use them.
pub(crate) fn recurrence_picker(current: Option<&Recurrence>) -> String {
    use chrono::Weekday;
    let cur_freq = current.map(|r| r.freq);
    let freq_opt = |value: &str, label: &str, f: Option<Freq>| {
        let sel = if cur_freq == f { " selected" } else { "" };
        format!(r#"<option value="{value}"{sel}>{label}</option>"#)
    };
    let freq_select = format!(
        r#"<select name="freq">{none}{d}{w}{m}{y}</select>"#,
        none = freq_opt("none", "Aucune (ponctuel)", None),
        d = freq_opt("daily", "Quotidienne", Some(Freq::Daily)),
        w = freq_opt("weekly", "Hebdomadaire", Some(Freq::Weekly)),
        m = freq_opt("monthly", "Mensuelle", Some(Freq::Monthly)),
        y = freq_opt("yearly", "Annuelle", Some(Freq::Yearly)),
    );

    let interval = current.map(|r| r.interval).unwrap_or(1);
    let byday_checked = |wd: Weekday| -> &'static str {
        match current {
            Some(r) if r.byday.contains(&wd) => " checked",
            _ => "",
        }
    };
    let day_checkbox = |name: &str, label: &str, wd: Weekday| {
        format!(
            r#"<label class="field inline"><input type="checkbox" name="{name}"{c}/>{label}</label>"#,
            c = byday_checked(wd),
        )
    };
    let days = format!(
        r#"<div class="actions">{mo}{tu}{we}{th}{fr}{sa}{su}</div>"#,
        mo = day_checkbox("byday_mo", "Lun", Weekday::Mon),
        tu = day_checkbox("byday_tu", "Mar", Weekday::Tue),
        we = day_checkbox("byday_we", "Mer", Weekday::Wed),
        th = day_checkbox("byday_th", "Jeu", Weekday::Thu),
        fr = day_checkbox("byday_fr", "Ven", Weekday::Fri),
        sa = day_checkbox("byday_sa", "Sam", Weekday::Sat),
        su = day_checkbox("byday_su", "Dim", Weekday::Sun),
    );

    let (end_never, end_count, end_until, count_val, until_val) = match current.map(|r| &r.end) {
        Some(RecurrenceEnd::Count(n)) => (" ", " selected", " ", n.to_string(), String::new()),
        Some(RecurrenceEnd::Until(d)) => (
            " ",
            " ",
            " selected",
            String::new(),
            d.format("%Y-%m-%d").to_string(),
        ),
        _ => (" selected", " ", " ", String::new(), String::new()),
    };

    format!(
        r#"<fieldset class="card">
<legend>Récurrence</legend>
<label>Fréquence {freq_select}</label>
<label>Intervalle
<input type="number" name="interval" min="1" max="99" value="{interval}"/>
<span class="muted">Ex. 2 = une occurrence sur deux. Ignoré si « Aucune ».</span>
</label>
<div class="muted">Jours (hebdomadaire seulement)</div>
{days}
<label>Arrêt de la récurrence
<select name="end_kind">
<option value="never"{end_never}>Jamais</option>
<option value="count"{end_count}>Après un nombre d'occurrences</option>
<option value="until"{end_until}>Jusqu'à une date</option>
</select>
</label>
<label>Nombre d'occurrences <input type="number" name="count" min="1" max="730" value="{count_val}"/></label>
<label>Jusqu'au <input type="date" name="until" value="{until_val}"/></label>
</fieldset>"#,
    )
}

pub(crate) fn error_message(code: &str) -> &'static str {
    match code {
        "title_required" => "Le titre est obligatoire.",
        "ends_before_starts" => "La fin doit être après le début.",
        "invalid_rrule" => "La récurrence choisie est invalide.",
        "invalid_form" => "Formulaire incomplet ou illisible, merci de réessayer.",
        "unavailable" => "Service momentanément indisponible, merci de réessayer.",
        _ => "Une erreur est survenue, merci de réessayer.",
    }
}

#[allow(clippy::too_many_arguments)]
fn page(
    header: &str,
    error: Option<&str>,
    default_start: &str,
    default_end: &str,
    members: &[GroupMember],
    selected_assignees: &[Uuid],
) -> String {
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let picker = recurrence_picker(None);
    let reminder = reminder_select("reminder", None);
    let assignees = assignee_checkboxes(members, selected_assignees);
    // `header` is trusted HTML built by `app_header`; embed it directly at
    // the top, the same position the `view!`-based pages give it.
    let body = format!(
        r#"<h1>Nouvel événement</h1>
{error_html}
<form method="post" action="/agenda/new">
<label>Titre <input type="text" name="title" required/></label>
<label class="field inline">
<input type="checkbox" name="is_task"/> Il s'agit d'une tâche (à cocher une fois faite)</label>
<label class="field inline">
<input type="checkbox" name="all_day"/> Journée entière</label>
<label>Début <input type="datetime-local" name="starts_at" value="{default_start}" required/></label>
<label>Fin <input type="datetime-local" name="ends_at" value="{default_end}" required/></label>
<label>Lieu <input type="text" name="location"/></label>
<label>Description <textarea name="description" rows="3"></textarea></label>
{picker}
{assignees}
<p class="muted">Aucune sélection = assigné à vous.</p>
<label>Rappel {reminder}</label>
<button type="submit">Créer l'événement</button>
</form>
<div class="links"><a href="/agenda">Retour à l'agenda</a></div>"#,
    );
    shell_with_header(Width::Form, "Nouvel événement", header, &body)
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/agenda/new").await else {
        return Redirect::to("/groups/new").into_response();
    };
    // Default to the next round hour, one-hour duration, in Paris.
    let now = chrono::Utc::now();
    let start = to_datetime_local(now);
    let end = to_datetime_local(now + chrono::Duration::hours(1));
    let cookie = agenda_cookie(&headers);
    let members = fetch_group_detail(&state, cookie.as_deref(), fam.gid)
        .await
        .ok()
        .flatten()
        .map(|g| g.members)
        .unwrap_or_default();
    // Nobody checked yet, so the picker shows nothing selected; the actual
    // default-to-creator happens server-side (`resolve_assignees`) when the
    // form is submitted with an empty selection.
    Html(page(&fam.header, None, &start, &end, &members, &[])).into_response()
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    raw_body: axum::body::Bytes,
) -> Response {
    if !is_form_urlencoded(&headers) {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let Some(fam) = family_context(&state, &headers, &me, "/agenda/new").await else {
        return Redirect::to("/groups/new").into_response();
    };

    let raw = String::from_utf8_lossy(&raw_body);
    let assignee_ids = assignee_ids_from_raw_form(&raw);
    let cookie = agenda_cookie(&headers);
    let members = fetch_group_detail(&state, cookie.as_deref(), fam.gid)
        .await
        .ok()
        .flatten()
        .map(|g| g.members)
        .unwrap_or_default();

    // A body that doesn't deserialize is reported as such. Swallowing the
    // error with `unwrap_or_default()` handed the caller a 200 carrying
    // "La fin doit être après le début." for a body that simply had no
    // `starts_at` at all (#98 verification, round 2).
    let Ok(form) = serde_urlencoded::from_bytes::<EventForm>(&raw_body) else {
        let now = chrono::Utc::now();
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Html(page(
                &fam.header,
                Some(error_message("invalid_form")),
                &to_datetime_local(now),
                &to_datetime_local(now + chrono::Duration::hours(1)),
                &members,
                &assignee_ids,
            )),
        )
            .into_response();
    };

    let render_error = |code: &str| {
        Html(page(
            &fam.header,
            Some(error_message(code)),
            &form.starts_at,
            &form.ends_at,
            &members,
            &assignee_ids,
        ))
        .into_response()
    };

    let (Some(starts_at), Some(ends_at)) = (
        paris_local_to_utc(&form.starts_at),
        paris_local_to_utc(&form.ends_at),
    ) else {
        return render_error("ends_before_starts");
    };

    match validate_event_form(&form.title, starts_at, ends_at) {
        Err(EventFormError::TitleRequired) => return render_error("title_required"),
        Err(EventFormError::EndsBeforeStarts) => return render_error("ends_before_starts"),
        Ok(()) => {}
    }

    let Ok(rrule) = rrule_from_form(&form) else {
        return render_error("invalid_rrule");
    };

    let description =
        (!form.description.trim().is_empty()).then(|| form.description.trim().to_string());
    let location = (!form.location.trim().is_empty()).then(|| form.location.trim().to_string());
    let req = CreateEventRequest {
        title: form.title.trim().to_string(),
        description,
        location,
        starts_at,
        ends_at,
        all_day: form.all_day.is_some(),
        is_task: form.is_task.is_some(),
        rrule,
        assignee_ids: (!assignee_ids.is_empty()).then(|| assignee_ids.clone()),
    };

    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/events", fam.gid),
        cookie.as_deref(),
        Some(serde_json::to_value(&req).unwrap()),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            // Fire the optional reminder against the freshly created event.
            if let Ok(event) = serde_json::from_value::<EventResponse>(resp.body) {
                if let Some((_, _, minutes)) = REMINDER_OPTIONS
                    .iter()
                    .find(|(k, _, _)| *k == form.reminder)
                {
                    let _ = api_request_auth(
                        &state,
                        reqwest::Method::POST,
                        &format!("/groups/{}/events/{}/reminders", fam.gid, event.id),
                        cookie.as_deref(),
                        Some(
                            serde_json::to_value(CreateReminderRequest {
                                offset_minutes: *minutes,
                            })
                            .unwrap(),
                        ),
                    )
                    .await;
                }
            }
            Redirect::to("/agenda?notice=event_created").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            render_error("ends_before_starts")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            forbidden_page().into_response()
        }
        Ok(_) | Err(_) => render_error("unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::CONTENT_TYPE;

    fn headers_with(content_type: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(CONTENT_TYPE, content_type.parse().unwrap());
        h
    }

    // -- is_form_urlencoded (#98 round-2, Mi1) ---------------------------

    #[test]
    fn a_plain_form_post_is_accepted() {
        assert!(is_form_urlencoded(&headers_with(
            "application/x-www-form-urlencoded"
        )));
    }

    /// Browsers append a charset, and the media type is case-insensitive:
    /// neither may turn a real form submission into a 415.
    #[test]
    fn parameters_and_casing_do_not_change_the_media_type() {
        assert!(is_form_urlencoded(&headers_with(
            "application/x-www-form-urlencoded; charset=UTF-8"
        )));
        assert!(is_form_urlencoded(&headers_with(
            "Application/X-WWW-Form-Urlencoded"
        )));
        assert!(is_form_urlencoded(&headers_with(
            " application/x-www-form-urlencoded "
        )));
    }

    /// The regression itself: `axum::Form` answered 415 to these, taking
    /// the body as raw `Bytes` answered 200 and created an event.
    #[test]
    fn any_other_media_type_is_rejected() {
        for ct in [
            "text/plain",
            "application/json",
            "multipart/form-data; boundary=x",
            "",
            "application/x-www-form-urlencoded-not-really",
        ] {
            assert!(!is_form_urlencoded(&headers_with(ct)), "{ct}");
        }
    }

    #[test]
    fn a_body_with_no_content_type_at_all_is_rejected() {
        assert!(!is_form_urlencoded(&HeaderMap::new()));
    }

    // -- assignee_checkboxes / ASSIGNEES_PRESENT_FIELD (Mo4) -------------

    fn member(n: u128, name: &str) -> GroupMember {
        GroupMember {
            user_id: Uuid::from_u128(n),
            display_name: name.to_string(),
            email: format!("{name}@example.test"),
            role: "member".to_string(),
        }
    }

    /// The marker rides along whenever the picker really listed members, so
    /// the receiving handler can tell "nobody checked" from "nothing shown".
    #[test]
    fn a_populated_picker_carries_the_presence_marker() {
        let html = assignee_checkboxes(&[member(1, "Alice"), member(2, "Bob")], &[]);
        assert!(html.contains(ASSIGNEES_PRESENT_FIELD), "{html}");
        assert_eq!(html.matches(r#"name="assignee_ids""#).count(), 2);
    }

    /// A roster that failed to load renders no marker and no checkbox — and
    /// says so, rather than looking like a family with no members.
    #[test]
    fn a_picker_with_no_roster_carries_no_marker() {
        let html = assignee_checkboxes(&[], &[]);
        assert!(!html.contains(ASSIGNEES_PRESENT_FIELD), "{html}");
        assert!(!html.contains(r#"name="assignee_ids""#), "{html}");
        assert!(html.contains("n'a pas pu être chargée"), "{html}");
    }

    #[test]
    fn the_selected_members_come_back_checked() {
        let members = vec![member(1, "Alice"), member(2, "Bob")];
        let html = assignee_checkboxes(&members, &[members[1].user_id]);
        assert!(html.contains(&format!(r#"value="{}" checked"#, members[1].user_id)));
        assert!(html.contains(&format!(r#"value="{}"/>"#, members[0].user_id)));
    }
}
