use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::Form;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::ForgotPasswordRequest;

use crate::app::shell;
use crate::state::{api_post_json, AppState};

#[derive(serde::Deserialize)]
pub struct ForgotPasswordForm {
    email: String,
}

fn form_page(submitted: bool) -> String {
    let body = if submitted {
        let view = view! {
            <h1>"Mot de passe oublié"</h1>
            // AC #4 / issue #15 error table: always the same generic
            // message regardless of whether the account exists
            // (anti-enumeration), mirroring apps/api's always-200
            // behavior.
            <p class="notice success">"Si ce compte existe, un email a été envoyé."</p>
            <div class="links">
                <a href="/login">"Retour à la connexion"</a>
            </div>
        };
        view.to_html()
    } else {
        let view = view! {
            <h1>"Mot de passe oublié"</h1>
            <form method="post" action="/forgot-password">
                <label>
                    "Email"
                    <input type="email" name="email" required=true />
                </label>
                <button type="submit">"Envoyer le lien de réinitialisation"</button>
            </form>
            <div class="links">
                <a href="/login">"Retour à la connexion"</a>
            </div>
        };
        view.to_html()
    };
    shell("Mot de passe oublié", &body)
}

pub async fn get() -> impl IntoResponse {
    Html(form_page(false))
}

pub async fn post(State(state): State<AppState>, Form(form): Form<ForgotPasswordForm>) -> impl IntoResponse {
    // Always show the generic success state, even if the call to apps/api
    // itself fails transport-wise — the anti-enumeration guarantee must
    // hold regardless (issue #15's error table: "always show ... regardless
    // of outcome").
    let _ = api_post_json(
        &state,
        "/auth/password/forgot",
        ForgotPasswordRequest { email: form.email },
    )
    .await;
    Html(form_page(true))
}
