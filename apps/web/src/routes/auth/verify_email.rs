use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse};
use leptos::prelude::*;
use manage_our_home_shared::validation::auth::is_bearer_token;

use crate::app::{shell, Width};
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

fn expired_link() -> (&'static str, String) {
    let v = view! {
        <h1>"Lien expiré"</h1>
        <p>"Ce lien de vérification a déjà été utilisé ou a expiré. Si votre compte attend encore sa vérification, demandez un nouvel email."</p>
        <div class="actions">
            <a class="btn" href="/verify-email/resend">"Renvoyer l'email de vérification"</a>
            <a class="btn secondary" href="/login">"Retour à la connexion"</a>
        </div>
    };
    ("Lien expiré", v.to_html())
}

pub async fn get(
    State(state): State<AppState>,
    Query(query): Query<VerifyEmailQuery>,
) -> impl IntoResponse {
    // Checked before interpolating into the internal API URL (same pattern
    // as reset_password.rs): a token not spelled as the api hands them out
    // is "Lien invalide", not a transport error, and one that is is
    // URL-safe by construction (base64url, #335).
    if !is_bearer_token(&query.token) {
        let (title, body_html) = invalid_link();
        return Html(shell(Width::Form, title, &body_html));
    }
    let result = api_get(&state, &format!("/auth/verify-email?token={}", query.token)).await;

    let (title, body_html): (&str, String) = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            let v = view! {
                <h1>"Email vérifié"</h1>
                <p>"Votre adresse email est confirmée. Vous pouvez maintenant vous connecter."</p>
                <a class="btn" href="/login">"Se connecter"</a>
            };
            ("Email vérifié", v.to_html())
        }
        Ok(resp) if resp.status == reqwest::StatusCode::GONE => expired_link(),
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => invalid_link(),
        _ => {
            let v = view! {
                <h1>"Service momentanément indisponible"</h1>
                <p>"Merci de réessayer dans quelques instants."</p>
            };
            ("Service indisponible", v.to_html())
        }
    };

    Html(shell(Width::Form, title, &body_html))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #420: an expired link sent its reader to register again — which
    /// answers "Un compte existe déjà avec cet email" — or to a support the
    /// page does not name. The way out is the resend form.
    #[test]
    fn an_expired_link_leads_to_the_resend_form() {
        let (_, html) = expired_link();
        assert!(html.contains(r#"href="/verify-email/resend""#), "{html}");
        assert!(!html.contains("recréer un compte"), "{html}");
        assert!(html.contains(r#"href="/login""#), "{html}");
    }
}
