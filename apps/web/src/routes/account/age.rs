//! `/account/age` — the age declaration asked after the fact (#318). The
//! registration form records the art. 8 GDPR declaration (#137); an account
//! opened through Google, or before #137, has none on file. Its session
//! opens this page and nothing else: every other page's extractor
//! (`CurrentUser`, `CurrentSuperAdmin`, `RedirectIfAuthenticated`) sends it
//! here, and apps/api refuses it everywhere else with 403
//! `age_not_declared`. The box is the registration's own.
//!
//! Under-15s are not accepted, and no parental-consent path exists: the page
//! says so, and its holder can log out. No app header: none of its links
//! would open.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::auth::AgeDeclarationRequest;
use manage_our_home_shared::validation::auth::{validate_age_declaration, MINIMUM_AGE_YEARS};

use crate::app::{html_escape, shell, Width};
use crate::layout::{AGE_DECLARATION_PAGE, DEACTIVATED_PAGE, TERMS_ACCEPTANCE_PAGE};
use crate::state::{api_request_auth, fetch_session, AppState, Session};

use super::{account_cookie, service_unavailable_page};

const TITLE: &str = "Déclaration d'âge";

#[derive(serde::Deserialize)]
pub struct PageQuery {
    error: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct DeclarationForm {
    /// Same shape as the registration's box: an unticked checkbox is absent
    /// from the POST, so the presence of the field *is* the declaration.
    minimum_age: Option<String>,
}

/// The banner for an `?error=` code, `None` for no or an unknown code.
fn error_message(error: Option<&str>) -> Option<String> {
    match error {
        Some("age_declaration_required") => Some(format!(
            "Le service n'est pas ouvert aux moins de {MINIMUM_AGE_YEARS} ans : \
             cochez la case pour déclarer votre âge."
        )),
        Some("unavailable") => {
            Some("Service momentanément indisponible, merci de réessayer.".to_string())
        }
        _ => None,
    }
}

fn page(error: Option<&str>) -> String {
    let error = error_message(error)
        .map(|text| format!(r#"<p class="notice error">{}</p>"#, html_escape(&text)))
        .unwrap_or_default();
    format!(
        r#"<h1>{TITLE}</h1>
{error}
<p>Le service n'est pas ouvert aux moins de {MINIMUM_AGE_YEARS} ans. Votre compte a été ouvert sans cette déclaration — avec Google, ou avant qu'elle soit demandée à l'inscription : elle vous est demandée une fois, avant l'accès à l'application.</p>
<form method="post" action="{AGE_DECLARATION_PAGE}">
<label class="field inline">
<input type="checkbox" name="minimum_age" value="1"/>
<span>Je déclare avoir {MINIMUM_AGE_YEARS} ans ou plus.</span>
</label>
<button type="submit">Continuer</button>
</form>
<p class="muted">Si vous avez moins de {MINIMUM_AGE_YEARS} ans, le service ne vous est pas ouvert : déconnectez-vous. Le titulaire de l'autorité parentale peut demander la fermeture du compte et la suppression de ses données à l'adresse donnée dans les mentions légales.</p>
<form method="post" action="/logout">
<button type="submit" class="secondary">Se déconnecter</button>
</form>
<div class="links">
<a href="/privacy-policy">Politique de confidentialité</a>
<a href="/terms-of-service">Conditions générales d'utilisation</a>
<a href="/legal-notice">Mentions légales</a>
</div>"#
    )
}

/// `GET /account/age`. Only the session of an account without declaration
/// gets the page; any other is sent where it belongs.
pub async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Response {
    let cookie = account_cookie(&headers);
    match fetch_session(&state, cookie.as_deref()).await {
        Session::AgeUndeclared => {
            Html(shell(Width::Form, TITLE, &page(query.error.as_deref()))).into_response()
        }
        Session::Active(_) => Redirect::to("/").into_response(),
        Session::Deactivated => Redirect::to(DEACTIVATED_PAGE).into_response(),
        Session::TermsNotAccepted => Redirect::to(TERMS_ACCEPTANCE_PAGE).into_response(),
        Session::None => Redirect::to("/login").into_response(),
    }
}

/// `POST /account/age` — checks the box locally, then relays to apps/api's
/// `POST /auth/age-declaration`. 204 → `/`, now open; 422 → the page with
/// the registration's message; 401 (not, or no longer, a session awaiting
/// the declaration) → `/`, whose own extractor sorts it out; anything else
/// → the unavailable banner.
pub async fn post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<DeclarationForm>,
) -> Response {
    let declares_minimum_age = form.minimum_age.is_some();
    if let Err(code) = validate_age_declaration(declares_minimum_age) {
        return Redirect::to(&format!("{AGE_DECLARATION_PAGE}?error={code}")).into_response();
    }
    let cookie = account_cookie(&headers);
    let body = serde_json::to_value(AgeDeclarationRequest {
        declares_minimum_age,
    })
    .unwrap_or_default();
    match api_request_auth(
        &state,
        reqwest::Method::POST,
        "/auth/age-declaration",
        cookie.as_deref(),
        Some(body),
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            Redirect::to("/").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNPROCESSABLE_ENTITY => Redirect::to(
            &format!("{AGE_DECLARATION_PAGE}?error=age_declaration_required"),
        )
        .into_response(),
        Ok(resp) if resp.status == reqwest::StatusCode::UNAUTHORIZED => {
            Redirect::to("/").into_response()
        }
        Ok(_) => Redirect::to(&format!("{AGE_DECLARATION_PAGE}?error=unavailable")).into_response(),
        Err(_) => service_unavailable_page().into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declining_gets_the_registration_message() {
        let message = error_message(Some("age_declaration_required")).unwrap();
        assert!(
            message.contains(&format!("moins de {MINIMUM_AGE_YEARS} ans")),
            "{message}"
        );
    }

    #[test]
    fn an_unavailable_api_says_so() {
        assert!(error_message(Some("unavailable"))
            .unwrap()
            .contains("indisponible"));
    }

    /// The code comes from the query string: anything unknown shows nothing,
    /// not a banner echoing it.
    #[test]
    fn no_or_an_unknown_code_shows_no_banner() {
        assert_eq!(error_message(None), None);
        assert_eq!(error_message(Some("<script>")), None);
        assert!(!page(Some("<script>")).contains("notice error"));
    }

    /// The box starts unticked — the declaration is the holder's act — and
    /// says the threshold the API enforces.
    #[test]
    fn the_page_asks_for_the_declaration_unticked() {
        let html = page(None);
        assert!(html.contains(r#"name="minimum_age""#), "{html}");
        assert!(!html.contains("checked"), "{html}");
        assert!(
            html.contains(&format!(
                "Je déclare avoir {MINIMUM_AGE_YEARS} ans ou plus."
            )),
            "{html}"
        );
        assert!(html.contains(r#"action="/logout""#), "{html}");
    }
}
