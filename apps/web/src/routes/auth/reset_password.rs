use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::Form;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::ResetPasswordRequest;
use manage_our_home_shared::validation::auth::{is_bearer_token, validate_password};

use crate::app::{
    password_confirmation_error, password_confirmation_field, password_error_message,
    password_field, shell, Width,
};
use crate::assets::Script;
use crate::state::{api_post_json, AppState};

#[derive(serde::Deserialize)]
pub struct ResetPasswordForm {
    token: String,
    new_password: String,
    /// The second entry of the new password (#461). Defaulted, so a form
    /// sent from a page served before it existed comes back with the
    /// confirmation error rather than a bare 422.
    #[serde(default)]
    new_password_confirm: String,
}

/// Moves the token from the link's fragment (`#token=<token>`, 43
/// base64url characters since #335, see
/// `password_reset_link` in apps/api) into the form's hidden field, then
/// drops the fragment from the current history entry (#142). The fragment
/// is never sent to the server, which is the point: the secret stays out of
/// access logs and `Referer`, and reaches us only in the POST body. A field
/// that already holds a token (re-render after a refused POST) is left as is;
/// a page with neither a fragment nor a token swaps the form for the
/// invalid-link notice. `remove()` rather than `hidden`, because
/// `form { display: flex }` in the sheet outranks the `hidden` attribute.
///
/// Served as a file under `/assets` (`assets::Script::ResetPassword`, #325):
/// infra/Caddyfile's CSP runs no inline script.
pub(crate) const FRAGMENT_SCRIPT: &str = r#"
(function () {
  var field = document.getElementById("reset-token");
  if (!field) return;
  var m = /^#token=([A-Za-z0-9_-]{43})$/.exec(location.hash);
  if (location.hash) history.replaceState(null, "", location.pathname);
  if (m) field.value = m[1];
  if (!field.value) {
    document.getElementById("reset-form").remove();
    document.getElementById("reset-missing").hidden = false;
  }
})();
"#;

fn invalid_link_page(title: &str, message: &str) -> String {
    let title_owned = title.to_string();
    let message_owned = message.to_string();
    let body = view! {
        <h1>{title_owned}</h1>
        <p>{message_owned}</p>
        <a class="btn secondary" href="/forgot-password">"Redemander un email"</a>
    };
    shell(Width::Form, title, &body.to_html())
}

/// `token` is `None` on the landing GET — the token is in the fragment,
/// which the server never sees — and `Some` when re-rendering after a
/// refused POST, which carried it in its body. `confirm_error` is the
/// confirmation's own (#461), shown on its field rather than as a banner.
fn form_page(token: Option<&str>, error: Option<&str>, confirm_error: Option<&str>) -> String {
    let token_owned = token.unwrap_or_default().to_string();
    let error_owned = error.map(str::to_string);
    let pw = password_field("Nouveau mot de passe", "new_password", "new-password", true);
    let confirm = password_confirmation_field("new_password_confirm", confirm_error);
    let body = view! {
        <h1>"Réinitialiser le mot de passe"</h1>
        {error_owned.map(|e| view! { <p class="notice error">{e}</p> })}
        <noscript>
            <p class="notice error">
                "Cette page a besoin de JavaScript pour lire le lien reçu par email."
            </p>
        </noscript>
        <div id="reset-missing" hidden=true>
            <p>"Ce lien de réinitialisation n'existe pas."</p>
            <a class="btn secondary" href="/forgot-password">"Redemander un email"</a>
        </div>
        <form id="reset-form" method="post" action="/reset-password">
            <input type="hidden" id="reset-token" name="token" value=token_owned />
            <div inner_html=pw></div>
            <div inner_html=confirm></div>
            <button type="submit">"Réinitialiser"</button>
        </form>
    };
    let mut html = body.to_html();
    html.push_str(&Script::ResetPassword.tag());
    shell(Width::Form, "Réinitialiser le mot de passe", &html)
}

/// The landing page of the emailed link. It reads nothing from the request:
/// the token arrives in the fragment and `FRAGMENT_SCRIPT` picks it up.
pub async fn get() -> impl IntoResponse {
    Html(form_page(None, None, None))
}

