use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::GroupSummary;
use serde::Serialize;

/// Outcome of a JSON call to apps/api: the status code, an optional
/// `Set-Cookie` header value to forward to the browser (present on
/// `login`/`reset_password` success, and on ending one's own session from
/// `/account/sessions`), and the parsed JSON body (empty
/// object if apps/api returned no body, e.g. `204`/`200` with nothing).
pub struct ApiResponse {
    pub status: reqwest::StatusCode,
    pub set_cookie: Option<String>,
    /// Kept for callers that need the `{"error": "..."}` body directly
    /// (none of the current pages need more than the status code, since
    /// the issue's error table maps status -> UI state one-to-one).
    #[allow(dead_code)]
    pub body: serde_json::Value,
}

/// Shared server state for `apps/web`'s axum app.
#[derive(Clone)]
pub struct AppState {
    pub http: reqwest::Client,
    /// Base URL apps/web's SSR layer uses to call apps/api, over the
    /// internal Docker network (service name, not through Caddy) — e.g.
    /// `http://api:8080`. See infra/docker-compose.yml / infra/Caddyfile.
    pub api_internal_base_url: String,
    /// Base URL the *browser* uses to reach apps/api directly — only
    /// needed for the Google OAuth button, which links straight to
    /// `{api_public_base_url}/auth/google/start` (backend-hosted
    /// redirect, no fetch from the frontend). Same registrable domain as
    /// apps/web in production (`mondomaine.com/api`), so the session
    /// cookie set by apps/api's callback is sent on subsequent apps/web
    /// requests without any CORS configuration.
    pub api_public_base_url: String,
    /// How long a client may take to send a request body, on every route
    /// (#219). `BodyReadLimits::PRODUCTION` outside tests.
    pub body_read_limits: manage_our_home_http_guard::BodyReadLimits,
    /// Uploads this process holds in memory at once, per account and in
    /// all (#219). Taken by `routes::agenda::attachments::upload` before it
    /// reads the browser's body, and held until the relay to apps/api is
    /// answered: the bytes are in memory for all of it.
    pub upload_gate: std::sync::Arc<manage_our_home_http_guard::UploadGate<uuid::Uuid>>,
    /// Whether the process is stopping (#424): `/readyz` answers 503 from
    /// then on. `main.rs` triggers it on SIGTERM.
    pub shutdown: manage_our_home_http_guard::Shutdown,
}

/// What the incoming request's session is, as `GET /auth/me` answers it.
pub enum Session {
    /// A full session.
    Active(MeResponse),
    /// A restricted session (#289): the account is deactivated, and only
    /// the `/account/deactivated` page is open to it. apps/api answers
    /// `/auth/me` with 403 `account_deactivated`.
    Deactivated,
    /// The session of an account with no age declaration on file (#318):
    /// only the `/account/age` page is open to it. apps/api answers
    /// `/auth/me` with 403 `age_not_declared`.
    AgeUndeclared,
    /// The session of an account with no acceptance of the CGU on file
    /// (#319), age declared: only the `/account/terms` page is open to it.
    /// apps/api answers `/auth/me` with 403 `terms_not_accepted`.
    TermsNotAccepted,
    /// No session, an invalid one, or apps/api unreachable.
    None,
}

/// Whether a refused `GET /auth/me` — its status and the `error` code of
/// its body — names a restricted session (#289) rather than no session.
fn is_deactivated_answer(status: reqwest::StatusCode, error: Option<&str>) -> bool {
    status == reqwest::StatusCode::FORBIDDEN && error == Some("account_deactivated")
}

/// Whether a refused `GET /auth/me` names the session of an account with
/// no age declaration on file (#318) rather than no session.
fn is_age_undeclared_answer(status: reqwest::StatusCode, error: Option<&str>) -> bool {
    status == reqwest::StatusCode::FORBIDDEN && error == Some("age_not_declared")
}

/// Whether a refused `GET /auth/me` names the session of an account with
/// no acceptance of the CGU on file (#319) rather than no session.
fn is_terms_not_accepted_answer(status: reqwest::StatusCode, error: Option<&str>) -> bool {
    status == reqwest::StatusCode::FORBIDDEN && error == Some("terms_not_accepted")
}

