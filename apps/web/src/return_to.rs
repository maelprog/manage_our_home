//! Where a visitor goes back to once logged in (#462). An invitation link
//! opened without a session used to end on `/` after the login, the
//! registration or the Google sign-in: the token was lost on the way.
//!
//! The path travels in a cookie of apps/web's own, set when `CurrentUser`
//! turns an anonymous visitor away from it (or by `/login?next=`), and read
//! back by `/` once a session exists — the page where the password login,
//! the Google callback (`FRONTEND_BASE_URL`) and the age and terms pages all
//! land. A cookie rather than a query parameter, so the invitation token is
//! never written into the URL of `/login` (browser history, access logs) and
//! survives the email verification and the round trip through Google.
//!
//! The path is checked by [`safe_return_path`] when it is stored and again
//! when it is read: only a same-site path of an allowed shape is ever a
//! redirect target, so nothing a URL or a cookie carries makes an open
//! redirect.

use manage_our_home_shared::validation::auth::is_bearer_token;

pub const RETURN_TO_COOKIE: &str = "return_to";

/// How long the path is kept: the lifetime of a verification link
/// (`EMAIL_VERIFICATION_TTL_HOURS`, 24 h), so a new account verified the
/// same day still lands on its invitation.
pub const RETURN_TO_MAX_AGE_SECS: u32 = 24 * 60 * 60;

/// `candidate` when it is a path apps/web may send a visitor back to, else
/// `None`.
///
/// A same-site path only — one `/`, then no `/` or `\` (`//host` and
/// `/\host` are both read as another host by browsers), no scheme, no
/// query, no fragment, nothing but the characters of a path segment — and
/// only of an allowed shape: today, the page of an invitation
/// (`/groups/invitations/{token}/accept`, the token spelled as apps/api
/// hands them out).
pub fn safe_return_path(candidate: &str) -> Option<&str> {
    if !is_same_site_path(candidate) {
        return None;
    }
    let token = candidate
        .strip_prefix("/groups/invitations/")?
        .strip_suffix("/accept")?;
    is_bearer_token(token).then_some(candidate)
}

/// Whether `candidate` is a path of this site, whatever its shape: one `/`,
/// then neither `/` nor `\`, and nothing but `[A-Za-z0-9_/-]`. The single
/// allowed shape of [`safe_return_path`] implies it today; it is checked on
/// its own so that a shape added to the list cannot open a redirect.
fn is_same_site_path(candidate: &str) -> bool {
    let Some(rest) = candidate.strip_prefix('/') else {
        return false;
    };
    !rest.starts_with('/')
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'/'))
}

/// The value of the return cookie the request carries, not yet checked: `/`
/// clears it whenever it is there, and follows it only when it is a
/// [`safe_return_path`].
pub fn return_cookie(cookie_header: Option<&str>) -> Option<&str> {
    cookie_header?.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == RETURN_TO_COOKIE).then_some(value)
    })
}

/// The `Set-Cookie` value storing `path`, which must already be a
/// [`safe_return_path`] (its characters are then valid in a cookie value).
/// `HttpOnly` and `SameSite=Lax` like the active-family cookie
/// (`family.rs`): no script reads it, and what lands on `/` after Google's
/// callback is a top-level navigation.
pub fn set_return_cookie(path: &str) -> String {
    format!(
        "{RETURN_TO_COOKIE}={path}; Path=/; Max-Age={RETURN_TO_MAX_AGE_SECS}; HttpOnly; SameSite=Lax"
    )
}

