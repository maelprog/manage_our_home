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

use crate::app::{shell, shell_with_header, Width};
use crate::layout::CurrentUserOpt;
use crate::routes::groups::header_with_groups;
use crate::state::{api_get_raw, AppState};

pub async fn get(
    CurrentUserOpt(me): CurrentUserOpt,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // An authenticated visitor keeps the app chrome — the sidebar, as its own
    // grid column. An anonymous one has no navigation to render at all, so the
    // page is a bare document with a way back to the login form in it: the
    // sidebar column would otherwise be a 15rem strip holding one link.
    let header = match &me {
        Some(me) => Some(
            header_with_groups(&state, &headers, me, "/privacy-policy")
                .await
                .1,
        ),
        None => None,
    };

    let policy = match api_get_raw(&state, "/privacy-policy", None).await {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            render_markdown(&String::from_utf8_lossy(&resp.body))
        }
        _ => r#"<h1>Politique de confidentialité</h1>
<p class="notice error">Le document est momentanément indisponible, merci de réessayer dans quelques instants.</p>"#
            .to_string(),
    };
    let article = format!(r#"<article class="prose">{policy}</article>"#);

    // `--w-read`, not the 28rem the whole app used to get: this is the one
    // page made of long-form text, and 448px is *too narrow* for it.
    Html(match header {
        Some(header) => shell_with_header(
            Width::Read,
            "Politique de confidentialité",
            &header,
            &article,
        ),
        None => shell(
            Width::Read,
            "Politique de confidentialité",
            &format!(
                r#"<p class="links"><a href="/login">← Retour à la connexion</a></p>{article}"#
            ),
        ),
    })
}
