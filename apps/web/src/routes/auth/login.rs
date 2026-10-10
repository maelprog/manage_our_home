use axum::extract::{ConnectInfo, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::LoginRequest;
use std::net::SocketAddr;

use crate::app::{password_field, shell, Width};
use crate::layout::RedirectIfAuthenticated;
use crate::state::{api_post_json, AppState};

#[derive(serde::Deserialize)]
pub struct LoginForm {
    email: String,
    password: String,
}

/// `GET /login`'s query: `?reauth=admin` when an `/admin` page sent the
/// visitor here (#226).
#[derive(serde::Deserialize)]
pub struct LoginQuery {
    reauth: Option<String>,
}

/// The notice `?reauth=` puts above the form. Only the one value the
/// `/admin` pages send has copy; anything else a URL carries shows nothing.
fn reauth_notice(reason: Option<&str>) -> Option<&'static str> {
    match reason {
        Some("admin") => Some(
            "Votre session d'administration a expiré : reconnectez-vous pour accéder à l'administration.",
        ),
        _ => None,
    }
}

fn page(
    email: &str,
    error: Option<&str>,
    notice: Option<&str>,
    api_public_base_url: &str,
) -> String {
    let google_start = format!("{api_public_base_url}/auth/google/start");
    let pw = password_field("Mot de passe", "password", "current-password", false);
    let body = view! {
        <h1>"Se connecter"</h1>
        {notice.map(|n| view! { <p class="notice">{n.to_string()}</p> })}
        {error.map(|e| view! { <p class="notice error">{e.to_string()}</p> })}
        <form method="post" action="/login">
            <label>
                "Email"
                <input type="email" name="email" required=true value=email.to_string() />
            </label>
            <div inner_html=pw></div>
            <button type="submit">"Se connecter"</button>
        </form>
        // #419: stacked, so the Google button takes the width of the form's
        // own; the legal documents moved to the shell's footer.
        <div class="actions stacked">
            <a class="btn secondary" href=google_start>"Continuer avec Google"</a>
        </div>
        <div class="links centered">
            <a href="/register">"Créer un compte"</a>
            <a href="/forgot-password">"Mot de passe oublié ?"</a>
            // #420: an unverified account meets the generic 401 below, which
            // must not say why; the way out is offered to everyone.
            <a href="/verify-email/resend">"Email de vérification non reçu ?"</a>
        </div>
    };
    shell(Width::Form, "Connexion", &body.to_html())
}

pub async fn get(
    _redirect: RedirectIfAuthenticated,
    State(state): State<AppState>,
    Query(query): Query<LoginQuery>,
) -> impl IntoResponse {
    Html(page(
        "",
        None,
        reauth_notice(query.reauth.as_deref()),
        &state.api_public_base_url,
    ))
}

pub async fn post(
    _redirect: RedirectIfAuthenticated,
    State(state): State<AppState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    request_headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    // apps/api locks repeated failures per (client address, email) and has
    // no other way to learn that address: this call reaches it from inside
    // the Docker network (#178). `Option` because a server started without
    // `into_make_service_with_connect_info` has no peer to report — then
    // nothing is forwarded, which apps/api reads as "unknown client"
    // rather than as someone else's address.
    let forwarded = peer.map(|ConnectInfo(peer)| {
        crate::client_ip::forwarded_for(
            request_headers
                .get(crate::client_ip::FORWARDED_FOR)
                .and_then(|v| v.to_str().ok()),
            peer.ip(),
        )
    });

    let result = api_post_json(
        &state,
        "/auth/login",
        LoginRequest {
            email: form.email.clone(),
            password: form.password.clone(),
        },
        forwarded.as_deref(),
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
            None,
            &state.api_public_base_url,
        ))
        .into_response(),
        // Too many failures from this address on this account (#178). Said
        // plainly rather than as "une erreur est survenue": the person who
        // meets this is usually someone who mistyped their own password,
        // and telling them to wait is the only way they can act on it. It
        // reveals nothing — the lock answers the same whether or not the
        // account exists.
        Ok(resp) if resp.status == reqwest::StatusCode::TOO_MANY_REQUESTS => Html(page(
            &form.email,
            Some("Trop de tentatives de connexion. Merci de réessayer dans quelques minutes."),
            None,
            &state.api_public_base_url,
        ))
        .into_response(),
        Ok(_) => Html(page(
            &form.email,
            Some("Une erreur est survenue, merci de réessayer."),
            None,
            &state.api_public_base_url,
        ))
        .into_response(),
        Err(_) => Html(page(
            &form.email,
            Some("Service momentanément indisponible, merci de réessayer."),
            None,
            &state.api_public_base_url,
        ))
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_admin_reauth_reason_has_its_notice() {
        let notice = reauth_notice(Some("admin")).expect("a notice for the admin reason");
        assert!(notice.contains("administration"), "{notice}");
        assert!(notice.contains("reconnectez-vous"), "{notice}");
    }

    /// The query string is the visitor's to write: nothing else it says
    /// puts copy on the page.
    #[test]
    fn no_other_reason_has_a_notice() {
        for reason in [None, Some(""), Some("Admin"), Some("other"), Some("<b>")] {
            assert_eq!(reauth_notice(reason), None, "{reason:?}");
        }
    }

    /// #419: the legal documents are the shell's footer, no longer a line of
    /// the account links — once each, or Playwright's strict
    /// `getByRole("link", { name })` would find two.
    #[test]
    fn each_legal_document_is_linked_once_from_the_footer() {
        let html = page("", None, None, "http://api");
        for (href, _) in crate::app::LEGAL_DOCUMENTS {
            assert_eq!(
                html.matches(&format!(r#"href="{href}""#)).count(),
                1,
                "{href}: {html}"
            );
        }
        let footer = html.find("<footer").expect("a footer");
        assert!(html.find(r#"href="/privacy-policy""#).unwrap() > footer);
    }

    /// #419: the two account links are centred under the form, and the
    /// Google button takes the form button's width, in its own stacked row.
    #[test]
    fn the_account_links_are_centred_and_the_google_button_is_stacked() {
        let html = page("", None, None, "http://api");
        assert!(
            html.contains(r#"<div class="links centered"><a href="/register">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<div class="actions stacked"><a href="http://api/auth/google/start" class="btn secondary">"#),
            "{html}"
        );
    }

    /// #420: an unverified account cannot log in, and the login page says
    /// nothing about why (generic 401): the resend is offered to everyone,
    /// next to the forgotten password.
    #[test]
    fn the_login_page_links_to_the_verification_resend() {
        let html = page("", None, None, "http://api");
        assert!(
            html.contains(r#"<a href="/verify-email/resend">Email de vérification non reçu ?</a>"#),
            "{html}"
        );
    }

    #[test]
    fn the_login_page_shows_the_notice_it_is_given_and_none_otherwise() {
        let with = page("", None, Some("Session expirée."), "http://api");
        assert!(
            with.contains(r#"<p class="notice">Session expirée.</p>"#),
            "{with}"
        );
        let without = page("", None, None, "http://api");
        assert!(!without.contains(r#"class="notice""#), "{without}");
    }
}