pub async fn post(
    State(state): State<AppState>,
    Form(form): Form<ResetPasswordForm>,
) -> impl IntoResponse {
    if !is_bearer_token(&form.token) {
        return Html(invalid_link_page(
            "Lien invalide",
            "Ce lien de réinitialisation n'existe pas.",
        ));
    }
    if let Err(code) = validate_password(&form.new_password) {
        return Html(form_page(
            Some(&form.token),
            Some(&password_error_message(code)),
            None,
        ));
    }
    // #461: checked before apps/api is called, so the token stays unused
    // and the form, which keeps it, can be sent again.
    if let Some(mismatch) =
        password_confirmation_error(&form.new_password, &form.new_password_confirm)
    {
        return Html(form_page(Some(&form.token), None, Some(mismatch)));
    }

    let result = api_post_json(
        &state,
        "/auth/password/reset",
        ResetPasswordRequest {
            token: form.token.clone(),
            new_password: form.new_password,
        },
        None,
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            let body = view! {
                <h1>"Mot de passe mis à jour"</h1>
                <p>"Votre mot de passe a été réinitialisé. Toutes vos autres sessions ont été déconnectées."</p>
                <a class="btn" href="/login">"Se connecter"</a>
            };
            Html(shell(
                Width::Form,
                "Mot de passe mis à jour",
                &body.to_html(),
            ))
        }
        // 410 no longer covers a used token: using one deletes it (#138), so
        // the only way here is an expiry — one hour after the email was sent.
        Ok(resp) if resp.status == reqwest::StatusCode::GONE => Html(invalid_link_page(
            "Lien expiré",
            "Ce lien de réinitialisation a expiré. Un lien n'est valable qu'une heure.",
        )),
        // A used token is deleted (#138): a second use lands here, not on 410.
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => Html(invalid_link_page(
            "Lien invalide",
            "Ce lien de réinitialisation n'existe pas ou a déjà été utilisé.",
        )),
        _ => Html(form_page(
            Some(&form.token),
            Some("Service momentanément indisponible, merci de réessayer."),
            None,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) const TOKEN: &str = "Zq3_k9XvT2mQ8pLw4rYb7nHc1sDf6gJ0aEuIoVtBy5M";

    /// The `<input>` tag carrying `id="reset-token"`.
    fn token_field(html: &str) -> &str {
        let at = html.find(r#"id="reset-token""#).expect("token field");
        let start = html[..at].rfind('<').unwrap();
        let end = at + html[at..].find('>').unwrap();
        &html[start..=end]
    }

    /// The link lands with the token in the fragment, which the browser never
    /// sends: the server-rendered page cannot know it and must not carry one.
    #[test]
    fn landing_page_renders_an_empty_token_field() {
        let html = form_page(None, None, None);
        let field = token_field(&html);
        assert!(field.contains(r#"name="token""#), "{field}");
        assert!(
            !field.contains("value=") || field.contains(r#"value="""#),
            "{field}"
        );
    }

    /// The script is what moves the token from the fragment into the POST
    /// body and scrubs the fragment from the session history entry; the page
    /// loads it from `/assets` (#325).
    #[test]
    fn landing_page_reads_the_fragment_and_scrubs_it() {
        let html = form_page(None, None, None);
        assert!(html.contains(&Script::ResetPassword.tag()), "{html}");
        assert!(FRAGMENT_SCRIPT.contains("location.hash"));
        assert!(FRAGMENT_SCRIPT.contains("history.replaceState"));
        assert!(html.contains("<noscript>"), "{html}");
    }

    /// A re-render after a refused POST keeps the token it received in the
    /// body, so the person can retry without going back to the email.
    #[test]
    fn rerender_after_error_keeps_the_posted_token() {
        let html = form_page(Some(TOKEN), Some("Mot de passe trop court"), None);
        assert!(
            token_field(&html).contains(&format!(r#"value="{TOKEN}""#)),
            "{html}"
        );
        assert!(html.contains("Mot de passe trop court"), "{html}");
    }

    /// The `<input>` tag carrying `name="{name}"`.
    fn input_named<'a>(html: &'a str, name: &str) -> &'a str {
        let at = html
            .find(&format!(r#"name="{name}""#))
            .expect("named input");
        let start = html[..at].rfind('<').unwrap();
        let end = at + html[at..].find('>').unwrap();
        &html[start..=end]
    }

    /// #461: the new password is asked twice; only the first entry gets the
    /// strength gauge.
    #[test]
    fn the_form_asks_for_the_new_password_twice() {
        let html = form_page(None, None, None);
        assert!(
            input_named(&html, "new_password").contains("data-pw-meter"),
            "{html}"
        );
        let confirm = input_named(&html, "new_password_confirm");
        assert!(confirm.contains(r#"type="password""#), "{confirm}");
        assert!(!confirm.contains("data-pw-meter"), "{confirm}");
        assert!(!confirm.contains("aria-invalid"), "{confirm}");
    }

    /// #461: a second entry that differs comes back on its own field, the
    /// token kept for the next try.
    #[test]
    fn a_confirmation_that_differs_is_described_on_its_field() {
        let mismatch = password_confirmation_error("a", "b");
        let html = form_page(Some(TOKEN), None, mismatch);
        let confirm = input_named(&html, "new_password_confirm");
        assert!(
            confirm
                .contains(r#"aria-invalid="true" aria-describedby="new_password_confirm-error""#),
            "{confirm}"
        );
        assert!(
            html.contains(
                r#"id="new_password_confirm-error">Les deux mots de passe ne correspondent pas.<"#
            ),
            "{html}"
        );
        assert!(!html.contains("notice error\">Les deux"), "{html}");
        assert!(
            token_field(&html).contains(&format!(r#"value="{TOKEN}""#)),
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tower::ServiceExt;

    use crate::state::AppState;

    /// #461: two entries that differ never reach apps/api, so the token is
    /// not spent: the API deletes a token on its use (#138).
    #[tokio::test]
    async fn a_confirmation_that_differs_leaves_the_token_unused() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let app = Router::new().route(
            "/auth/password/reset",
            post(move || async move {
                counted.fetch_add(1, Ordering::SeqCst);
                StatusCode::OK
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let router = crate::build_router(AppState {
            http: reqwest::Client::new(),
            api_internal_base_url: format!("http://{addr}"),
            api_public_base_url: "/api".into(),
            body_read_limits: manage_our_home_http_guard::BodyReadLimits::PRODUCTION,
            upload_gate: manage_our_home_http_guard::UploadGate::production(),
            shutdown: manage_our_home_http_guard::Shutdown::new(),
        });
        let send = |confirm: &str| {
            Request::builder()
                .method(Method::POST)
                .uri("/reset-password")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "token={}&new_password=correct+horse+battery+staple\
                     &new_password_confirm={confirm}",
                    super::tests::TOKEN
                )))
                .unwrap()
        };

        let response = router
            .clone()
            .oneshot(send("correct+horse+battery+stapel"))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains("Les deux mots de passe ne correspondent pas."),
            "{body}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // The same form, both entries alike, goes through.
        let response = router
            .oneshot(send("correct+horse+battery+staple"))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("Mot de passe mis à jour"), "{body}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