/// Calls `GET /auth/me` on apps/api, forwarding the incoming request's
/// `Cookie` header so the session (if any) is recognized.
pub async fn fetch_session(state: &AppState, cookie_header: Option<&str>) -> Session {
    let mut req = state
        .http
        .get(format!("{}/auth/me", state.api_internal_base_url));
    if let Some(cookie) = cookie_header {
        req = req.header("cookie", cookie);
    }
    let Ok(resp) = req.send().await else {
        return Session::None;
    };
    let status = resp.status();
    if status.is_success() {
        return match resp.json::<MeResponse>().await {
            Ok(me) => Session::Active(me),
            Err(_) => Session::None,
        };
    }
    let error = resp
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|b| b.get("error").and_then(|e| e.as_str()).map(str::to_string));
    if is_deactivated_answer(status, error.as_deref()) {
        Session::Deactivated
    } else if is_age_undeclared_answer(status, error.as_deref()) {
        Session::AgeUndeclared
    } else if is_terms_not_accepted_answer(status, error.as_deref()) {
        Session::TermsNotAccepted
    } else {
        Session::None
    }
}

/// [`fetch_session`] for callers that only want a full session: `None`
/// covers no session, a restricted one, one awaiting its age declaration
/// (#318) or its acceptance of the CGU (#319), and any transport error
/// talking to apps/api — callers treat them all as "not authenticated".
pub async fn fetch_me(state: &AppState, cookie_header: Option<&str>) -> Option<MeResponse> {
    match fetch_session(state, cookie_header).await {
        Session::Active(me) => Some(me),
        Session::Deactivated
        | Session::AgeUndeclared
        | Session::TermsNotAccepted
        | Session::None => None,
    }
}

