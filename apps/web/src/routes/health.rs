//! The probes an orchestrator asks this process (#424). Mounted outside
//! every guard of `build_router` — no session, no origin or body check —
//! and silent: a probe every few seconds would otherwise bury the journal.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;

use crate::state::AppState;

/// How long `/readyz` waits for apps/api.
pub const READY_API_TIMEOUT: Duration = Duration::from_secs(2);

/// The process answers: no I/O. Still 200 while draining — a stopping
/// process is alive, and failing this probe would have it killed before its
/// grace period is over.
pub async fn healthz() -> StatusCode {
    StatusCode::OK
}

/// The process can serve: it is not stopping, and apps/api answers its own
/// `/healthz`. Not apps/api's `/readyz`: a Postgres outage would then take
/// every web replica out of service too, where they can still answer the
/// pages that need no data (the legal documents, the login form).
pub async fn readyz(State(state): State<AppState>) -> StatusCode {
    if state.shutdown.is_draining() {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    let probe = state
        .http
        .get(format!("{}/healthz", state.api_internal_base_url))
        .timeout(READY_API_TIMEOUT)
        .send()
        .await;
    match probe {
        Ok(resp) if resp.status().is_success() => StatusCode::OK,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    use crate::state::AppState;

    /// An apps/api whose `/healthz` answers `status`.
    async fn fake_api(status: StatusCode) -> String {
        let app = Router::new().route("/healthz", get(move || async move { status }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn state(api: String) -> AppState {
        AppState {
            http: reqwest::Client::new(),
            api_internal_base_url: api,
            api_public_base_url: "/api".into(),
            body_read_limits: manage_our_home_http_guard::BodyReadLimits::PRODUCTION,
            upload_gate: manage_our_home_http_guard::UploadGate::production(),
            shutdown: manage_our_home_http_guard::Shutdown::new(),
        }
    }

    async fn probe(state: AppState, path: &str) -> StatusCode {
        let router = crate::build_router(state);
        let request = Request::builder().uri(path).body(Body::empty()).unwrap();
        router.oneshot(request).await.unwrap().status()
    }

    #[tokio::test]
    async fn both_answer_200_while_apps_api_answers() {
        let api = fake_api(StatusCode::OK).await;
        assert_eq!(probe(state(api.clone()), "/healthz").await, StatusCode::OK);
        assert_eq!(probe(state(api), "/readyz").await, StatusCode::OK);
    }

    #[tokio::test]
    async fn readyz_is_503_when_apps_api_is_unreachable_or_failing() {
        // Nothing listens on port 1.
        let down = "http://127.0.0.1:1".to_string();
        assert_eq!(probe(state(down.clone()), "/readyz").await, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(probe(state(down), "/healthz").await, StatusCode::OK);
        let failing = fake_api(StatusCode::INTERNAL_SERVER_ERROR).await;
        assert_eq!(probe(state(failing), "/readyz").await, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn readyz_is_503_once_draining_and_healthz_stays_200() {
        let state = state(fake_api(StatusCode::OK).await);
        state.shutdown.trigger();
        assert_eq!(probe(state.clone(), "/readyz").await, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(probe(state, "/healthz").await, StatusCode::OK);
    }
}
