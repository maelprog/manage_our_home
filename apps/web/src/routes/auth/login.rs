use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::LoginRequest;

use crate::app::{password_field, shell, Width};
use crate::layout::RedirectIfAuthenticated;
use crate::state::{api_post_json, AppState};

#[derive(serde::Deserialize)]
pub struct LoginForm {
    email: String,
    password: String,
}

fn page(email: &str, error: Option<&str>, api_public_base_url: &str) -> String {
    let google_start = format!("{api_public_base_url}/auth/google/start");
    let pw = password_field("Mot de passe", "password", "current-password", false);
    let body = view! {
        <h1>"Se connecter"</h1>
        {error.map(|e| view! { <p class="notice error">{e.to_string()}</p> })}
        <form method="post" action="/login">
            <label>
                "Email"
                <input type="email" name="email" required=true value=email.to_string() />
            </label>
            <div inner_html=pw></div>
            <button type="submit">"Se connecter"</button>
        </form>
        <div class="actions">
            <a class="btn secondary" href=google_start>"Continuer avec Google"</a>
        </div>
        <div class="links">
            <a href="/register">"Créer un compte"</a>
            <a href="/forgot-password">"Mot de passe oublié ?"</a>
            <a href="/privacy-policy">"Politique de confidentialité"</a>
        </div>
    };
    shell(Width::Form, "Connexion", &body.to_html())
}

pub async fn get(
    _redirect: RedirectIfAuthenticated,
    State(state): State<AppState>,
) -> impl IntoResponse {
    Html(page("", None, &state.api_public_base_url))
}

pub async fn post(
    _redirect: RedirectIfAuthenticated,
    State(state): State<AppState>,
    Form(form): Form<LoginForm>,
) -> Response {
    let result = api_post_json(
        &state,
        "/auth/login",
        LoginRequest {
            email: form.email.clone(),
            password: form.password.clone(),
        },
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            let mut headers = HeaderMap::new();
            if let Some(cookie) = resp.set_cookie {
                if let Ok(v) = cookie.parse() {
                    headers.insert(axum::http::header::SET_COOKIE, v);
                }
            }
            (headers, Redirect::to("/")).into_response()
        }
        // Backend deliberately returns a generic 401 for wrong
        // email/password/unverified/Google-only account — the UI mirrors
        // that and doesn't try to distinguish further (issue #15's error
        // table).
        Ok(resp) if resp.status == reqwest::StatusCode::UNAUTHORIZED => Html(page(
            &form.email,
            Some("Email ou mot de passe incorrect."),
            &state.api_public_base_url,
        ))
        .into_response(),
        Ok(_) => Html(page(
            &form.email,
            Some("Une erreur est survenue, merci de réessayer."),
            &state.api_public_base_url,
        ))
        .into_response(),
        Err(_) => Html(page(
            &form.email,
            Some("Service momentanément indisponible, merci de réessayer."),
            &state.api_public_base_url,
        ))
        .into_response(),
    }
}