/// POSTs a JSON body to apps/api over the internal network, returning the
/// status/`Set-Cookie`/body so callers can implement the exact per-page
/// error-handling table from issue #15 without leaking raw JSON to the
/// browser. Transport failures (apps/api unreachable) surface as
/// `Err(String)` — callers render a generic "service unavailable" state.
///
/// `forwarded_for` is the proxy chain to relay, built by
/// `crate::client_ip::forwarded_for` from the incoming request. Only the
/// login page passes one today, because `POST /auth/login` is the one
/// route apps/api keys anything on the client address (#178); the other
/// callers pass `None` and their requests are unchanged. The neighbouring
/// `api_request_auth` relays `Cookie` the same way.
pub async fn api_post_json(
    state: &AppState,
    path: &str,
    body: impl Serialize,
    forwarded_for: Option<&str>,
) -> Result<ApiResponse, String> {
    let mut req = state
        .http
        .post(format!("{}{}", state.api_internal_base_url, path))
        .json(&body);
    if let Some(chain) = forwarded_for {
        req = req.header(crate::client_ip::FORWARDED_FOR, chain);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;

    let status = resp.status();
    let set_cookie = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body = resp.json::<serde_json::Value>().await.unwrap_or_default();

    Ok(ApiResponse {
        status,
        set_cookie,
        body,
    })
}

/// Sends an authenticated JSON request to apps/api over the internal
/// network, forwarding the incoming request's `Cookie` header so apps/api
/// recognizes the session — the Groups endpoints are all session-scoped,
/// unlike the Auth endpoints `api_post_json` was written for. `body:
/// None` sends no JSON body (e.g. DELETE). Transport failures surface as
/// `Err(String)`, same contract as `api_post_json`.
pub async fn api_request_auth(
    state: &AppState,
    method: reqwest::Method,
    path: &str,
    cookie_header: Option<&str>,
    body: Option<serde_json::Value>,
) -> Result<ApiResponse, String> {
    let mut req = state
        .http
        .request(method, format!("{}{}", state.api_internal_base_url, path));
    if let Some(cookie) = cookie_header {
        req = req.header("cookie", cookie);
    }
    if let Some(body) = body {
        req = req.json(&body);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;

    let status = resp.status();
    // Ending sessions from `/account/sessions` (#225) clears the caller's
    // own cookie when its session is among them.
    let set_cookie = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body = resp.json::<serde_json::Value>().await.unwrap_or_default();

    Ok(ApiResponse {
        status,
        set_cookie,
        body,
    })
}

/// Calls `GET /groups` on apps/api with the caller's session cookie:
/// every group the user belongs to, with their role in each. `None`
/// covers both an unauthenticated session and transport errors — callers
/// (the family switcher, /groups) render an empty list in that case.
pub async fn fetch_groups(
    state: &AppState,
    cookie_header: Option<&str>,
) -> Option<Vec<GroupSummary>> {
    let mut req = state
        .http
        .get(format!("{}/groups", state.api_internal_base_url));
    if let Some(cookie) = cookie_header {
        req = req.header("cookie", cookie);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<Vec<GroupSummary>>().await.ok()
}

/// Body of a non-JSON response from apps/api, kept as raw bytes.
pub struct ApiRawResponse {
    pub status: reqwest::StatusCode,
    pub body: Vec<u8>,
}

/// GETs a path on apps/api without parsing the body — its callers' reads are
/// not JSON-object responses `apps/web` inspects: `GET /account/export` (front
/// epic F10) is a document relayed byte-for-byte to the browser as a download
/// (parsing then re-serializing it would risk reshaping the user's own data on
/// the way out), and the three public legal documents — `/privacy-policy`,
/// `/legal-notice` and `/terms-of-service` — are `text/markdown`. Forwards the
/// incoming `Cookie` header when given one (export is session-scoped; the legal
/// documents are public).
pub async fn api_get_raw(
    state: &AppState,
    path: &str,
    cookie_header: Option<&str>,
) -> Result<ApiRawResponse, String> {
    let mut req = state
        .http
        .get(format!("{}{}", state.api_internal_base_url, path));
    if let Some(cookie) = cookie_header {
        req = req.header("cookie", cookie);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let body = resp.bytes().await.map_err(|e| e.to_string())?.to_vec();

    Ok(ApiRawResponse { status, body })
}

/// GETs a URL-encoded query against apps/api (used for the token-based
/// verify-email/reset-password landing pages).
pub async fn api_get(state: &AppState, path_and_query: &str) -> Result<ApiResponse, String> {
    let resp = state
        .http
        .get(format!("{}{}", state.api_internal_base_url, path_and_query))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status();
    let body = resp.json::<serde_json::Value>().await.unwrap_or_default();

    Ok(ApiResponse {
        status,
        set_cookie: None,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn a_403_account_deactivated_is_a_restricted_session() {
        assert!(is_deactivated_answer(
            StatusCode::FORBIDDEN,
            Some("account_deactivated")
        ));
    }

    #[test]
    fn a_401_is_no_session_whatever_its_body() {
        assert!(!is_deactivated_answer(StatusCode::UNAUTHORIZED, None));
        assert!(!is_deactivated_answer(
            StatusCode::UNAUTHORIZED,
            Some("account_deactivated")
        ));
    }

    #[test]
    fn another_403_is_no_session() {
        assert!(!is_deactivated_answer(
            StatusCode::FORBIDDEN,
            Some("forbidden")
        ));
        assert!(!is_deactivated_answer(StatusCode::FORBIDDEN, None));
    }

    // -- age declaration (#318) -------------------------------------------

    #[test]
    fn a_403_age_not_declared_awaits_the_age_declaration() {
        assert!(is_age_undeclared_answer(
            StatusCode::FORBIDDEN,
            Some("age_not_declared")
        ));
    }

    #[test]
    fn the_two_403_sessions_are_told_apart() {
        assert!(!is_age_undeclared_answer(
            StatusCode::FORBIDDEN,
            Some("account_deactivated")
        ));
        assert!(!is_deactivated_answer(
            StatusCode::FORBIDDEN,
            Some("age_not_declared")
        ));
    }

    #[test]
    fn no_other_answer_awaits_the_age_declaration() {
        assert!(!is_age_undeclared_answer(
            StatusCode::UNAUTHORIZED,
            Some("age_not_declared")
        ));
        assert!(!is_age_undeclared_answer(
            StatusCode::FORBIDDEN,
            Some("forbidden")
        ));
        assert!(!is_age_undeclared_answer(StatusCode::FORBIDDEN, None));
        assert!(!is_age_undeclared_answer(
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("age_declaration_required")
        ));
    }

    // -- acceptance of the CGU (#319) -------------------------------------

    #[test]
    fn a_403_terms_not_accepted_awaits_the_acceptance() {
        assert!(is_terms_not_accepted_answer(
            StatusCode::FORBIDDEN,
            Some("terms_not_accepted")
        ));
    }

    #[test]
    fn the_three_403_sessions_are_told_apart() {
        for other in ["account_deactivated", "age_not_declared"] {
            assert!(!is_terms_not_accepted_answer(
                StatusCode::FORBIDDEN,
                Some(other)
            ));
        }
        assert!(!is_deactivated_answer(
            StatusCode::FORBIDDEN,
            Some("terms_not_accepted")
        ));
        assert!(!is_age_undeclared_answer(
            StatusCode::FORBIDDEN,
            Some("terms_not_accepted")
        ));
    }

    #[test]
    fn no_other_answer_awaits_the_acceptance() {
        assert!(!is_terms_not_accepted_answer(
            StatusCode::UNAUTHORIZED,
            Some("terms_not_accepted")
        ));
        assert!(!is_terms_not_accepted_answer(StatusCode::FORBIDDEN, None));
        assert!(!is_terms_not_accepted_answer(
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("terms_acceptance_required")
        ));
    }
}
