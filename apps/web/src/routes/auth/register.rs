use axum::extract::State;
use axum::response::{Html, IntoResponse, Redirect};
use axum::Form;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::RegisterRequest;
use manage_our_home_shared::validation::auth::{
    validate_age_declaration, validate_display_name, validate_email, validate_password,
    MINIMUM_AGE_YEARS,
};

use crate::app::{password_error_message, password_field, shell, Width};
use crate::layout::RedirectIfAuthenticated;
use crate::state::{api_post_json, AppState};

#[derive(serde::Deserialize)]
pub struct RegisterForm {
    email: String,
    password: String,
    display_name: String,
    /// The art. 8 GDPR age declaration (#137). An unticked checkbox is simply
    /// absent from the POST, so the presence of the field *is* the
    /// declaration — same shape as the deletion consent box
    /// (`routes::account::delete`).
    minimum_age: Option<String>,
}

fn page(
    email: &str,
    display_name: &str,
    declares_minimum_age: bool,
    field_error: Option<&str>,
    error: Option<&str>,
    api_public_base_url: &str,
) -> String {
    let google_start = format!("{api_public_base_url}/auth/google/start");
    let pw = password_field("Mot de passe", "password", "new-password", true);
    // #137: the service is not open under MINIMUM_AGE_YEARS, and says so here
    // rather than only in the CGU — a rule nobody is shown is a rule nobody
    // follows. The box keeps its state when the form comes back with an error,
    // so a corrected form is not silently unticked.
    let age_checked = if declares_minimum_age { " checked" } else { "" };
    let age_field = format!(
        r#"<label class="field inline">
<input type="checkbox" name="minimum_age" value="1"{age_checked}/>
<span>Je déclare avoir {MINIMUM_AGE_YEARS} ans ou plus.</span>
</label>"#
    );
    let body = view! {
        <h1>"Créer un compte"</h1>
        {error.map(|e| view! {
            <p class="notice error">{e.to_string()}</p>
        })}
        <form method="post" action="/register">
            <label>
                "Email"
                <input type="email" name="email" required=true value=email.to_string() />
                {field_error.map(|e| view! { <span class="field-error">{e.to_string()}</span> })}
            </label>
            <label>
                "Nom affiché"
                <input type="text" name="display_name" required=true value=display_name.to_string() />
            </label>
            <div inner_html=pw></div>
            <div inner_html=age_field></div>
            <button type="submit">"Créer mon compte"</button>
        </form>
        <div class="actions">
            <a class="btn secondary" href=google_start>"Continuer avec Google"</a>
        </div>
        <div class="links">
            <a href="/login">"J'ai déjà un compte"</a>
            // RGPD (front epic F10): the policy must be readable *before*
            // creating an account, so it is linked from the unauthenticated
            // pages and served without a session. Same for the CGU and the
            // legal notice since #132 — creating an account accepts the CGU,
            // which makes reading them beforehand the whole point.
            <a href="/privacy-policy">"Politique de confidentialité"</a>
            <a href="/terms-of-service">"Conditions générales d'utilisation"</a>
            <a href="/legal-notice">"Mentions légales"</a>
        </div>
    };
    shell(Width::Form, "Créer un compte", &body.to_html())
}

pub async fn get(
    _redirect: RedirectIfAuthenticated,
    State(state): State<AppState>,
) -> impl IntoResponse {
    Html(page("", "", false, None, None, &state.api_public_base_url))
}

pub async fn post(
    _redirect: RedirectIfAuthenticated,
    State(state): State<AppState>,
    Form(form): Form<RegisterForm>,
) -> impl IntoResponse {
    let declares_minimum_age = form.minimum_age.is_some();
    if validate_email(&form.email).is_err() {
        return Html(page(
            &form.email,
            &form.display_name,
            declares_minimum_age,
            Some("Adresse email invalide."),
            None,
            &state.api_public_base_url,
        ))
        .into_response();
    }
    if validate_display_name(&form.display_name).is_err() {
        return Html(page(
            &form.email,
            &form.display_name,
            declares_minimum_age,
            None,
            Some("Le nom affiché ne peut pas être vide."),
            &state.api_public_base_url,
        ))
        .into_response();
    }
    if let Err(code) = validate_password(&form.password) {
        return Html(page(
            &form.email,
            &form.display_name,
            declares_minimum_age,
            None,
            Some(&password_error_message(code)),
            &state.api_public_base_url,
        ))
        .into_response();
    }
    if validate_age_declaration(declares_minimum_age).is_err() {
        return Html(page(
            &form.email,
            &form.display_name,
            declares_minimum_age,
            None,
            Some(&format!(
                "Le service n'est pas ouvert aux moins de {MINIMUM_AGE_YEARS} ans : \
                 cochez la case pour déclarer votre âge."
            )),
            &state.api_public_base_url,
        ))
        .into_response();
    }

    let result = api_post_json(
        &state,
        "/auth/register",
        RegisterRequest {
            email: form.email.clone(),
            password: form.password.clone(),
            display_name: form.display_name.clone(),
            declares_minimum_age,
        },
        None,
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            Redirect::to("/register/check-email").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::CONFLICT => Html(page(
            &form.email,
            &form.display_name,
            declares_minimum_age,
            Some("Un compte existe déjà avec cet email."),
            None,
            &state.api_public_base_url,
        ))
        .into_response(),
        Ok(_) => Html(page(
            &form.email,
            &form.display_name,
            declares_minimum_age,
            None,
            Some("Une erreur est survenue, merci de réessayer."),
            &state.api_public_base_url,
        ))
        .into_response(),
        Err(_) => Html(page(
            &form.email,
            &form.display_name,
            declares_minimum_age,
            None,
            Some("Service momentanément indisponible, merci de réessayer."),
            &state.api_public_base_url,
        ))
        .into_response(),
    }
}

pub async fn check_email() -> impl IntoResponse {
    let body = view! {
        <h1>"Vérifiez votre boîte mail"</h1>
        <p>"Un email de confirmation vous a été envoyé. Cliquez sur le lien qu'il contient pour activer votre compte."</p>
        <div class="links">
            <a href="/login">"Retour à la connexion"</a>
        </div>
    };
    Html(shell(Width::Form, "Vérifiez votre email", &body.to_html()))
}
