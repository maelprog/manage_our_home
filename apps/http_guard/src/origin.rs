//! Cross-origin request protection (#223), on apps/api and apps/web alike.
//!
//! `SameSite=Lax` on the session cookie was the only barrier against
//! cross-site request forgery. It cannot stop a login CSRF (`POST /login`
//! needs no cookie beforehand, so there is nothing for `SameSite` to
//! withhold), and "same-site" is not "same-origin": a sibling subdomain of
//! the registrable domain gets the cookie on every request, the Messagerie
//! WebSocket handshake included.
//!
//! So every request that can change something — any method but `GET`,
//! `HEAD` and `OPTIONS`, plus a WebSocket upgrade, whose `GET` opens a
//! channel that reads the thread — must show it comes from the
//! application's own pages. The model is Go 1.25's
//! `http.CrossOriginProtection`, which OWASP's CSRF cheat sheet accepts as
//! a primary defence on modern browsers, with no token to manage:
//!
//! 1. an `Origin` equal to the trusted origin (apps/api: the origin of
//!    `FRONTEND_BASE_URL`, whose pages open the socket and may sit on
//!    another port, as in CI) passes;
//! 2. otherwise `Sec-Fetch-Site`, when the browser sends it, decides:
//!    `same-origin` or `none` (typed in the address bar, a bookmark) pass,
//!    anything else — `same-site`, `cross-site` — is refused;
//! 3. without `Sec-Fetch-Site`, `Origin` decides: it passes when its host
//!    is the request's own `Host`, and is refused otherwise (`null`
//!    included). This is the main case of the WebSocket handshake, not a
//!    fallback: Chromium sends no `Sec-Fetch-Site` on it, so the socket's
//!    defence rests on `Origin`, not on Fetch Metadata. It also covers
//!    browsers too old for Fetch Metadata, and plain http to a host other
//!    than `localhost`;
//! 4. a request with neither header passes. It is not a browser's, so it
//!    is no forgery: apps/web's own calls to apps/api over the internal
//!    network are of this kind, and so is `curl`.
//!
//! The Google OAuth callback is a legitimate cross-site navigation, but a
//! `GET`: it is not looked at here, and is protected by `state` and PKCE
//! (#193).

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

/// What the guard makes of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossOrigin {
    Allow,
    Refuse,
}

/// `Sec-Fetch-Site` (Fetch Metadata).
pub const SEC_FETCH_SITE: &str = "sec-fetch-site";

/// Judges a request from its method and headers. `trusted_origin` is
/// already normalised by [`origin_of`].
pub fn judge(method: &Method, headers: &HeaderMap, trusted_origin: Option<&str>) -> CrossOrigin {
    let safe = matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    if safe && !is_websocket_upgrade(headers) {
        return CrossOrigin::Allow;
    }

    // An `Origin` or a `Sec-Fetch-Site` sent twice refuses outright:
    // judging only the first value would let the second through unread.
    // No browser sends two.
    for name in [header::ORIGIN.as_str(), SEC_FETCH_SITE] {
        if headers.get_all(name).iter().nth(1).is_some() {
            return CrossOrigin::Refuse;
        }
    }

    // A header that is not valid text is taken as present and matching
    // nothing: it refuses, it never passes.
    let text = |name: &str| headers.get(name).map(|v| v.to_str().unwrap_or(""));
    let origin = text(header::ORIGIN.as_str());

    if let (Some(origin), Some(trusted)) = (origin, trusted_origin) {
        if origin.eq_ignore_ascii_case(trusted) {
            return CrossOrigin::Allow;
        }
    }

    if let Some(site) = text(SEC_FETCH_SITE) {
        return match site {
            "same-origin" | "none" => CrossOrigin::Allow,
            _ => CrossOrigin::Refuse,
        };
    }

    let Some(origin) = origin else {
        return CrossOrigin::Allow;
    };
    let authority = origin.split_once("://").map(|(_, rest)| rest);
    match (authority, text(header::HOST.as_str())) {
        (Some(authority), Some(host))
            if !host.is_empty() && authority.eq_ignore_ascii_case(host) =>
        {
            CrossOrigin::Allow
        }
        _ => CrossOrigin::Refuse,
    }
}

/// A `GET` asking to become a WebSocket (RFC 6455 §4.1: `Upgrade` carries
/// the `websocket` token, in any case).
fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::UPGRADE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case("websocket"))
}

