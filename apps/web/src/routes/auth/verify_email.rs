use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse};
use leptos::prelude::*;
use uuid::Uuid;

use crate::app::shell;
use crate::state::{api_get, AppState};

#[derive(serde::Deserialize)]
pub struct VerifyEmailQuery {
    token: String,
}

fn invalid_link() -> (&'static str, String) {
    let v = view! {
        <h1>"Lien invalide"</h1>
        <p>"Ce lien de vérification n'existe pas."</p>
        <a class="btn secondary" href="/login">"Retour à la connexion"</a>
    };
    ("Lien invalide", v.to_html())
}

pub async fn get(
    State(state): State<AppState>,
    Query(query): Query<VerifyEmailQuery>,
) -> impl IntoResponse {
    // Parse before interpolating into the internal API URL (same pattern as
    // reset_password.rs): a non-UUID token is "Lien invalide", not a
    // transport error, and a UUID is URL-safe by construction.
    let Ok(token) = Uuid::parse_str(&query.token) else {
        let (title, body_html) = invalid_link();
        return Html(shell(title, &body_html));
    };
    let result = api_get(&state, &format!("/auth/verify-email?token={token}")).await;

    let (title, body_html): (&str, String) = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            let v = view! {
                <h1>"Email vérifié"</h1>
                <p>"Votre adresse email est confirmée. Vous pouvez maintenant vous connecter."</p>
                <a class="btn" href="/login">"Se connecter"</a>
            };
            ("Email vérifié", v.to_html())
        }
        Ok(resp) if resp.status == reqwest::StatusCode::GONE => {
            let v = view! {
                <h1>"Lien expiré"</h1>
                <p>"Ce lien de vérification a déjà été utilisé ou a expiré. Merci de recréer un compte ou de contacter le support pour en obtenir un nouveau."</p>
                <a class="btn secondary" href="/login">"Retour à la connexion"</a>
            };
            ("Lien expiré", v.to_html())
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => invalid_link(),
        _ => {
            let v = view! {
                <h1>"Service momentanément indisponible"</h1>
                <p>"Merci de réessayer dans quelques instants."</p>
            };
            ("Service indisponible", v.to_html())
        }
    };

    Html(shell(title, &body_html))
}
