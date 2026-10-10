//! `/verify-email/resend` (#420): the form that asks apps/api for a new
//! verification email (`POST /auth/verify-email/resend`). An account whose
//! email never arrived could not log in (the generic 401 of an unverified
//! account) nor register again (409), and had no other way out.

use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::Form;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::ResendVerificationRequest;
use manage_our_home_shared::validation::auth::VERIFICATION_RESEND_COOLDOWN_SECS;

use crate::app::{shell, Width};
use crate::state::{api_post_json, AppState};

#[derive(serde::Deserialize)]
pub struct ResendVerificationForm {
    email: String,
}

/// What the visitor is told once the form is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// apps/api answered: the one message, whether the account exists, is
    /// verified, is waiting, or was sent an email within the cooldown — it
    /// answers 200 to all of them and so does this page (anti-enumeration,
    /// same reasoning as `/forgot-password`).
    Sent,
    /// No answer, or a 5xx: the service is down, not the request wrong.
    Unavailable,
    /// Any other answer: nothing was sent, and nothing more is known.
    Failed,
}

fn outcome(status: Option<reqwest::StatusCode>) -> Outcome {
    match status {
        Some(status) if status.is_success() => Outcome::Sent,
        Some(status) if !status.is_server_error() => Outcome::Failed,
        _ => Outcome::Unavailable,
    }
}

const SENT: &str = "Si un compte en attente de vérification existe pour cette adresse, \
                    un nouvel email de vérification vient de lui être envoyé.";
const UNAVAILABLE: &str = "Service momentanément indisponible, merci de réessayer.";
const FAILED: &str = "Une erreur est survenue, merci de réessayer.";

/// The form, here and on `/register/check-email`. `countdown` when an email
/// has just left: the button then carries the cooldown apps/api applies,
/// which `enhance.js` counts down on it before enabling it. Without
/// JavaScript the button stays usable, and a send within the cooldown is a
/// silent no-op on the server. The label changes once a second, so it sits
/// in no live region: a screen reader reads it when it reaches the button,
/// and is not interrupted thirty times.
pub fn resend_form(countdown: bool) -> String {
    let cooldown = countdown.then(|| VERIFICATION_RESEND_COOLDOWN_SECS.to_string());
    view! {
        <form method="post" action="/verify-email/resend">
            <label>
                "Email"
                <input type="email" name="email" required=true autocomplete="email" />
            </label>
            <button type="submit" data-resend-cooldown=cooldown>
                "Renvoyer l'email de vérification"
            </button>
        </form>
    }
    .to_html()
}

/// The page, blank (`None`) or after a send. The address typed is not
/// echoed back: the page after a send is the very same for every address.
fn page(outcome: Option<Outcome>) -> String {
    let notice = match outcome {
        None => None,
        Some(Outcome::Sent) => Some(("notice success", SENT)),
        Some(Outcome::Unavailable) => Some(("notice error", UNAVAILABLE)),
        Some(Outcome::Failed) => Some(("notice error", FAILED)),
    };
    let head = view! {
        <h1>"Email de vérification non reçu"</h1>
        {notice.map(|(class, text)| view! { <p class=class>{text}</p> })}
        <p>"Saisissez l'adresse de votre compte. S'il attend encore sa vérification, un nouveau lien lui est envoyé, et les précédents ne servent plus."</p>
    };
    let links = view! {
        <div class="links">
            <a href="/login">"Retour à la connexion"</a>
        </div>
    };
    let body = format!(
        "{}{}{}",
        head.to_html(),
        resend_form(outcome == Some(Outcome::Sent)),
        links.to_html()
    );
    shell(Width::Form, "Email de vérification non reçu", &body)
}

pub async fn get() -> impl IntoResponse {
    Html(page(None))
}