/// The `Set-Cookie` value removing the return path.
pub fn clear_return_cookie() -> String {
    format!("{RETURN_TO_COOKIE}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "Zq3_k9XvT2mQ8pLw4rYb7nHc1sDf6gJ0aEuIoVtBy5M";

    fn invitation() -> String {
        format!("/groups/invitations/{TOKEN}/accept")
    }

    #[test]
    fn an_invitation_page_is_a_return_path() {
        let path = invitation();
        assert_eq!(safe_return_path(&path), Some(path.as_str()));
    }

    /// The four of the issue's acceptance criteria, and their neighbours.
    #[test]
    fn another_site_is_never_a_return_path() {
        for evil in [
            "https://evil.example",
            "http://evil.example/groups/invitations/x/accept",
            "//evil.example",
            "//evil.example/groups/invitations/",
            "/\\evil",
            "/\\evil.example/groups/invitations/",
            "\\\\evil.example",
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "data:text/html,x",
            "",
            "groups/invitations/x/accept",
        ] {
            assert_eq!(safe_return_path(evil), None, "{evil:?}");
        }
    }

    #[test]
    fn a_path_outside_the_allowed_shapes_is_not_a_return_path() {
        let shaped = |rest: &str| format!("/groups/invitations/{TOKEN}{rest}");
        let paths = [
            "/".to_string(),
            "/admin/users".into(),
            "/groups".into(),
            "/groups/invitations/".into(),
            "/groups/invitations//accept".into(),
            "/groups/invitations/not-a-token/accept".into(),
            "/groups/invitations/%2F%2Fevil.example/accept".into(),
            shaped(""),
            shaped("/accept/"),
            shaped("/accept?x=1"),
            shaped("/accept#x"),
            shaped("/decline"),
            shaped("/../../admin"),
            shaped("/accept\r\nSet-Cookie: x=1"),
            format!(" {}", shaped("/accept")),
        ];
        for path in &paths {
            assert_eq!(safe_return_path(path), None, "{path:?}");
        }
    }

    #[test]
    fn the_cookie_round_trips_a_return_path() {
        let path = invitation();
        let set = set_return_cookie(&path);
        assert!(
            set.starts_with(&format!("{RETURN_TO_COOKIE}={path};")),
            "{set}"
        );
        assert!(set.contains("; Path=/"), "{set}");
        assert!(set.contains("; HttpOnly"), "{set}");
        assert!(set.contains("; SameSite=Lax"), "{set}");
        assert!(
            set.contains(&format!("; Max-Age={RETURN_TO_MAX_AGE_SECS}")),
            "{set}"
        );

        let header = format!("active_group_id=1; {RETURN_TO_COOKIE}={path}; other=2");
        assert_eq!(return_cookie(Some(&header)), Some(path.as_str()));
    }

    /// The site-wide filter on its own, shapes aside.
    #[test]
    fn a_same_site_path_is_one_slash_then_path_characters() {
        for path in [
            "/",
            "/groups",
            "/a/b-c_d/E9",
            "/groups/invitations/x/accept",
        ] {
            assert!(is_same_site_path(path), "{path:?}");
        }
        for path in [
            "",
            "groups",
            "//evil.example",
            "/\\evil",
            "\\\\evil",
            "https://evil.example",
            "javascript:alert(1)",
            "/a/../b",
            "/a?b",
            "/a#b",
            "/a%2F",
            "/a b",
            "/a\r\nb",
            "/é",
        ] {
            assert!(!is_same_site_path(path), "{path:?}");
        }
    }

    /// A cookie is the visitor's to write: it is read as it is, so that `/`
    /// can clear it, and is no return path unless it passes the check.
    #[test]
    fn a_cookie_carrying_another_site_is_read_but_no_return_path() {
        for value in ["//evil.example", "https://evil.example", "/admin/users", ""] {
            let header = format!("{RETURN_TO_COOKIE}={value}");
            assert_eq!(return_cookie(Some(&header)), Some(value), "{value:?}");
            assert_eq!(safe_return_path(value), None, "{value:?}");
        }
        assert_eq!(return_cookie(None), None);
        assert_eq!(return_cookie(Some("return_to_x=/")), None);
    }

    #[test]
    fn the_clearing_cookie_expires_the_same_cookie() {
        let clear = clear_return_cookie();
        assert!(
            clear.starts_with(&format!("{RETURN_TO_COOKIE}=;")),
            "{clear}"
        );
        assert!(clear.contains("; Path=/"), "{clear}");
        assert!(clear.contains("; Max-Age=0"), "{clear}");
    }
}

/// The cookie on the real router, against a stand-in apps/api.
#[cfg(test)]
mod route_tests {
    use axum::body::Body;
    use axum::http::{header, HeaderMap, Request, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::{Json, Router};
    use tower::ServiceExt;
    use uuid::Uuid;

    use super::{clear_return_cookie, set_return_cookie, RETURN_TO_COOKIE};
    use crate::state::AppState;

    const TOKEN: &str = "Zq3_k9XvT2mQ8pLw4rYb7nHc1sDf6gJ0aEuIoVtBy5M";

    fn invitation() -> String {
        format!("/groups/invitations/{TOKEN}/accept")
    }

    /// An apps/api that knows a session by its `session` cookie, and whose
    /// member belongs to no group.
    async fn fake_api() -> String {
        let app = Router::new()
            .route(
                "/auth/me",
                get(|headers: HeaderMap| async move {
                    let cookies = headers
                        .get(header::COOKIE)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    if !cookies.split(';').any(|c| c.trim().starts_with("session=")) {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    Json(serde_json::json!({
                        "user_id": Uuid::from_u128(7),
                        "email": "membre@example.test",
                        "display_name": "Membre",
                        "email_verified": true,
                    }))
                    .into_response()
                }),
            )
            .route("/groups", get(|| async { Json(serde_json::json!([])) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    async fn send(uri: &str, cookie: Option<&str>) -> Response {
        let router = crate::build_router(AppState {
            http: reqwest::Client::new(),
            api_internal_base_url: fake_api().await,
            api_public_base_url: "/api".into(),
            body_read_limits: manage_our_home_http_guard::BodyReadLimits::PRODUCTION,
            upload_gate: manage_our_home_http_guard::UploadGate::production(),
            shutdown: manage_our_home_http_guard::Shutdown::new(),
        });
        let mut request = Request::builder().uri(uri);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        router
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    fn location(response: &Response) -> Option<&str> {
        response
            .headers()
            .get(header::LOCATION)
            .map(|v| v.to_str().unwrap())
    }

    fn set_cookies(response: &Response) -> Vec<String> {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect()
    }

    /// An invitation opened without a session: the visitor goes to the
    /// login, the token in a cookie and not in the login's URL.
    #[tokio::test]
    async fn an_invitation_opened_without_a_session_is_remembered() {
        let response = send(&invitation(), None).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(location(&response), Some("/login"));
        assert_eq!(
            set_cookies(&response),
            vec![set_return_cookie(&invitation())]
        );
    }

    /// Any other page still sends to a bare login, and stores nothing.
    #[tokio::test]
    async fn another_page_opened_without_a_session_is_not_remembered() {
        let response = send("/groups", None).await;
        assert_eq!(location(&response), Some("/login"));
        assert!(set_cookies(&response).is_empty());
    }

    #[tokio::test]
    async fn a_next_parameter_naming_an_invitation_is_remembered() {
        let response = send(&format!("/login?next={}", invitation()), None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            set_cookies(&response),
            vec![set_return_cookie(&invitation())]
        );
    }

    /// The issue's four, URL-encoded as a link carries them: the login page
    /// is served and nothing is stored, so the login ends on `/`.
    #[tokio::test]
    async fn a_next_parameter_naming_another_site_is_ignored() {
        for next in [
            "https%3A%2F%2Fevil.example",
            "%2F%2Fevil.example",
            "%2F%5Cevil",
            "javascript%3Aalert(1)",
        ] {
            let response = send(&format!("/login?next={next}"), None).await;
            assert_eq!(response.status(), StatusCode::OK, "{next}");
            assert!(set_cookies(&response).is_empty(), "{next}");
        }
    }

    /// Once logged in, `/` sends the visitor back to the invitation, once.
    #[tokio::test]
    async fn the_home_page_sends_back_to_the_remembered_invitation_once() {
        let cookie = format!("session=s; {RETURN_TO_COOKIE}={}", invitation());
        let response = send("/", Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(location(&response), Some(invitation().as_str()));
        assert_eq!(set_cookies(&response), vec![clear_return_cookie()]);
    }

    /// A cookie written by hand to point elsewhere is not followed, and is
    /// cleared rather than left until its `Max-Age`: `/` sends back to
    /// itself, which then shows the dashboard.
    #[tokio::test]
    async fn the_home_page_follows_no_cookie_naming_another_site_and_clears_it() {
        for value in ["//evil.example", "https://evil.example", "/%5Cevil", ""] {
            let cookie = format!("session=s; {RETURN_TO_COOKIE}={value}");
            let response = send("/", Some(&cookie)).await;
            assert_eq!(response.status(), StatusCode::SEE_OTHER, "{value}");
            assert_eq!(location(&response), Some("/"), "{value}");
            assert_eq!(set_cookies(&response), vec![clear_return_cookie()]);
        }
        // Without the cookie, `/` is the dashboard and clears nothing.
        let response = send("/", Some("session=s")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(set_cookies(&response).is_empty());
    }

    /// Without a session, `/` keeps the cookie for after the login.
    #[tokio::test]
    async fn the_home_page_without_a_session_keeps_the_invitation() {
        let cookie = format!("{RETURN_TO_COOKIE}={}", invitation());
        let response = send("/", Some(&cookie)).await;
        assert_eq!(location(&response), Some("/login"));
        assert!(set_cookies(&response).is_empty());
    }
}
