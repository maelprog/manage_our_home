//! `/account/terms` — the acceptance of the CGU outside registration (#319).
//! The registration form records it; an account opened through Google, or
//! before #319, has none on file. Once its age is declared (#318), its
//! session opens this page and nothing else: every other page's extractor
//! (`CurrentUser`, `CurrentSuperAdmin`, `RedirectIfAuthenticated`) sends it
//! here, and apps/api refuses it everywhere else with 403
//! `terms_not_accepted`. The box is the registration's own.
//!
//! The same `POST` records a member's acknowledgement of a new version, from
//! [`update_notice`] — the notice the home page and `/account` show to a
//! member who accepted an earlier text, until they acknowledge it. No app
//! header on the page: none of its links would open.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use chrono::Utc;
use manage_our_home_shared::dto::auth::{MeResponse, TermsAcceptanceRequest};
use manage_our_home_shared::validation::auth::{
    format_terms_version, paris_day, terms_announced_on, terms_in_force_on, terms_update_pending,
    validate_terms_acceptance,
};

use crate::app::{html_escape, shell, Width};
use crate::layout::{AGE_DECLARATION_PAGE, DEACTIVATED_PAGE, TERMS_ACCEPTANCE_PAGE};
use crate::routes::legal::ANNOUNCED_TERMS_PAGE;
use crate::state::{api_request_auth, fetch_session, AppState, Session};

use super::{account_cookie, service_unavailable_page};

const TITLE: &str = "Conditions d'utilisation";

#[derive(serde::Deserialize)]
pub struct PageQuery {
    error: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct AcceptanceForm {
    /// Same shape as the registration's box: an unticked checkbox is absent
    /// from the POST, so the presence of the field *is* the acceptance.
    accepts_terms: Option<String>,
}

/// The banner for an `?error=` code, `None` for no or an unknown code.
fn error_message(error: Option<&str>) -> Option<&'static str> {
    match error {
        Some("terms_acceptance_required") => Some(
            "Le service n'est ouvert qu'aux membres qui en acceptent les conditions : \
             cochez la case pour les accepter.",
        ),
        Some("unavailable") => Some("Service momentanément indisponible, merci de réessayer."),
        _ => None,
    }
}

