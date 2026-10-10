use axum::extract::State;
use axum::response::{Html, IntoResponse, Redirect};
use axum::Form;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::RegisterRequest;
use manage_our_home_shared::validation::auth::{
    validate_age_declaration, validate_display_name, validate_email, validate_password,
    validate_terms_acceptance, MINIMUM_AGE_YEARS,
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
    /// The acceptance of the CGU (#319), same shape: present when ticked.
    accepts_terms: Option<String>,
}

/// The boxes of the form, as the visitor left them: a form that comes back
/// with an error keeps them, so a corrected form is not silently unticked.
#[derive(Clone, Copy, Default)]
struct Boxes {
    declares_minimum_age: bool,
    accepts_terms: bool,
}

/// `" checked"` for a ticked box.
fn checked(ticked: bool) -> &'static str {
    if ticked {
        " checked"
    } else {
        ""
    }
}

fn page(
    email: &str,
    display_name: &str,
    boxes: Boxes,
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
    //
    // #319: the CGU are accepted by a box of their own, unticked, whose
    // acceptance apps/api records with the version in force. The text is
    // linked from the page's footer, under the same name.
    let age_field = format!(
        r#"<label class="field inline">
<input type="checkbox" name="minimum_age" value="1"{age_checked}/>
<span>Je déclare avoir {MINIMUM_AGE_YEARS} ans ou plus.</span>
</label>
<label class="field inline">
<input type="checkbox" name="accepts_terms" value="1"{terms_checked}/>
<span>J'accepte les conditions générales d'utilisation.</span>
</label>"#,
        age_checked = checked(boxes.declares_minimum_age),
        terms_checked = checked(boxes.accepts_terms),
    );
    let body = view! {
        <h1>"Créer un compte"</h1>
        {error.map(|e| view! {
            <p class="notice error">{e.to_string()}</p>
        })}
        <form method="post" action="/register">
            <label>
                "Email"
                // #145: the message below joins the field's name by sitting
                // in its label; `aria-invalid` carries the *state*, and is
                // absent rather than "false" when there is nothing wrong.
                <input
                    type="email"
                    name="email"
                    required=true
                    value=email.to_string()
                    aria-invalid=field_error.map(|_| "true")
                />
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
        // #419: stacked, so the Google button takes the width of the form's
        // own.
        <div class="actions stacked">
            <a class="btn secondary" href=google_start>"Continuer avec Google"</a>
        </div>
        // RGPD (front epic F10): the policy must be readable *before*
        // creating an account, so it is linked from the unauthenticated
        // pages and served without a session. Same for the CGU and the
        // legal notice since #132 — creating an account accepts the CGU
        // (the box above, #319), which makes reading them beforehand the
        // whole point. All three are the shell's footer since #419.
        <div class="links">
            <a href="/login">"J'ai déjà un compte"</a>
        </div>
    };
    shell(Width::Form, "Créer un compte", &body.to_html())
}

/// The banner of a form sent without the CGU accepted (#319).
const TERMS_REQUIRED: &str = "Cochez la case pour accepter les conditions générales d'utilisation.";

pub async fn get(
    _redirect: RedirectIfAuthenticated,
    State(state): State<AppState>,
) -> impl IntoResponse {
    Html(page(
        "",
        "",
        Boxes::default(),
        None,
        None,
        &state.api_public_base_url,
    ))
}