pub async fn post(
    State(state): State<AppState>,
    Form(form): Form<ResendVerificationForm>,
) -> impl IntoResponse {
    let result = api_post_json(
        &state,
        "/auth/verify-email/resend",
        ResendVerificationRequest { email: form.email },
        None,
    )
    .await;
    Html(page(Some(outcome(result.ok().map(|r| r.status)))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use manage_our_home_shared::validation::auth::VERIFICATION_RESEND_COOLDOWN_SECS;
    use reqwest::StatusCode;

    const SENT: &str = "Si un compte en attente de vérification existe pour cette adresse, \
                        un nouvel email de vérification vient de lui être envoyé.";
    const UNAVAILABLE: &str = "Service momentanément indisponible, merci de réessayer.";

    #[test]
    fn a_success_from_apps_api_is_sent() {
        for status in [StatusCode::OK, StatusCode::NO_CONTENT] {
            assert_eq!(outcome(Some(status)), Outcome::Sent, "{status}");
        }
    }

    /// The issue's error table: a transport error or a 5xx is the service
    /// being down, said as such and not as a success.
    #[test]
    fn no_answer_or_a_server_error_is_unavailable() {
        assert_eq!(outcome(None), Outcome::Unavailable);
        for status in [
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert_eq!(outcome(Some(status)), Outcome::Unavailable, "{status}");
        }
    }

    #[test]
    fn any_other_answer_failed() {
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNPROCESSABLE_ENTITY,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            assert_eq!(outcome(Some(status)), Outcome::Failed, "{status}");
        }
    }

    /// The form posts an email to the web route that calls apps/api.
    #[test]
    fn the_form_posts_an_email_to_the_resend_route() {
        let form = resend_form(false);
        assert!(
            form.contains(r#"<form method="post" action="/verify-email/resend">"#),
            "{form}"
        );
        assert!(form.contains(r#"type="email""#), "{form}");
        assert!(form.contains(r#"name="email""#), "{form}");
        assert!(
            form.contains("Renvoyer l'email de vérification</button>"),
            "{form}"
        );
    }

    /// The countdown is `enhance.js`'s, and it reads its length from the
    /// button: the value is the shared constant, never a second copy.
    #[test]
    fn only_a_form_after_a_sending_carries_the_countdown() {
        let marker = format!(r#"data-resend-cooldown="{VERIFICATION_RESEND_COOLDOWN_SECS}""#);
        assert!(resend_form(true).contains(&marker));
        assert!(!resend_form(false).contains("data-resend-cooldown"));
        // A countdown read out every second is noise: no live region.
        assert!(!resend_form(true).contains("aria-live"));
    }

    #[test]
    fn the_blank_form_starts_without_a_countdown_or_a_message() {
        let html = page(None);
        assert!(html.contains(&resend_form(false)), "{html}");
        assert!(
            !html.contains(SENT) && !html.contains(UNAVAILABLE),
            "{html}"
        );
        assert!(html.contains(r#"href="/login""#), "{html}");
    }

    /// Anti-enumeration: one message, whatever apps/api did with the email,
    /// and the countdown starts again since an email may have left.
    #[test]
    fn a_sent_form_shows_the_neutral_message_and_the_countdown() {
        let html = page(Some(Outcome::Sent));
        assert!(html.contains(SENT), "{html}");
        assert!(html.contains(r#"class="notice success""#), "{html}");
        assert!(html.contains(&resend_form(true)), "{html}");
        assert!(!html.contains(UNAVAILABLE), "{html}");
    }

    #[test]
    fn an_unavailable_service_is_not_a_success() {
        let html = page(Some(Outcome::Unavailable));
        assert!(html.contains(UNAVAILABLE), "{html}");
        assert!(html.contains(r#"class="notice error""#), "{html}");
        assert!(!html.contains(SENT), "{html}");
        // Nothing left: no countdown to wait out.
        assert!(html.contains(&resend_form(false)), "{html}");
    }

    #[test]
    fn a_failure_is_neither_a_success_nor_an_outage() {
        let html = page(Some(Outcome::Failed));
        assert!(
            html.contains("Une erreur est survenue, merci de réessayer."),
            "{html}"
        );
        assert!(
            !html.contains(SENT) && !html.contains(UNAVAILABLE),
            "{html}"
        );
    }
}

/// The route on the real router, against a stand-in apps/api.
#[cfg(test)]
mod route_tests {
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Method, Request, StatusCode};
    use axum::routing::post;
    use axum::Router;
    use tower::ServiceExt;

    use crate::state::AppState;

    /// An apps/api whose resend answers `status`, as the real one answers
    /// 200 for an unknown, a verified and an unverified email alike.
    async fn fake_api(status: StatusCode) -> String {
        let app = Router::new().route(
            "/auth/verify-email/resend",
            post(move || async move { status }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    /// An address nothing listens on.
    async fn no_api() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{addr}")
    }

    async fn post_resend(api: String, email: &str) -> (StatusCode, String) {
        let router = crate::build_router(AppState {
            http: reqwest::Client::new(),
            api_internal_base_url: api,
            api_public_base_url: "/api".into(),
            body_read_limits: manage_our_home_http_guard::BodyReadLimits::PRODUCTION,
            upload_gate: manage_our_home_http_guard::UploadGate::production(),
            shutdown: manage_our_home_http_guard::Shutdown::new(),
        });
        let request = Request::builder()
            .method(Method::POST)
            .uri("/verify-email/resend")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(format!("email={}", email.replace('@', "%40"))))
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    /// No enumeration oracle on the web side: the same code and the very
    /// same body for an unknown, a verified and an unverified address — the
    /// email typed is not even echoed back.
    #[tokio::test]
    async fn every_address_gets_the_same_page() {
        let api = fake_api(StatusCode::OK).await;
        let mut pages = Vec::new();
        for email in [
            "inconnu@example.test",
            "verifie@example.test",
            "en-attente@example.test",
        ] {
            let (status, body) = post_resend(api.clone(), email).await;
            assert_eq!(status, StatusCode::OK);
            assert!(!body.contains(email), "{body}");
            pages.push(body);
        }
        assert!(pages.windows(2).all(|w| w[0] == w[1]));
        assert!(pages[0].contains("un nouvel email de vérification"));
    }

    #[tokio::test]
    async fn an_api_down_or_failing_says_the_service_is_unavailable() {
        for api in [
            no_api().await,
            fake_api(StatusCode::INTERNAL_SERVER_ERROR).await,
        ] {
            let (_, body) = post_resend(api, "a@example.test").await;
            assert!(
                body.contains("Service momentanément indisponible"),
                "{body}"
            );
            assert!(!body.contains("un nouvel email de vérification"), "{body}");
        }
    }
}
