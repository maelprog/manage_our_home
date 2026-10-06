//! The public legal documents: `GET /legal-notice` (mentions légales, LCEN
//! art. 1-1) and `GET /terms-of-service` (CGU) — issue #132.
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
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::Utc;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::validation::auth::{
    format_terms_version, paris_day, terms_announced_on,
};
use manage_our_home_shared::validation::rgpd::render_markdown;

use crate::app::{html_escape, shell, shell_with_header, Width};
use crate::layout::CurrentUserOpt;
use crate::routes::groups::header_with_groups;
use crate::state::{api_get_raw, AppState};

/// Where the CGU version announced and not yet in force is read (#367).
pub const ANNOUNCED_TERMS_PAGE: &str = "/terms-of-service/announced";

const TERMS_TITLE: &str = "Conditions générales d'utilisation";

pub async fn legal_notice(
    CurrentUserOpt(me): CurrentUserOpt,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    public_document(
        me,
        &state,
        &headers,
        "/legal-notice",
        "Mentions légales",
        "",
    )
    .await
}

/// `GET /terms-of-service` — the CGU in force today, which `apps/api` picks
/// (#367). While a version is announced, a notice above the text gives its
/// date and links it. The day is read on every request and the page is
/// `no-cache`: the text behind this URL changes on a date, not on a deploy,
/// and no cached copy may outlive that date.
pub async fn terms_of_service(
    CurrentUserOpt(me): CurrentUserOpt,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let notice = terms_announced_on(paris_day(Utc::now()))
        .map(|announced| announcement_notice(announced.version))
        .unwrap_or_default();
    let page = public_document(
        me,
        &state,
        &headers,
        "/terms-of-service",
        TERMS_TITLE,
        &notice,
    )
    .await;
    not_cached(page.into_response())
}

/// `GET /terms-of-service/announced` — the version announced, under a notice
/// that it is not in force yet and which text is (#367). Nothing announced,
/// or its date come, sends to `/terms-of-service`. `no-cache` too.
pub async fn announced_terms_of_service(
    CurrentUserOpt(me): CurrentUserOpt,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(announced) = terms_announced_on(paris_day(Utc::now())) else {
        return not_cached(Redirect::to("/terms-of-service").into_response());
    };
    let page = public_document(
        me,
        &state,
        &headers,
        ANNOUNCED_TERMS_PAGE,
        TERMS_TITLE,
        &announced_notice(announced.version),
    )
    .await;
    not_cached(page.into_response())
}

/// Above the text in force: a version is announced, from its date (#367).
fn announcement_notice(version: &str) -> String {
    format!(
        r#"<p class="notice">Une nouvelle version de ces conditions entrera en vigueur le {date} : <a href="{ANNOUNCED_TERMS_PAGE}">lire la nouvelle version</a>. Jusqu'à cette date, la version ci-dessous s'applique.</p>"#,
        date = html_escape(&format_terms_version(version)),
    )
}

/// Above the announced text: not in force before its date, and where the
/// text in force is (#367).
fn announced_notice(version: &str) -> String {
    format!(
        r#"<p class="notice">Version annoncée : elle entrera en vigueur le {date}. Jusqu'à cette date, c'est <a href="/terms-of-service">la version en vigueur</a> qui s'applique.</p>"#,
        date = html_escape(&format_terms_version(version)),
    )
}

/// A CGU page a cache must revalidate before reusing it (#367).
fn not_cached(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

/// Renders the markdown `apps/api` serves at `path` as a standalone document
/// page titled `title`, with `notice` — trusted HTML, empty for none — above
/// the document.
pub(crate) async fn public_document(
    me: Option<MeResponse>,
    state: &AppState,
    headers: &HeaderMap,
    path: &str,
    title: &str,
    notice: &str,
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
    let article = format!(r#"{notice}<article class="prose">{document}</article>"#);

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

#[cfg(test)]
mod tests {
    use super::*;

    /// The text in force names the version announced, its date in French,
    /// and links it — and says the text below applies until then.
    #[test]
    fn the_text_in_force_announces_the_next_version() {
        let html = announcement_notice("2026-12-01");
        assert!(html.starts_with(r#"<p class="notice">"#), "{html}");
        assert!(html.contains("01/12/2026"), "{html}");
        assert!(
            html.contains(&format!(r#"href="{ANNOUNCED_TERMS_PAGE}""#)),
            "{html}"
        );
        assert!(html.contains("ci-dessous s'applique"), "{html}");
    }

    /// The announced text says it is not in force yet, from when it will
    /// be, and where the text in force is.
    #[test]
    fn the_announced_text_says_it_is_not_in_force_yet() {
        let html = announced_notice("2026-12-01");
        assert!(html.starts_with(r#"<p class="notice">"#), "{html}");
        assert!(html.contains("entrera en vigueur le 01/12/2026"), "{html}");
        assert!(html.contains(r#"href="/terms-of-service""#), "{html}");
    }

    /// A version is a constant of the release, but it is text put in HTML:
    /// escaped like any other.
    #[test]
    fn a_version_is_escaped() {
        assert!(!announcement_notice("<b>").contains("<b>"));
        assert!(!announced_notice("<b>").contains("<b>"));
    }

    #[test]
    fn a_cgu_page_is_never_reused_unchecked() {
        let response = not_cached(Html("x").into_response());
        assert_eq!(response.headers().get(CACHE_CONTROL).unwrap(), "no-cache");
    }
}
