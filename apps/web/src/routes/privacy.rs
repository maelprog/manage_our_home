//! `GET /privacy-policy` — the public privacy policy (front epic F10, issue
//! #25). Deliberately **not** under `routes::account`: a prospective user must be
//! able to read it before registering, so this is the only page besides the auth
//! entry points that renders without a session (linked from the login and
//! register footers).
//!
//! The content is not duplicated here: `apps/api` serves `docs/privacy-policy.md`
//! verbatim as `text/markdown` (compiled in via `include_str!`), and this page
//! renders that markdown with the TDD'd
//! `validation::rgpd::render_markdown` — so the deployed policy, the document in
//! source control and the API response can never drift, and the renderer never
//! lets raw HTML through.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse};
use manage_our_home_shared::validation::rgpd::render_markdown;

use crate::app::shell;
use crate::layout::CurrentUserOpt;
use crate::routes::groups::header_with_groups;
use crate::state::{api_get_raw, AppState};

pub async fn get(
    CurrentUserOpt(me): CurrentUserOpt,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // An authenticated visitor keeps the app chrome; an anonymous one gets a way
    // back to the login page (they have no header to return to).
    let header = match &me {
        Some(me) => {
            header_with_groups(&state, &headers, me, "/privacy-policy")
                .await
                .1
        }
        None => r#"<p class="links"><a href="/login">← Retour à la connexion</a></p>"#.to_string(),
    };

    let policy = match api_get_raw(&state, "/privacy-policy", None).await {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            render_markdown(&String::from_utf8_lossy(&resp.body))
        }
        _ => r#"<h1>Politique de confidentialité</h1>
<p class="notice error">Le document est momentanément indisponible, merci de réessayer dans quelques instants.</p>"#
            .to_string(),
    };

    Html(shell(
        "Politique de confidentialité",
        &format!(r#"{header}<article class="prose">{policy}</article>"#),
    ))
}
