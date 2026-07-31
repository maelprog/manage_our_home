//! `/agenda/:id/edit` — edit an event/task's fields and recurrence. Same
//! permission bar as delete (`can_modify`): a non-editor never sees the
//! form, and the backend 403 is mapped to `?error=forbidden` on the detail
//! page defensively. `is_task` is shown read-only — the backend's
//! `UpdateEventRequest` has no `is_task` field, so it can't change after
//! creation. Choosing "Aucune" recurrence clears an existing rule (sent as
//! the empty string, which the backend maps to `NULL`).

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::agenda::{EventResponse, UpdateEventRequest};
use manage_our_home_shared::validation::agenda::{
    parse_rrule, validate_event_form, EventFormError, Recurrence,
};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::new::{error_message, recurrence_picker, rrule_from_form, EventForm};
use super::{
    agenda_cookie, can_modify, event_not_found_page, family_context, forbidden_page,
    paris_local_to_utc, service_unavailable_page, to_datetime_local,
};

#[allow(clippy::too_many_arguments)]
fn page(
    header: &str,
    id: Uuid,
    is_task: bool,
    title: &str,
    description: &str,
    location: &str,
    starts_local: &str,
    ends_local: &str,
    all_day: bool,
    recurrence: Option<&Recurrence>,
    error: Option<&str>,
) -> String {
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let picker = recurrence_picker(recurrence);
    let all_day_checked = if all_day { " checked" } else { "" };
    let kind = if is_task { "Tâche" } else { "Événement" };
    let body = format!(
        r#"<h1>Modifier — {title_esc}</h1>
<p class="muted">{kind} (le type ne peut pas être changé après création)</p>
{error_html}
<form method="post" action="/agenda/{id}/edit">
<label>Titre <input type="text" name="title" required value="{title_attr}"/></label>
<label class="field inline">
<input type="checkbox" name="all_day"{all_day_checked}/> Journée entière</label>
<label>Début <input type="datetime-local" name="starts_at" value="{starts_local}" required/></label>
<label>Fin <input type="datetime-local" name="ends_at" value="{ends_local}" required/></label>
<label>Lieu <input type="text" name="location" value="{location_attr}"/></label>
<label>Description <textarea name="description" rows="3">{description_esc}</textarea></label>
{picker}
<button type="submit">Enregistrer</button>
</form>
<div class="links"><a href="/agenda/{id}">Retour au détail</a></div>"#,
        title_esc = html_escape(title),
        title_attr = html_escape(title),
        location_attr = html_escape(location),
        description_esc = html_escape(description),
    );
    shell_with_header(Width::Form, "Modifier l'événement", header, &body)
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(event_id): Path<Uuid>,
) -> Response {
    let Some(fam) =
        family_context(&state, &headers, &me, &format!("/agenda/{event_id}/edit")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = agenda_cookie(&headers);
    let event = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/events/{}", fam.gid, event_id),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match serde_json::from_value::<EventResponse>(resp.body) {
                Ok(e) => e,
                Err(_) => return service_unavailable_page().into_response(),
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return event_not_found_page().into_response()
        }
        _ => return service_unavailable_page().into_response(),
    };

    if !can_modify(&fam.role, event.created_by == me.user_id) {
        return forbidden_page().into_response();
    }

    let recurrence = event.rrule.as_deref().and_then(parse_rrule);
    Html(page(
        &fam.header,
        event_id,
        event.is_task,
        &event.title,
        event.description.as_deref().unwrap_or(""),
        event.location.as_deref().unwrap_or(""),
        &to_datetime_local(event.starts_at),
        &to_datetime_local(event.ends_at),
        event.all_day,
        recurrence.as_ref(),
        None,
    ))
    .into_response()
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(event_id): Path<Uuid>,
    Form(form): Form<EventForm>,
) -> Response {
    let Some(fam) =
        family_context(&state, &headers, &me, &format!("/agenda/{event_id}/edit")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };

    // Reconstruct the recurrence for re-rendering the picker on an inline
    // error, tolerating a bad picker value (falls back to "Aucune").
    let rrule_result = rrule_from_form(&form);
    let rec_for_render: Option<Recurrence> = rrule_result
        .as_ref()
        .ok()
        .and_then(|o| o.as_deref())
        .and_then(parse_rrule);

    let render_error = |code: &str| {
        Html(page(
            &fam.header,
            event_id,
            // is_task display: unknown here without a fetch; the field is
            // read-only anyway, default to false label rather than a round
            // trip on an error path.
            false,
            &form.title,
            &form.description,
            &form.location,
            &form.starts_at,
            &form.ends_at,
            form.all_day.is_some(),
            rec_for_render.as_ref(),
            Some(error_message(code)),
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
    let Ok(rrule_opt) = rrule_result else {
        return render_error("invalid_rrule");
    };
    // "Aucune" clears an existing rule: the backend treats an empty string
    // as NULL; `None` would instead leave the rule unchanged.
    let rrule = Some(rrule_opt.unwrap_or_default());

    let body = UpdateEventRequest {
        title: Some(form.title.trim().to_string()),
        description: Some(form.description.trim().to_string()),
        location: Some(form.location.trim().to_string()),
        starts_at: Some(starts_at),
        ends_at: Some(ends_at),
        all_day: Some(form.all_day.is_some()),
        rrule,
        completed: None,
        occurrence_at: None,
    };

    let cookie = agenda_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::PATCH,
        &format!("/groups/{}/events/{}", fam.gid, event_id),
        cookie.as_deref(),
        Some(serde_json::to_value(&body).unwrap()),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            Redirect::to(&format!("/agenda/{event_id}?notice=updated")).into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            Redirect::to(&format!("/agenda/{event_id}?error=forbidden")).into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            event_not_found_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            render_error("ends_before_starts")
        }
        Ok(_) | Err(_) => render_error("unavailable"),
    }
}
