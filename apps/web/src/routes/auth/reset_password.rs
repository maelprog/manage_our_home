use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::Form;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::ResetPasswordRequest;
use manage_our_home_shared::validation::auth::validate_password;
use uuid::Uuid;

use crate::app::{password_error_message, password_field, shell, Width};
use crate::state::{api_post_json, AppState};

#[derive(serde::Deserialize)]
pub struct ResetPasswordForm {
    token: String,
    new_password: String,
}

/// Moves the token from the link's fragment (`#token=<uuid>`, see
/// `password_reset_link` in apps/api) into the form's hidden field, then
/// drops the fragment from the current history entry (#142). The fragment
/// is never sent to the server, which is the point: the secret stays out of
/// access logs and `Referer`, and reaches us only in the POST body. A field
/// that already holds a token (re-render after a refused POST) is left as is;
/// a page with neither a fragment nor a token swaps the form for the
/// invalid-link notice. `remove()` rather than `hidden`, because
/// `form { display: flex }` in the sheet outranks the `hidden` attribute.
const FRAGMENT_SCRIPT: &str = r#"<script>
(function () {
  var field = document.getElementById("reset-token");
  if (!field) return;
  var m = /^#token=([0-9A-Fa-f-]{36})$/.exec(location.hash);
  if (location.hash) history.replaceState(null, "", location.pathname);
  if (m) field.value = m[1];
  if (!field.value) {
    document.getElementById("reset-form").remove();
    document.getElementById("reset-missing").hidden = false;
  }
})();
</script>"#;

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
/// refused POST, which carried it in its body.
fn form_page(token: Option<&str>, error: Option<&str>) -> String {
    let token_owned = token.unwrap_or_default().to_string();
    let error_owned = error.map(str::to_string);
    let pw = password_field("Nouveau mot de passe", "new_password", "new-password", true);
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
            <button type="submit">"Réinitialiser"</button>
        </form>
    };
    let mut html = body.to_html();
    html.push_str(FRAGMENT_SCRIPT);
    shell(Width::Form, "Réinitialiser le mot de passe", &html)
}

/// The landing page of the emailed link. It reads nothing from the request:
/// the token arrives in the fragment and `FRAGMENT_SCRIPT` picks it up.
pub async fn get() -> impl IntoResponse {
    Html(form_page(None, None))
}

pub async fn post(
    State(state): State<AppState>,
    Form(form): Form<ResetPasswordForm>,
) -> impl IntoResponse {
    let Ok(token) = Uuid::parse_str(&form.token) else {
        return Html(invalid_link_page(
            "Lien invalide",
            "Ce lien de réinitialisation n'existe pas.",
        ));
    };
    if let Err(code) = validate_password(&form.new_password) {
        return Html(form_page(
            Some(&form.token),
            Some(&password_error_message(code)),
        ));
    }

    let result = api_post_json(
        &state,
        "/auth/password/reset",
        ResetPasswordRequest {
            token,
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
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "5f0c7a3e-2b1d-4c8e-9a6f-0d3b2e1c4a5b";

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
        let html = form_page(None, None);
        let field = token_field(&html);
        assert!(field.contains(r#"name="token""#), "{field}");
        assert!(
            !field.contains("value=") || field.contains(r#"value="""#),
            "{field}"
        );
    }

    /// The script is what moves the token from the fragment into the POST
    /// body and scrubs the fragment from the session history entry.
    #[test]
    fn landing_page_reads_the_fragment_and_scrubs_it() {
        let html = form_page(None, None);
        assert!(html.contains("location.hash"), "{html}");
        assert!(html.contains("history.replaceState"), "{html}");
        assert!(html.contains("<noscript>"), "{html}");
    }

    /// A re-render after a refused POST keeps the token it received in the
    /// body, so the person can retry without going back to the email.
    #[test]
    fn rerender_after_error_keeps_the_posted_token() {
        let html = form_page(Some(TOKEN), Some("Mot de passe trop court"));
        assert!(
            token_field(&html).contains(&format!(r#"value="{TOKEN}""#)),
            "{html}"
        );
        assert!(html.contains("Mot de passe trop court"), "{html}");
    }
}
