//! Telling a request from the Android app apart from a browser's (#665).
//!
//! The marker the app's WebView appends to its User-Agent, and how a
//! User-Agent is read, live in `manage_our_home_shared::mobile_app`, so
//! that apps/api reads it the same way when it records which client opened
//! a session (#754). `apps/mobile/capacitor.config.ts` reads the constant
//! out of that file.
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

pub use manage_our_home_shared::mobile_app::{is_mobile_app, USER_AGENT_MARKER};

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
}