fn page(error: Option<&str>) -> String {
    let error = error_message(error)
        .map(|text| format!(r#"<p class="notice error">{}</p>"#, html_escape(text)))
        .unwrap_or_default();
    let version = html_escape(&format_terms_version(terms_in_force_on(paris_day(
        Utc::now(),
    ))));
    format!(
        r#"<h1>{TITLE}</h1>
{error}
<p>Votre compte a été ouvert sans que l'acceptation des conditions générales d'utilisation soit enregistrée — avec Google, ou avant qu'elle soit demandée à l'inscription : elle vous est demandée une fois, avant l'accès à l'application.</p>
<p><a href="/terms-of-service">Lire les conditions (version du {version})</a></p>
<form method="post" action="{TERMS_ACCEPTANCE_PAGE}">
<label class="field inline">
<input type="checkbox" name="accepts_terms" value="1"/>
<span>J'accepte les conditions générales d'utilisation.</span>
</label>
<button type="submit">Continuer</button>
</form>
<p class="muted">Si vous ne les acceptez pas, le service ne vous est pas ouvert : déconnectez-vous. Vous pouvez demander la fermeture du compte et la suppression de ses données à l'adresse donnée dans les mentions légales.</p>
<form method="post" action="/logout">
<button type="submit" class="secondary">Se déconnecter</button>
</form>
<div class="links">
<a href="/privacy-policy">Politique de confidentialité</a>
<a href="/legal-notice">Mentions légales</a>
</div>"#
    )
}

/// The notice that tells a member the CGU changed since the version they
/// accepted (#319), with the acknowledgement that records the new one — or,
/// before a version announced applies, that it will (#367). Empty when there
/// is nothing to tell — see [`notice_for`]. The day is read on every call.
pub(crate) fn update_notice(me: &MeResponse) -> String {
    let today = paris_day(Utc::now());
    notice_for(
        me.terms_accepted_version.as_deref(),
        terms_in_force_on(today),
        terms_announced_on(today).map(|announced| announced.version),
    )
}

/// The notice for a member who accepted `accepted`, on a day `in_force` is
/// the version in force and `announced` the one announced, if any.
///
/// - An acceptance that does not cover `in_force`: the CGU changed, with the
///   acknowledgement — acceptance is asked for from the version's date on,
///   not before (#367).
/// - Otherwise, one that does not cover `announced`: the CGU will change on
///   its date, with the link to the announced text and nothing to
///   acknowledge yet.
/// - Otherwise, or with no acceptance on file — an account held at the
///   acceptance page, which a full session never is — nothing.
fn notice_for(accepted: Option<&str>, in_force: &str, announced: Option<&str>) -> String {
    if terms_update_pending(accepted, in_force) {
        return changed_notice(in_force);
    }
    match announced {
        Some(announced) if terms_update_pending(accepted, announced) => announced_notice(announced),
        _ => String::new(),
    }
}

/// The notice that the version `version` is in force and replaces the one the
/// member accepted.
fn changed_notice(version: &str) -> String {
    format!(
        r#"<section class="notice">
<h2>Les conditions d'utilisation ont changé</h2>
<p>Une nouvelle version des conditions générales d'utilisation, en vigueur à partir du {version}, remplace celle que vous aviez acceptée. Continuer à utiliser le service après cette date vaut acceptation de la nouvelle version.</p>
<p><a href="/terms-of-service">Lire la nouvelle version</a></p>
<form method="post" action="{TERMS_ACCEPTANCE_PAGE}">
<input type="hidden" name="accepts_terms" value="1"/>
<button type="submit" class="secondary">J'en ai pris connaissance</button>
</form>
</section>"#,
        version = html_escape(&format_terms_version(version)),
    )
}

/// The notice that the version `version` is announced and applies from its
/// date (#367). No acknowledgement: until that date the version the member
/// accepted is the one in force.
fn announced_notice(version: &str) -> String {
    format!(
        r#"<section class="notice">
<h2>Les conditions d'utilisation vont changer</h2>
<p>Une nouvelle version des conditions générales d'utilisation entrera en vigueur le {version}. D'ici là, la version que vous avez acceptée continue de s'appliquer. Continuer à utiliser le service après cette date vaut acceptation de la nouvelle version.</p>
<p><a href="{ANNOUNCED_TERMS_PAGE}">Lire la nouvelle version</a></p>
</section>"#,
        version = html_escape(&format_terms_version(version)),
    )
}

/// `GET /account/terms`. Only the session of an account without acceptance
/// gets the page; any other is sent where it belongs.
pub async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Response {
    let cookie = account_cookie(&headers);
    match fetch_session(&state, cookie.as_deref()).await {
        Session::TermsNotAccepted => {
            Html(shell(Width::Form, TITLE, &page(query.error.as_deref()))).into_response()
        }
        Session::Active(_) => Redirect::to("/").into_response(),
        Session::Deactivated => Redirect::to(DEACTIVATED_PAGE).into_response(),
        Session::AgeUndeclared => Redirect::to(AGE_DECLARATION_PAGE).into_response(),
        Session::None => Redirect::to("/login").into_response(),
    }
}

/// `POST /account/terms` — checks the box locally, then relays to apps/api's
/// `POST /auth/terms-acceptance`. 204 → `/`, now open; 422 → the page with
/// the registration's message; 401 (a session that may not accept) → `/`,
/// whose own extractor sorts it out; anything else → the unavailable
/// banner.
pub async fn post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<AcceptanceForm>,
) -> Response {
    let accepts_terms = form.accepts_terms.is_some();
    if let Err(code) = validate_terms_acceptance(accepts_terms) {
        return Redirect::to(&format!("{TERMS_ACCEPTANCE_PAGE}?error={code}")).into_response();
    }
    let cookie = account_cookie(&headers);
    let body = serde_json::to_value(TermsAcceptanceRequest { accepts_terms }).unwrap_or_default();
    match api_request_auth(
        &state,
        reqwest::Method::POST,
        "/auth/terms-acceptance",
        cookie.as_deref(),
        Some(body),
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            Redirect::to("/").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNPROCESSABLE_ENTITY => Redirect::to(
            &format!("{TERMS_ACCEPTANCE_PAGE}?error=terms_acceptance_required"),
        )
        .into_response(),
        Ok(resp) if resp.status == reqwest::StatusCode::UNAUTHORIZED => {
            Redirect::to("/").into_response()
        }
        Ok(_) => {
            Redirect::to(&format!("{TERMS_ACCEPTANCE_PAGE}?error=unavailable")).into_response()
        }
        Err(_) => service_unavailable_page().into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me(terms_accepted_version: Option<&str>) -> MeResponse {
        MeResponse {
            user_id: uuid::Uuid::nil(),
            email: "a@example.test".into(),
            display_name: "A".into(),
            email_verified: true,
            is_superadmin: false,
            has_password: true,
            deletion_requested_at: None,
            terms_accepted_version: terms_accepted_version.map(str::to_string),
        }
    }

    #[test]
    fn declining_gets_the_message() {
        assert!(error_message(Some("terms_acceptance_required"))
            .unwrap()
            .contains("cochez la case"));
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

    /// The box starts unticked — the acceptance is the holder's act — and
    /// the page links the text it asks them to accept, naming its version.
    #[test]
    fn the_page_asks_for_the_acceptance_unticked() {
        let html = page(None);
        assert!(html.contains(r#"name="accepts_terms""#), "{html}");
        assert!(!html.contains("checked"), "{html}");
        assert!(
            html.contains("J'accepte les conditions générales d'utilisation."),
            "{html}"
        );
        assert!(html.contains(r#"href="/terms-of-service""#), "{html}");
        assert!(html.contains(&format_terms_version(in_force())), "{html}");
        assert!(html.contains(r#"action="/logout""#), "{html}");
    }

    /// A member who accepted an earlier version is told, with a link to the
    /// text and the acknowledgement that posts to this page.
    #[test]
    fn an_earlier_version_accepted_gets_the_notice() {
        let html = update_notice(&me(Some("2000-01-01")));
        assert!(html.contains(r#"class="notice""#), "{html}");
        assert!(html.contains(&format_terms_version(in_force())), "{html}");
        assert!(html.contains(r#"href="/terms-of-service""#), "{html}");
        assert!(
            html.contains(&format!(r#"action="{TERMS_ACCEPTANCE_PAGE}""#)),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="hidden" name="accepts_terms" value="1"/>"#),
            "{html}"
        );
    }

    #[test]
    fn the_version_in_force_accepted_gets_no_notice() {
        assert_eq!(update_notice(&me(Some(in_force()))), "");
    }

    #[test]
    fn no_acceptance_on_file_gets_no_notice() {
        assert_eq!(update_notice(&me(None)), "");
    }

    /// This release's version in force today.
    fn in_force() -> &'static str {
        terms_in_force_on(paris_day(Utc::now()))
    }

    // -- notice_for (#367) ------------------------------------------------

    /// Before its date, a version announced is told about — its date and a
    /// link to its text — with nothing to acknowledge yet.
    #[test]
    fn before_its_date_an_announced_version_is_told_without_acknowledgement() {
        let html = notice_for(Some("2026-10-04"), "2026-10-04", Some("2026-12-01"));
        assert!(html.contains("vont changer"), "{html}");
        assert!(html.contains("entrera en vigueur le 01/12/2026"), "{html}");
        assert!(
            html.contains(&format!(r#"href="{ANNOUNCED_TERMS_PAGE}""#)),
            "{html}"
        );
        assert!(!html.contains("<form"), "{html}");
    }

    /// From its date, the same member is asked to acknowledge it.
    #[test]
    fn from_its_date_the_new_version_is_to_acknowledge() {
        let html = notice_for(Some("2026-10-04"), "2026-12-01", None);
        assert!(html.contains("ont changé"), "{html}");
        assert!(html.contains("01/12/2026"), "{html}");
        assert!(html.contains(r#"href="/terms-of-service""#), "{html}");
        assert!(
            html.contains(&format!(r#"action="{TERMS_ACCEPTANCE_PAGE}""#)),
            "{html}"
        );
    }

    /// A version in force not yet acknowledged comes first; the one
    /// announced after it waits for that acknowledgement.
    #[test]
    fn the_version_in_force_is_acknowledged_before_the_next_is_announced() {
        let html = notice_for(Some("2026-01-01"), "2026-10-04", Some("2026-12-01"));
        assert!(html.contains("ont changé"), "{html}");
        assert!(html.contains("04/10/2026"), "{html}");
        assert!(!html.contains("vont changer"), "{html}");
    }

    #[test]
    fn an_acceptance_covering_both_gets_no_notice() {
        assert_eq!(notice_for(Some("2026-10-04"), "2026-10-04", None), "");
        assert_eq!(
            notice_for(Some("2026-12-01"), "2026-10-04", Some("2026-12-01")),
            ""
        );
    }

    /// Rollback (#367): this binary holds an earlier version in force than
    /// the one the member accepted under a later release. Nothing to ask.
    #[test]
    fn a_later_version_accepted_gets_no_notice() {
        assert_eq!(notice_for(Some("2026-12-01"), "2026-10-04", None), "");
    }

    #[test]
    fn no_acceptance_on_file_gets_no_notice_even_with_an_announcement() {
        assert_eq!(notice_for(None, "2026-10-04", Some("2026-12-01")), "");
    }
}
