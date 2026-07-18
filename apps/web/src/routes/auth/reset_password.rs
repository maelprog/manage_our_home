use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse};
use axum::Form;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::ResetPasswordRequest;
use manage_our_home_shared::validation::auth::validate_password;
use uuid::Uuid;

use crate::app::{password_error_message, password_field, shell};
use crate::state::{api_post_json, AppState};

#[derive(serde::Deserialize)]
pub struct ResetPasswordQuery {
    token: String,
}

#[derive(serde::Deserialize)]
pub struct ResetPasswordForm {
    token: String,
    new_password: String,
}

fn invalid_link_page(title: &str, message: &str) -> String {
    let title_owned = title.to_string();
    let message_owned = message.to_string();
    let body = view! {
        <h1>{title_owned}</h1>
        <p>{message_owned}</p>
        <a class="button secondary" href="/forgot-password">"Redemander un email"</a>
    };
    shell(title, &body.to_html())
}

fn form_page(token: &str, error: Option<&str>) -> String {
    let token_owned = token.to_string();
    let error_owned = error.map(str::to_string);
    let pw = password_field("Nouveau mot de passe", "new_password", "new-password", true);
    let body = view! {
        <h1>"Réinitialiser le mot de passe"</h1>
        {error_owned.map(|e| view! { <p class="notice error">{e}</p> })}
        <form method="post" action="/reset-password">
            <input type="hidden" name="token" value=token_owned />
            <div inner_html=pw></div>
            <button type="submit">"Réinitialiser"</button>
        </form>
    };
    shell("Réinitialiser le mot de passe", &body.to_html())
}

pub async fn get(Query(query): Query<ResetPasswordQuery>) -> impl IntoResponse {
    if Uuid::parse_str(&query.token).is_err() {
        return Html(invalid_link_page(
            "Lien invalide",
            "Ce lien de réinitialisation n'existe pas.",
        ));
    }
    Html(form_page(&query.token, None))
}

pub async fn post(
    State(state): State<AppState>,
    Form(form): Form<ResetPasswordForm>,
) -> impl IntoResponse {
    let Ok(token) = Uuid::parse_str(&form.token) else {
        return Html(invalid_link_page(
            "Lien invalide",
            "Ce lien de réinitialisation n'existe pas.",
        ));
    };
    if let Err(code) = validate_password(&form.new_password) {
        return Html(form_page(&form.token, Some(&password_error_message(code))));
    }

    let result = api_post_json(
        &state,
        "/auth/password/reset",
        ResetPasswordRequest {
            token,
            new_password: form.new_password,
        },
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            let body = view! {
                <h1>"Mot de passe mis à jour"</h1>
                <p>"Votre mot de passe a été réinitialisé. Toutes vos autres sessions ont été déconnectées."</p>
                <a class="button" href="/login">"Se connecter"</a>
            };
            Html(shell("Mot de passe mis à jour", &body.to_html()))
        }
        Ok(resp) if resp.status == reqwest::StatusCode::GONE => Html(invalid_link_page(
            "Lien expiré",
            "Ce lien de réinitialisation a déjà été utilisé ou a expiré.",
        )),
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => Html(invalid_link_page(
            "Lien invalide",
            "Ce lien de réinitialisation n'existe pas.",
        )),
        _ => Html(form_page(
            &form.token,
            Some("Service momentanément indisponible, merci de réessayer."),
        )),
    }
}
