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
//!
//! The cookie is `__Host-` prefixed (#576): the browser accepts a cookie of
//! that name only when it is `Secure`, `Path=/` and has no `Domain`, so a
//! sibling host (another subdomain, a staging instance) or a plain-HTTP
//! answer cannot plant one. And `/` never redirects to itself on a cookie it
//! rejects: it clears it and shows the home page in the same answer, so no
//! cookie, wherever it came from, can make a redirect loop. The price of the
//! prefix: over plain HTTP, Chromium and Firefox keep such a cookie only
//! from `localhost`, and WebKit (Safari) from no origin at all, `localhost`
//! included — it keeps it over HTTPS only. Where it is not kept, an
//! invitation opened logged out ends on `/`.

use manage_our_home_shared::validation::auth::is_bearer_token;

pub const RETURN_TO_COOKIE: &str = "__Host-return_to";

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
    checked_return_path(candidate, is_invitation_page)
}

/// `candidate` when it is a same-site path of the `shape` given. Apart from
/// [`safe_return_path`] so that a test can hold the site-wide filter behind
/// a shape that lets everything through.
fn checked_return_path(candidate: &str, shape: impl Fn(&str) -> bool) -> Option<&str> {
    (is_same_site_path(candidate) && shape(candidate)).then_some(candidate)
}

/// `/groups/invitations/{token}/accept`, the token spelled as apps/api
/// hands them out.
fn is_invitation_page(candidate: &str) -> bool {
    candidate
        .strip_prefix("/groups/invitations/")
        .and_then(|rest| rest.strip_suffix("/accept"))
        .is_some_and(is_bearer_token)
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
/// callback is a top-level navigation. `Secure`, `Path=/` and no `Domain`:
/// what the `__Host-` prefix demands, or the browser drops the cookie.
pub fn set_return_cookie(path: &str) -> String {
    format!(
        "{RETURN_TO_COOKIE}={path}; Path=/; Max-Age={RETURN_TO_MAX_AGE_SECS}; Secure; HttpOnly; SameSite=Lax"
    )
}

/// The `Set-Cookie` value removing the return path, with the attributes of
/// the one storing it: the `__Host-` prefix applies to it as well.
pub fn clear_return_cookie() -> String {
    format!("{RETURN_TO_COOKIE}=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Lax")
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

    /// #576: the `__Host-` prefix makes the browser refuse a cookie of this
    /// name unless it is `Secure`, has `Path=/` and no `Domain` — so no
    /// sibling host and no plain-HTTP answer can plant one. The clearing
    /// cookie must carry the same attributes, or the browser drops it.
    #[test]
    fn the_cookie_is_host_prefixed_secure_and_host_only() {
        assert_eq!(RETURN_TO_COOKIE, "__Host-return_to");
        for set in [set_return_cookie(&invitation()), clear_return_cookie()] {
            let mut parts = set.split("; ");
            assert!(
                parts.next().unwrap().starts_with("__Host-return_to="),
                "{set}"
            );
            let attributes: Vec<&str> = parts.collect();
            assert!(attributes.contains(&"Secure"), "{set}");
            assert!(attributes.contains(&"Path=/"), "{set}");
            assert!(
                !attributes
                    .iter()
                    .any(|a| a.to_ascii_lowercase().starts_with("domain")),
                "{set}"
            );
        }
    }

    /// #576: the name the cookie had before is no longer read, so a cookie
    /// left over from it, or planted under it, leads nowhere.
    #[test]
    fn the_former_cookie_name_is_not_read() {
        let header = format!("return_to={}", invitation());
        assert_eq!(return_cookie(Some(&header)), None);
    }

    /// The site-wide filter holds even behind a shape that would let
    /// anything through: a shape added to the list cannot open a redirect.
    #[test]
    fn no_shape_lets_another_site_through() {
        for evil in [
            "//evil.example",
            "/\\evil",
            "https://evil.example",
            "javascript:alert(1)",
            "/a?b",
            "",
        ] {
            assert_eq!(checked_return_path(evil, |_| true), None, "{evil:?}");
        }
        assert_eq!(checked_return_path("/groups", |_| true), Some("/groups"));
        assert_eq!(checked_return_path("/groups", |_| false), None);
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

    async fn body(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
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
    /// cleared rather than left until its `Max-Age` — in the same answer,
    /// which is the home page itself (#576): sending back to `/` looped
    /// forever on a cookie the clearing did not reach.
    #[tokio::test]
    async fn the_home_page_follows_no_cookie_naming_another_site_and_clears_it() {
        for value in [
            "//evil.example",
            "https://evil.example",
            "/%5Cevil",
            "/",
            "",
        ] {
            let cookie = format!("session=s; {RETURN_TO_COOKIE}={value}");
            let response = send("/", Some(&cookie)).await;
            assert_eq!(response.status(), StatusCode::OK, "{value}");
            assert_eq!(location(&response), None, "{value}");
            assert_eq!(set_cookies(&response), vec![clear_return_cookie()]);
            assert!(body(response).await.contains("Bienvenue"), "{value}");
        }
        // Without the cookie, `/` is the dashboard and clears nothing.
        let response = send("/", Some("session=s")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(set_cookies(&response).is_empty());
    }

    /// #576: a cookie under the former name is neither followed nor
    /// touched: the browser keeps it until its `Max-Age`, harmlessly.
    #[tokio::test]
    async fn the_home_page_ignores_the_former_cookie_name() {
        let cookie = format!("session=s; return_to={}", invitation());
        let response = send("/", Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(set_cookies(&response).is_empty());
    }

    /// An invalid `?next=` stores nothing, and so leaves alone an
    /// invitation remembered earlier in the same browser: the login then
    /// ends on that invitation, a checked path, rather than on `/`.
    #[tokio::test]
    async fn an_invalid_next_parameter_keeps_a_remembered_invitation() {
        let cookie = format!("{RETURN_TO_COOKIE}={}", invitation());
        let response = send("/login?next=%2F%2Fevil.example", Some(&cookie)).await;
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