pub async fn post(
    _redirect: RedirectIfAuthenticated,
    State(state): State<AppState>,
    Form(form): Form<RegisterForm>,
) -> impl IntoResponse {
    let declares_minimum_age = form.minimum_age.is_some();
    let accepts_terms = form.accepts_terms.is_some();
    let boxes = Boxes {
        declares_minimum_age,
        accepts_terms,
    };
    if validate_email(&form.email).is_err() {
        return Html(page(
            &form.email,
            &form.display_name,
            boxes,
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
            boxes,
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
            boxes,
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
            boxes,
            None,
            Some(&format!(
                "Le service n'est pas ouvert aux moins de {MINIMUM_AGE_YEARS} ans : \
                 cochez la case pour déclarer votre âge."
            )),
            &state.api_public_base_url,
        ))
        .into_response();
    }
    if validate_terms_acceptance(accepts_terms).is_err() {
        return Html(page(
            &form.email,
            &form.display_name,
            boxes,
            None,
            Some(TERMS_REQUIRED),
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
            accepts_terms,
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
            boxes,
            Some("Un compte existe déjà avec cet email."),
            None,
            &state.api_public_base_url,
        ))
        .into_response(),
        Ok(_) => Html(page(
            &form.email,
            &form.display_name,
            boxes,
            None,
            Some("Une erreur est survenue, merci de réessayer."),
            &state.api_public_base_url,
        ))
        .into_response(),
        Err(_) => Html(page(
            &form.email,
            &form.display_name,
            boxes,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The `<input …>` tag carrying `name="{name}"` in the rendered page.
    fn input_named<'a>(html: &'a str, name: &str) -> &'a str {
        let at = html
            .find(&format!(r#"name="{name}""#))
            .unwrap_or_else(|| panic!("no input named {name}: {html}"));
        let open = html[..at]
            .rfind("<input")
            .expect("attribute outside an input");
        let close = at + html[at..].find('>').expect("unterminated input");
        &html[open..=close]
    }

    /// #419: the legal documents are the shell's footer, no longer a line of
    /// the account links — once each, or Playwright's strict
    /// `getByRole("link", { name })` would find two.
    #[test]
    fn each_legal_document_is_linked_once_from_the_footer() {
        let html = page("", "", Boxes::default(), None, None, "http://api");
        for (href, _) in crate::app::LEGAL_DOCUMENTS {
            assert_eq!(
                html.matches(&format!(r#"href="{href}""#)).count(),
                1,
                "{href}: {html}"
            );
        }
        let footer = html.find("<footer").expect("a footer");
        assert!(html.find(r#"href="/privacy-policy""#).unwrap() > footer);
        // The Google button takes the form button's width, as on /login.
        assert!(
            html.contains(r#"<div class="actions stacked"><a href="http://api/auth/google/start" class="btn secondary">"#),
            "{html}"
        );
    }

    #[test]
    fn a_field_in_error_says_so_in_its_state() {
        // #145: the message already reaches the field's accessible name,
        // since it sits inside its `<label>`; the state did not follow.
        let html = page(
            "x",
            "",
            Boxes::default(),
            Some("Adresse email invalide."),
            None,
            "",
        );
        let email = input_named(&html, "email");
        assert!(email.contains(r#"aria-invalid="true""#), "{email}");
    }

    #[test]
    fn a_field_without_error_claims_no_invalid_state() {
        let html = page("", "", Boxes::default(), None, None, "");
        assert!(!html.contains("aria-invalid"), "{html}");
        // A banner error is about the form, not about the email field.
        let html = page("a@b.c", "", Boxes::default(), None, Some("Nom vide."), "");
        assert!(!html.contains("aria-invalid"), "{html}");
    }

    /// #319: the CGU are accepted by a box of their own, unticked on a fresh
    /// form and kept as the visitor left it on a form that comes back.
    #[test]
    fn the_form_asks_for_the_terms_acceptance() {
        let fresh = page("", "", Boxes::default(), None, None, "");
        let terms = input_named(&fresh, "accepts_terms");
        assert!(terms.contains(r#"type="checkbox""#), "{terms}");
        assert!(!terms.contains("checked"), "{terms}");
        assert!(
            fresh.contains("J'accepte les conditions générales d'utilisation."),
            "{fresh}"
        );

        let back = page(
            "a@b.c",
            "A",
            Boxes {
                declares_minimum_age: false,
                accepts_terms: true,
            },
            None,
            Some(TERMS_REQUIRED),
            "",
        );
        assert!(input_named(&back, "accepts_terms").contains("checked"));
        assert!(!input_named(&back, "minimum_age").contains("checked"));
    }
}
