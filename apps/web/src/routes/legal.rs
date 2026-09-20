//! The public legal documents: `GET /legal-notice` (mentions légales, LCEN
//! art. 6-III) and `GET /terms-of-service` (CGU) — issue #132.
//!
//! Same three properties as `crate::routes::privacy`, and for the same
//! reasons: no session is required (both must be readable *before* an account
//! exists, and the CGU are what registering accepts), the text is not
//! duplicated here — `apps/api` serves the `docs/` markdown verbatim and this
//! module renders it with the TDD'd `validation::rgpd::render_markdown` — and
//! the renderer never lets raw HTML through.
//!
//! [`public_document`] is that shared page, and `routes::privacy` calls it
//! too: three routes, one rendering path. Anything that changes how a public
//! document is framed — the fallback when the API is down, the anonymous way
//! back to the login form, the reading width — changes for the three at once,
//! which is the drift that a third copy of this function would invite.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse};
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::validation::rgpd::render_markdown;

use crate::app::{shell, shell_with_header, Width};
use crate::layout::CurrentUserOpt;
use crate::routes::groups::header_with_groups;
use crate::state::{api_get_raw, AppState};

pub async fn legal_notice(
    CurrentUserOpt(me): CurrentUserOpt,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    public_document(me, &state, &headers, "/legal-notice", "Mentions légales").await
}

pub async fn terms_of_service(
    CurrentUserOpt(me): CurrentUserOpt,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    public_document(
        me,
        &state,
        &headers,
        "/terms-of-service",
        "Conditions générales d'utilisation",
    )
    .await
}

/// Renders the markdown `apps/api` serves at `path` as a standalone document
/// page titled `title`.
pub(crate) async fn public_document(
    me: Option<MeResponse>,
    state: &AppState,
    headers: &HeaderMap,
    path: &str,
    title: &str,
) -> Html<String> {
    // An authenticated visitor keeps the app chrome — the sidebar, as its own
    // grid column. An anonymous one has no navigation to render at all, so the
    // page is a bare document with a way back to the login form in it: the
    // sidebar column would otherwise be a 15rem strip holding one link.
    let header = match &me {
        Some(me) => Some(header_with_groups(state, headers, me, path).await.1),
        None => None,
    };

    let document = match api_get_raw(state, path, None).await {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            render_markdown(&String::from_utf8_lossy(&resp.body))
        }
        _ => format!(
            r#"<h1>{title}</h1>
<p class="notice error">Le document est momentanément indisponible, merci de réessayer dans quelques instants.</p>"#
        ),
    };
    let article = format!(r#"<article class="prose">{document}</article>"#);

    // `--w-read`, not the 28rem the whole app used to get: these are the pages
    // made of long-form text, and 448px is *too narrow* for them.
    Html(match header {
        Some(header) => shell_with_header(Width::Read, title, &header, &article),
        None => shell(
            Width::Read,
            title,
            &format!(
                r#"<p class="links"><a href="/login">← Retour à la connexion</a></p>{article}"#
            ),
        ),
    })
}
