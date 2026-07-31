//! `/groups/new` — create a family/group. Error table (from
//! `apps/api/src/groups/mod.rs::create_group`): 422 `too_many_groups`
//! when the caller already belongs to 10 groups; the empty-name rule is
//! enforced client-side via the shared `validate_group_name` (the same
//! rule the API applies on rename). On success (201) the new group
//! becomes the active family immediately.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use leptos::prelude::*;
use manage_our_home_shared::dto::groups::{CreateGroupRequest, GroupResponse};
use manage_our_home_shared::validation::groups::validate_group_name;

use crate::app::{shell_with_header, Width};
use crate::family::set_active_group_cookie;
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{cookie_of, header_with_groups};

fn page(header: &str, name: &str, error: Option<&str>) -> String {
    let body = view! {
        <h1>"Créer un groupe"</h1>
        {error.map(|e| view! { <p class="notice error">{e.to_string()}</p> })}
        <form method="post" action="/groups/new">
            <label>
                "Nom du groupe"
                <input type="text" name="name" required=true value=name.to_string() />
            </label>
            <button type="submit">"Créer le groupe"</button>
        </form>
        <div class="links">
            <a href="/groups">"Retour à mes groupes"</a>
        </div>
    };
    shell_with_header(Width::Form, "Créer un groupe", header, &body.to_html())
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let (_groups, header) = header_with_groups(&state, &headers, &me, "/groups/new").await;
    Html(page(&header, "", None))
}

#[derive(serde::Deserialize)]
pub struct NewGroupForm {
    name: String,
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<NewGroupForm>,
) -> Response {
    let (_groups, header) = header_with_groups(&state, &headers, &me, "/groups/new").await;

    if validate_group_name(&form.name).is_err() {
        return Html(page(
            &header,
            &form.name,
            Some("Le nom du groupe ne peut pas être vide."),
        ))
        .into_response();
    }

    let cookie = cookie_of(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        "/groups",
        cookie.as_deref(),
        Some(serde_json::json!(CreateGroupRequest {
            name: form.name.trim().to_string(),
        })),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            let mut response_headers = HeaderMap::new();
            // The freshly created group becomes the active family.
            if let Ok(group) = serde_json::from_value::<GroupResponse>(resp.body) {
                if let Ok(v) = set_active_group_cookie(group.id).parse() {
                    response_headers.insert(axum::http::header::SET_COOKIE, v);
                }
            }
            (
                response_headers,
                Redirect::to("/groups?notice=group_created"),
            )
                .into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNPROCESSABLE_ENTITY => Html(page(
            &header,
            &form.name,
            // Only 422 create_group emits: `too_many_groups` (limit 10).
            Some("Vous avez atteint la limite de 10 groupes par personne."),
        ))
        .into_response(),
        Ok(_) => Html(page(
            &header,
            &form.name,
            Some("Une erreur est survenue, merci de réessayer."),
        ))
        .into_response(),
        Err(_) => Html(page(
            &header,
            &form.name,
            Some("Service momentanément indisponible, merci de réessayer."),
        ))
        .into_response(),
    }
}