/// The origin (`scheme://host[:port]`, lowercase, default port dropped) of
/// a base URL such as `FRONTEND_BASE_URL`, as a browser writes it in
/// `Origin`. `None` for a relative URL or anything without a scheme.
///
/// Userinfo (`user:pass@`) is kept, not stripped: a browser never writes it
/// in `Origin`, so such a base URL matches no request and the trusted-origin
/// rule never fires. A misconfiguration that refuses, never one that lets
/// a request through.
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if scheme.is_empty() || authority.is_empty() {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    let mut authority = authority.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => Some(":80"),
        "https" => Some(":443"),
        _ => None,
    };
    if let Some(port) = default_port {
        if let Some(host) = authority.strip_suffix(port) {
            authority = host.to_string();
        }
    }
    Some(format!("{scheme}://{authority}"))
}

/// The middleware's state: the trusted origin, and the body of the 403
/// (the API answers JSON, apps/web a page). Status is set by the
/// middleware.
#[derive(Clone)]
pub struct OriginGuard {
    pub trusted_origin: Option<std::sync::Arc<str>>,
    pub render: fn() -> Response,
}

/// Refuses, with a 403, a request [`judge`] refuses.
pub async fn guard_cross_origin(
    State(guard): State<OriginGuard>,
    req: Request,
    next: Next,
) -> Response {
    match judge(req.method(), req.headers(), guard.trusted_origin.as_deref()) {
        CrossOrigin::Allow => next.run(req).await,
        CrossOrigin::Refuse => {
            tracing::info!(
                method = %req.method(),
                path = req.uri().path(),
                origin = ?req.headers().get(header::ORIGIN),
                sec_fetch_site = ?req.headers().get(SEC_FETCH_SITE),
                "cross-origin request refused"
            );
            let mut resp = (guard.render)();
            *resp.status_mut() = StatusCode::FORBIDDEN;
            resp
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    const FRONT: &str = "http://localhost:3000";

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_static(v));
        }
        h
    }

    fn post(pairs: &[(&'static str, &'static str)]) -> CrossOrigin {
        judge(&Method::POST, &headers(pairs), Some(FRONT))
    }

    // --- methods that cannot change anything ---

    #[test]
    fn safe_methods_pass_even_cross_site() {
        let h = headers(&[
            ("sec-fetch-site", "cross-site"),
            ("origin", "https://evil.test"),
        ]);
        for m in [Method::GET, Method::HEAD, Method::OPTIONS] {
            assert_eq!(judge(&m, &h, Some(FRONT)), CrossOrigin::Allow, "{m}");
        }
    }

    #[test]
    fn every_unsafe_method_is_judged() {
        let h = headers(&[("sec-fetch-site", "cross-site")]);
        for m in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert_eq!(judge(&m, &h, Some(FRONT)), CrossOrigin::Refuse, "{m}");
        }
        // A method no route uses today is not a way around the guard.
        let other = Method::from_bytes(b"PROPFIND").unwrap();
        assert_eq!(judge(&other, &h, Some(FRONT)), CrossOrigin::Refuse);
    }

    // --- Sec-Fetch-Site ---

    #[test]
    fn same_origin_and_none_pass() {
        assert_eq!(
            post(&[("sec-fetch-site", "same-origin")]),
            CrossOrigin::Allow
        );
        assert_eq!(post(&[("sec-fetch-site", "none")]), CrossOrigin::Allow);
    }

    #[test]
    fn cross_site_is_refused_the_login_csrf() {
        assert_eq!(
            post(&[
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://evil.test")
            ]),
            CrossOrigin::Refuse
        );
    }

    #[test]
    fn same_site_is_refused_a_sibling_subdomain() {
        assert_eq!(
            post(&[
                ("sec-fetch-site", "same-site"),
                ("origin", "https://other.mondomaine.com"),
                ("host", "maison.mondomaine.com"),
            ]),
            CrossOrigin::Refuse
        );
    }

    #[test]
    fn an_unknown_sec_fetch_site_is_refused() {
        assert_eq!(post(&[("sec-fetch-site", "bogus")]), CrossOrigin::Refuse);
    }

    #[test]
    fn sec_fetch_site_decides_over_a_matching_host() {
        // A same-site request whose Origin host happens to equal Host is
        // still the browser saying "not same-origin": the header wins.
        assert_eq!(
            post(&[
                ("sec-fetch-site", "cross-site"),
                ("origin", "http://maison.test"),
                ("host", "maison.test"),
            ]),
            CrossOrigin::Refuse
        );
    }

    // --- the trusted origin ---

    #[test]
    fn the_trusted_origin_passes_even_same_site() {
        // CI: pages on localhost:3000 open the socket on localhost:8080.
        assert_eq!(
            post(&[("sec-fetch-site", "same-site"), ("origin", FRONT)]),
            CrossOrigin::Allow
        );
    }

    #[test]
    fn the_trusted_origin_is_compared_without_case() {
        assert_eq!(
            post(&[
                ("sec-fetch-site", "same-site"),
                ("origin", "HTTP://LocalHost:3000")
            ]),
            CrossOrigin::Allow
        );
    }

    #[test]
    fn another_port_of_the_trusted_host_is_not_trusted() {
        assert_eq!(
            post(&[
                ("sec-fetch-site", "same-site"),
                ("origin", "http://localhost:4000")
            ]),
            CrossOrigin::Refuse
        );
    }

    #[test]
    fn without_a_trusted_origin_nothing_is_trusted_by_origin() {
        let h = headers(&[("sec-fetch-site", "same-site"), ("origin", FRONT)]);
        assert_eq!(judge(&Method::POST, &h, None), CrossOrigin::Refuse);
    }

    // --- browsers without Sec-Fetch-Site ---

    #[test]
    fn an_origin_matching_host_passes() {
        assert_eq!(
            post(&[("origin", "https://maison.test"), ("host", "maison.test")]),
            CrossOrigin::Allow
        );
        assert_eq!(
            post(&[("origin", "http://mom-web:3000"), ("host", "MOM-WEB:3000")]),
            CrossOrigin::Allow
        );
    }

    #[test]
    fn a_foreign_origin_is_refused() {
        assert_eq!(
            post(&[("origin", "https://evil.test"), ("host", "maison.test")]),
            CrossOrigin::Refuse
        );
        // Same host, another port: another origin.
        assert_eq!(
            post(&[
                ("origin", "http://maison.test:8080"),
                ("host", "maison.test")
            ]),
            CrossOrigin::Refuse
        );
    }

    #[test]
    fn a_null_origin_is_refused() {
        assert_eq!(
            post(&[("origin", "null"), ("host", "maison.test")]),
            CrossOrigin::Refuse
        );
    }

    #[test]
    fn a_foreign_origin_without_host_is_refused() {
        assert_eq!(
            post(&[("origin", "https://evil.test")]),
            CrossOrigin::Refuse
        );
    }

    #[test]
    fn an_origin_with_a_path_does_not_match_host() {
        assert_eq!(
            post(&[("origin", "https://maison.test/x"), ("host", "maison.test")]),
            CrossOrigin::Refuse
        );
    }

    // --- a header sent twice ---

    #[test]
    fn a_duplicated_origin_is_refused() {
        // Reading only the first value would let the second ride along
        // unjudged: whichever comes first, two Origins refuse.
        assert_eq!(
            post(&[("origin", FRONT), ("origin", "https://evil.test")]),
            CrossOrigin::Refuse
        );
        assert_eq!(
            post(&[("origin", "https://evil.test"), ("origin", FRONT)]),
            CrossOrigin::Refuse
        );
        assert_eq!(
            post(&[
                ("origin", "https://maison.test"),
                ("origin", "https://maison.test"),
                ("host", "maison.test"),
            ]),
            CrossOrigin::Refuse
        );
    }

    #[test]
    fn a_duplicated_sec_fetch_site_is_refused() {
        assert_eq!(
            post(&[
                ("sec-fetch-site", "same-origin"),
                ("sec-fetch-site", "cross-site")
            ]),
            CrossOrigin::Refuse
        );
        assert_eq!(
            post(&[
                ("sec-fetch-site", "same-origin"),
                ("sec-fetch-site", "same-origin")
            ]),
            CrossOrigin::Refuse
        );
    }

    #[test]
    fn a_duplicated_origin_refuses_the_websocket_handshake() {
        assert_eq!(
            ws(&[("origin", FRONT), ("origin", "https://evil.test")]),
            CrossOrigin::Refuse
        );
    }

    // --- not a browser ---

    #[test]
    fn neither_header_passes_server_to_server() {
        // apps/web -> apps/api over the internal network.
        assert_eq!(post(&[("host", "api:8080")]), CrossOrigin::Allow);
        assert_eq!(post(&[]), CrossOrigin::Allow);
    }

    // --- the WebSocket handshake ---

    fn ws(pairs: &[(&'static str, &'static str)]) -> CrossOrigin {
        let mut all = vec![("upgrade", "websocket"), ("connection", "Upgrade")];
        all.extend_from_slice(pairs);
        judge(&Method::GET, &headers(&all), Some(FRONT))
    }

    #[test]
    fn a_websocket_upgrade_is_judged_like_a_post() {
        assert_eq!(ws(&[("origin", "https://evil.test")]), CrossOrigin::Refuse);
        assert_eq!(
            ws(&[
                ("sec-fetch-site", "same-site"),
                ("origin", "https://chat.mondomaine.com")
            ]),
            CrossOrigin::Refuse
        );
        assert_eq!(ws(&[("sec-fetch-site", "same-origin")]), CrossOrigin::Allow);
        assert_eq!(ws(&[("origin", FRONT)]), CrossOrigin::Allow);
        assert_eq!(ws(&[]), CrossOrigin::Allow);
    }

    #[test]
    fn the_upgrade_token_is_matched_without_case() {
        let h = headers(&[("upgrade", "WebSocket"), ("origin", "https://evil.test")]);
        assert_eq!(judge(&Method::GET, &h, Some(FRONT)), CrossOrigin::Refuse);
    }

    #[test]
    fn a_plain_get_with_a_foreign_origin_passes() {
        // A navigation or a subresource: nothing changes on a GET.
        let h = headers(&[("origin", "https://evil.test"), ("host", "maison.test")]);
        assert_eq!(judge(&Method::GET, &h, Some(FRONT)), CrossOrigin::Allow);
    }

    // --- origin_of ---

    #[test]
    fn origin_of_keeps_scheme_host_and_port() {
        assert_eq!(
            origin_of("http://localhost:3000").as_deref(),
            Some("http://localhost:3000")
        );
        assert_eq!(
            origin_of("https://maison.example.org").as_deref(),
            Some("https://maison.example.org")
        );
    }

    #[test]
    fn origin_of_drops_path_query_fragment_and_trailing_slash() {
        assert_eq!(
            origin_of("https://maison.example.org/").as_deref(),
            Some("https://maison.example.org")
        );
        assert_eq!(
            origin_of("https://maison.example.org/sub/path?x=1#y").as_deref(),
            Some("https://maison.example.org")
        );
    }

    #[test]
    fn origin_of_lowercases_and_drops_default_ports() {
        assert_eq!(
            origin_of("HTTPS://Maison.Example.ORG:443").as_deref(),
            Some("https://maison.example.org")
        );
        assert_eq!(
            origin_of("http://maison.test:80").as_deref(),
            Some("http://maison.test")
        );
        assert_eq!(
            origin_of("http://maison.test:443").as_deref(),
            Some("http://maison.test:443")
        );
    }

    #[test]
    fn origin_of_keeps_userinfo_so_nothing_is_trusted_by_origin() {
        // A browser never writes userinfo in `Origin`, so a base URL that
        // carries some can match no request: the trusted-origin rule never
        // fires, and the guard falls back on Sec-Fetch-Site and Host. A
        // misconfiguration that refuses, never one that lets through.
        let trusted = origin_of("http://user:secret@localhost:3000");
        assert_eq!(
            trusted.as_deref(),
            Some("http://user:secret@localhost:3000")
        );
        let h = headers(&[
            ("sec-fetch-site", "same-site"),
            ("origin", "http://localhost:3000"),
        ]);
        assert_eq!(
            judge(&Method::POST, &h, trusted.as_deref()),
            CrossOrigin::Refuse
        );
    }

    #[test]
    fn origin_of_refuses_what_is_not_an_absolute_url() {
        assert_eq!(origin_of("/api"), None);
        assert_eq!(origin_of(""), None);
        assert_eq!(origin_of("http://"), None);
        assert_eq!(origin_of("localhost:3000"), None);
    }
}
