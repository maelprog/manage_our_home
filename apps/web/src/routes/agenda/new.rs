//! `/agenda/new` — create an event or a task (`is_task`). Houses the RRULE
//! v1 picker (also reused by `edit.rs`) and the optional at-creation
//! reminder. Error table: title empty / `ends_at < starts_at` are
//! pre-validated (`validate_event_form`) with an inline error and no API
//! round-trip; the backend's matching 400s (`ends_at_before_starts_at`,
//! `invalid_rrule`) are mapped defensively. 403 (non-member) → forbidden
//! page. Success (201) → PRG `/agenda?notice=event_created`.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use chrono::NaiveDate;
use leptos::prelude::*;
use manage_our_home_shared::dto::agenda::{
    CreateEventRequest, CreateReminderRequest, EventResponse,
};
use manage_our_home_shared::validation::agenda::{
    build_rrule, validate_event_form, EventFormError, Freq, Recurrence, RecurrenceEnd,
};

use crate::app::{html_escape, shell};
use crate::layout::CurrentUser;
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
            r#"<label style="flex-direction:row;gap:0.3rem;align-items:center;margin:0;font-weight:normal;"><input type="checkbox" name="{name}"{c}/>{label}</label>"#,
            c = byday_checked(wd),
        )
    };
    let days = format!(
        r#"<div style="display:flex;gap:0.6rem;flex-wrap:wrap;margin:0.4rem 0;">{mo}{tu}{we}{th}{fr}{sa}{su}</div>"#,
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
        r#"<fieldset style="border:1px solid var(--border);padding:0.75rem;margin-top:1rem;">
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
        "unavailable" => "Service momentanément indisponible, merci de réessayer.",
        _ => "Une erreur est survenue, merci de réessayer.",
    }
}

fn page(header: &str, error: Option<&str>, default_start: &str, default_end: &str) -> String {
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let picker = recurrence_picker(None);
    let reminder = reminder_select("reminder", None);
    // `header` is trusted HTML built by `app_header`; embed it directly at
    // the top, the same position the `view!`-based pages give it.
    let body = format!(
        r#"{header}
<h1>Nouvel événement</h1>
{error_html}
<form method="post" action="/agenda/new">
<label>Titre <input type="text" name="title" required/></label>
<label style="flex-direction:row;gap:0.4rem;align-items:center;font-weight:normal;">
<input type="checkbox" name="is_task"/> Il s'agit d'une tâche (à cocher une fois faite)</label>
<label style="flex-direction:row;gap:0.4rem;align-items:center;font-weight:normal;">
<input type="checkbox" name="all_day"/> Journée entière</label>
<label>Début <input type="datetime-local" name="starts_at" value="{default_start}" required/></label>
<label>Fin <input type="datetime-local" name="ends_at" value="{default_end}" required/></label>
<label>Lieu <input type="text" name="location"/></label>
<label>Description <textarea name="description" rows="3"></textarea></label>
{picker}
<label style="margin-top:1rem;">Rappel {reminder}</label>
<button type="submit">Créer l'événement</button>
</form>
<div class="links"><a href="/agenda">Retour à l'agenda</a></div>"#,
    );
    shell("Nouvel événement", &body)
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
    Html(page(&fam.header, None, &start, &end)).into_response()
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<EventForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/agenda/new").await else {
        return Redirect::to("/groups/new").into_response();
    };

    let render_error = |code: &str| {
        Html(page(
            &fam.header,
            Some(error_message(code)),
            &form.starts_at,
            &form.ends_at,
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
    };

    let cookie = agenda_cookie(&headers);
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
