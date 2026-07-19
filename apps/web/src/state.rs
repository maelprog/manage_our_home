use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::GroupSummary;
use serde::Serialize;

/// Outcome of a JSON call to apps/api: the status code, an optional
/// `Set-Cookie` header value to forward to the browser (present on
/// `login`/`reset_password` success), and the parsed JSON body (empty
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
}

/// Calls `GET /auth/me` on apps/api, forwarding the incoming request's
/// `Cookie` header so the session (if any) is recognized. `None` covers
/// both "no session" (401) and any transport error talking to apps/api —
/// callers treat both as "not authenticated" for redirect purposes.
pub async fn fetch_me(state: &AppState, cookie_header: Option<&str>) -> Option<MeResponse> {
    let mut req = state
        .http
        .get(format!("{}/auth/me", state.api_internal_base_url));
    if let Some(cookie) = cookie_header {
        req = req.header("cookie", cookie);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<MeResponse>().await.ok()
}

/// POSTs a JSON body to apps/api over the internal network, returning the
/// status/`Set-Cookie`/body so callers can implement the exact per-page
/// error-handling table from issue #15 without leaking raw JSON to the
/// browser. Transport failures (apps/api unreachable) surface as
/// `Err(String)` — callers render a generic "service unavailable" state.
pub async fn api_post_json(
    state: &AppState,
    path: &str,
    body: impl Serialize,
) -> Result<ApiResponse, String> {
    let resp = state
        .http
        .post(format!("{}{}", state.api_internal_base_url, path))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

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
    let body = resp.json::<serde_json::Value>().await.unwrap_or_default();

    Ok(ApiResponse {
        status,
        set_cookie: None,
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
