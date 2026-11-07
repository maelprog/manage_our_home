//! Telling a request from the Android app apart from a browser's (#665).
//!
//! The app (`apps/mobile`) is a Capacitor shell whose WebView loads this
//! site from its real origin: it sends the same requests as Chrome on the
//! same phone, cookies included. The one thing that tells them apart is
//! the token its WebView appends to its User-Agent, `USER_AGENT_MARKER`.
//! `apps/mobile/capacitor.config.ts` reads that constant out of this file
//! (`appendUserAgent`), so the two cannot drift apart: rename it, or give
//! it a value its reader does not recognise, and `npx cap sync` fails.
//!
//! [`scope`] reads it for every request, so that a page can tell without
//! each route passing it down ([`current`]): `app::document` loads the
//! local reminders' script in the app's pages only, and
//! `/account/notifications` shows their state there in place of the
//! browser's notifications (#668).
//!
//! The marker says which client the member chose, not who they are: anyone
//! can send it, so it may change what a page offers, never what a request
//! is allowed to do.

use axum::extract::Request;
use axum::http::{header::USER_AGENT, HeaderMap};
use axum::middleware::Next;
use axum::response::Response;

/// The token the app's WebView appends to its User-Agent, after a space:
/// `… Mobile Safari/537.36 MHomeApp`. A product token in the sense of RFC
/// 9110 § 10.1.5, with no version: the app's version is not this site's
/// business.
pub const USER_AGENT_MARKER: &str = "MHomeApp";

/// Whether the User-Agent `user_agent` is the app's: one of its
/// space-separated tokens is `USER_AGENT_MARKER`, alone or followed by a
/// `/version`. A token that only contains the marker
/// (`NotMHomeApp`, `MHomeAppX`) is someone else's.
pub fn is_mobile_app(user_agent: &str) -> bool {
    user_agent.split_whitespace().any(|token| {
        token
            .strip_prefix(USER_AGENT_MARKER)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// Whether the request with `headers` comes from the app: its
/// `User-Agent` is ([`is_mobile_app`]). No header, or one that is not
/// text, is a browser's.
pub fn from_headers(headers: &HeaderMap) -> bool {
    headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(is_mobile_app)
}

tokio::task_local! {
    /// Whether the request being served is the app's, set by [`scope`].
    static IN_APP: bool;
}

/// Whether the request being served comes from the app — `false` outside
/// [`scope`], as in a unit test rendering a page directly.
pub fn current() -> bool {
    IN_APP.try_with(|in_app| *in_app).unwrap_or(false)
}

/// Middleware: serves the request knowing whether it is the app's, for
/// [`current`].
pub async fn scope(request: Request, next: Next) -> Response {
    let in_app = from_headers(request.headers());
    IN_APP.scope(in_app, next.run(request)).await
}

/// Runs `render` as if serving a request from the app (`in_app`) or from
/// a browser: what [`scope`] does for a real one.
#[cfg(test)]
pub fn serving<R>(in_app: bool, render: impl FnOnce() -> R) -> R {
    IN_APP.sync_scope(in_app, render)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(user_agent: Option<&[u8]>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(ua) = user_agent {
            headers.insert(USER_AGENT, ua.try_into().unwrap());
        }
        headers
    }

    #[test]
    fn a_request_is_the_apps_by_its_user_agent_header() {
        let app = format!("{ANDROID_CHROME} {USER_AGENT_MARKER}");
        assert!(from_headers(&headers(Some(app.as_bytes()))));
        assert!(!from_headers(&headers(Some(ANDROID_CHROME.as_bytes()))));
        assert!(!from_headers(&headers(None)));
        // A header that is not text is no one's.
        assert!(!from_headers(&headers(Some(b"MHomeApp \xff"))));
    }

    #[test]
    fn outside_a_request_no_page_is_the_apps() {
        assert!(!current());
    }

    #[test]
    fn inside_its_scope_a_page_knows_it_is_the_apps() {
        assert!(serving(true, current));
        assert!(!serving(false, current));
    }

    const ANDROID_CHROME: &str = "Mozilla/5.0 (Linux; Android 14; Pixel 7) \
        AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36";

    #[test]
    fn the_apps_webview_is_recognised() {
        // What Capacitor's `appendUserAgent` makes of the WebView's own UA:
        // the marker after a space, at the end.
        let ua = format!("{ANDROID_CHROME} {USER_AGENT_MARKER}");
        assert!(is_mobile_app(&ua));
    }

    #[test]
    fn a_browser_is_not_the_app() {
        assert!(!is_mobile_app(ANDROID_CHROME));
        assert!(!is_mobile_app(
            "Mozilla/5.0 (X11; Linux x86_64; rv:132.0) Gecko/20100101 Firefox/132.0"
        ));
        assert!(!is_mobile_app(""));
    }

    #[test]
    fn the_marker_may_sit_anywhere_and_carry_a_version() {
        assert!(is_mobile_app(USER_AGENT_MARKER));
        assert!(is_mobile_app(&format!(
            "{USER_AGENT_MARKER} {ANDROID_CHROME}"
        )));
        assert!(is_mobile_app(&format!(
            "{ANDROID_CHROME} {USER_AGENT_MARKER}/2"
        )));
        assert!(is_mobile_app(&format!(
            "{ANDROID_CHROME}\t{USER_AGENT_MARKER}"
        )));
    }

    #[test]
    fn a_token_that_only_contains_the_marker_is_not_the_app() {
        assert!(!is_mobile_app(&format!(
            "{ANDROID_CHROME} Not{USER_AGENT_MARKER}"
        )));
        assert!(!is_mobile_app(&format!(
            "{ANDROID_CHROME} {USER_AGENT_MARKER}X"
        )));
        assert!(!is_mobile_app(&format!(
            "{ANDROID_CHROME} {USER_AGENT_MARKER}-beta"
        )));
        // Inside a comment, as in `(Linux; MHomeApp)`, it is not a product
        // token of its own.
        assert!(!is_mobile_app(&format!(
            "Mozilla/5.0 (Linux; {USER_AGENT_MARKER}) Chrome/130.0.0.0"
        )));
    }

    #[test]
    fn the_case_of_the_marker_counts() {
        // `appendUserAgent` writes it as it is; a lower-cased copy is not
        // the app's.
        assert!(!is_mobile_app(&format!(
            "{ANDROID_CHROME} {}",
            USER_AGENT_MARKER.to_lowercase()
        )));
    }

    #[test]
    fn the_marker_is_a_bare_product_token() {
        // `capacitor.config.ts` refuses anything else, and the detection
        // above splits on whitespace and `/`: a space or a slash inside
        // the marker would make it unrecognisable.
        assert!(!USER_AGENT_MARKER.is_empty());
        assert!(USER_AGENT_MARKER.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
