use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use leptos::prelude::*;

use crate::app::{authenticated_header, shell};
use crate::layout::CurrentUser;
use crate::state::AppState;

/// AC #2: "an authenticated placeholder home page" — every other front
/// epic (Groups, Agenda, ...) replaces this once it lands; for this epic
/// it only needs to prove the session round-trip works end to end.
pub async fn get(CurrentUser(me): CurrentUser) -> impl IntoResponse {
    let header = authenticated_header(&me);
    let body = view! {
        <div inner_html=header></div>
        <h1>"Bienvenue"</h1>
        <p>"Vous êtes connecté."</p>
    };
    Html(shell("Accueil", &body.to_html()))
}

/// `POST /logout` on apps/web itself: forwards to `POST /auth/logout` on
/// apps/api (revoking the session server-side), then always redirects to
/// `/login` — matching AC #7 ("Logout clears the session cookie and
/// redirects to /login").
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let cookie_header = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok());

    let mut req = state
        .http
        .post(format!("{}/auth/logout", state.api_internal_base_url));
    if let Some(cookie) = cookie_header {
        req = req.header("cookie", cookie);
    }
    let api_resp = req.send().await.ok();

    let mut response_headers = HeaderMap::new();
    if let Some(resp) = api_resp {
        if let Some(set_cookie) = resp.headers().get(axum::http::header::SET_COOKIE) {
            response_headers.insert(axum::http::header::SET_COOKIE, set_cookie.clone());
        }
    }
    (response_headers, Redirect::to("/login")).into_response()
}

/// Landing page for the post-Google-OAuth redirect. In practice
/// apps/api's `/auth/google/callback` already redirects straight to `/`
/// once the session cookie is set (`state.frontend_base_url`, see
/// `apps/api/src/auth/oauth_google.rs::callback`), so the root layout's
/// own `GET /auth/me` check is what actually confirms the session. This
/// route exists as a defensive landing spot matching issue #15's route
/// table (`/auth/google/callback` — "confirms session cookie present,
/// redirects to /") in case that redirect target ever points here
/// instead.
pub async fn google_callback(CurrentUserOpt(me): CurrentUserOpt) -> impl IntoResponse {
    if me.is_some() {
        Redirect::to("/")
    } else {
        Redirect::to("/login")
    }
}

pub use crate::layout::CurrentUserOpt;
