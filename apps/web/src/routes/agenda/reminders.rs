//! `/agenda/:id/reminders` — add a reminder (offset only; the backend
//! materializes the `scheduled_notifications` rows, the UI does no
//! client-side scheduling). Backend exposes no *list* endpoint, so the
//! created reminder is surfaced once (via `?rid=&offset=` on the detail
//! page) with a delete link — see the detail-page note. `…/reminders/:rid/delete`
//! removes a known reminder.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::agenda::{CreateReminderRequest, ReminderResponse};
use uuid::Uuid;

use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::new::REMINDER_OPTIONS;
use super::{agenda_cookie, event_not_found_page, family_context};

#[derive(serde::Deserialize)]
pub struct AddReminderForm {
    #[serde(default)]
    reminder: String,
}

pub async fn add(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(event_id): Path<Uuid>,
    Form(form): Form<AddReminderForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, &format!("/agenda/{event_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let detail = format!("/agenda/{event_id}");

    // "Aucun rappel": nothing to do.
    let Some((_, _, minutes)) = REMINDER_OPTIONS
        .iter()
        .find(|(k, _, _)| *k == form.reminder)
    else {
        return Redirect::to(&detail).into_response();
    };

    let cookie = agenda_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/events/{}/reminders", fam.gid, event_id),
        cookie.as_deref(),
        Some(
            serde_json::to_value(CreateReminderRequest {
                offset_minutes: *minutes,
            })
            .unwrap(),
        ),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            match serde_json::from_value::<ReminderResponse>(resp.body) {
                Ok(r) => Redirect::to(&format!(
                    "{detail}?notice=reminder_added&rid={}&offset={}",
                    r.id, r.offset_minutes
                ))
                .into_response(),
                Err(_) => Redirect::to(&format!("{detail}?notice=reminder_added")).into_response(),
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            event_not_found_page().into_response()
        }
        Ok(_) | Err(_) => Redirect::to(&format!("{detail}?error=unavailable")).into_response(),
    }
}

pub async fn delete(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((event_id, reminder_id)): Path<(Uuid, Uuid)>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, &format!("/agenda/{event_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let detail = format!("/agenda/{event_id}");
    let cookie = agenda_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &format!(
            "/groups/{}/events/{}/reminders/{}",
            fam.gid, event_id, reminder_id
        ),
        cookie.as_deref(),
        None,
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            format!("{detail}?notice=reminder_deleted")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            format!("{detail}?error=reminder_not_found")
        }
        Ok(_) | Err(_) => format!("{detail}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}
