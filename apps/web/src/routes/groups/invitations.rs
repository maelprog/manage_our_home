//! `/groups/invitations/:token/accept` — the landing page for an
//! invitation link (the exact URL shape apps/api emails out, see
//! `apps/api/src/groups/mod.rs::create_invitation`'s
//! `{frontend_base_url}/groups/invitations/{token}/accept`). GET shows a
//! confirm page (auth-gated: an anonymous visitor is bounced to /login by
//! `CurrentUser` and can come back after logging in); POST calls the API.
//!
//! Error table (`accept_invitation`): 404 unknown token, 410 Gone when
//! already consumed (single-use) or past the 7-day expiry.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use leptos::prelude::*;
use manage_our_home_shared::dto::groups::AcceptInvitationResponse;
use uuid::Uuid;

use crate::app::shell;
use crate::family::set_active_group_cookie;
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::cookie_of;

fn invalid_page() -> Html<String> {
    let body = view! {
        <h1>"Invitation invalide"</h1>
        <p>"Ce lien d'invitation n'existe pas."</p>
        <a class="button secondary" href="/groups">"Retour à mes groupes"</a>
    };
    Html(shell("Invitation invalide", &body.to_html()))
}

fn gone_page() -> Html<String> {
    let body = view! {
        <h1>"Invitation expirée"</h1>
        <p>"Cette invitation a déjà été utilisée ou a expiré (elles sont valables 7 jours et à usage unique). Demandez-en une nouvelle à un membre du groupe."</p>
        <a class="button secondary" href="/groups">"Retour à mes groupes"</a>
    };
    Html(shell("Invitation expirée", &body.to_html()))
}

pub async fn get(CurrentUser(_me): CurrentUser, Path(token): Path<String>) -> Response {
    // Parse before echoing into the form action (same pattern as
    // verify_email.rs): a non-UUID token is "Invitation invalide".
    let Ok(token) = token.parse::<Uuid>() else {
        return invalid_page().into_response();
    };
    let action = format!("/groups/invitations/{token}/accept");
    let body = view! {
        <h1>"Rejoindre un groupe"</h1>
        <p>"Vous avez été invité à rejoindre un groupe familial. Confirmez pour en devenir membre."</p>
        <form method="post" action=action>
            <button type="submit">"Rejoindre le groupe"</button>
        </form>
        <div class="links">
            <a href="/groups">"Annuler"</a>
        </div>
    };
    Html(shell("Rejoindre un groupe", &body.to_html())).into_response()
}

pub async fn post(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Response {
    let Ok(token) = token.parse::<Uuid>() else {
        return invalid_page().into_response();
    };

    let cookie = cookie_of(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/invitations/{token}/accept"),
        cookie.as_deref(),
        None,
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            let mut response_headers = HeaderMap::new();
            // The group just joined becomes the active family.
            if let Ok(accepted) = serde_json::from_value::<AcceptInvitationResponse>(resp.body) {
                if let Ok(v) = set_active_group_cookie(accepted.group_id).parse() {
                    response_headers.insert(axum::http::header::SET_COOKIE, v);
                }
            }
            (response_headers, Redirect::to("/groups?notice=joined")).into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => invalid_page().into_response(),
        Ok(resp) if resp.status == reqwest::StatusCode::GONE => gone_page().into_response(),
        Ok(_) | Err(_) => {
            let body = view! {
                <h1>"Service momentanément indisponible"</h1>
                <p>"Merci de réessayer dans quelques instants."</p>
                <a class="button secondary" href="/groups">"Retour à mes groupes"</a>
            };
            Html(shell("Service indisponible", &body.to_html())).into_response()
        }
    }
}
