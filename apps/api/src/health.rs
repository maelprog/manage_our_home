//! The probes an orchestrator asks this process (#424). Mounted outside
//! every guard of `build_router` — no session, no cookie, no origin or body
//! check, no limit — and silent: a probe every few seconds would otherwise
//! bury the journal.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;

use crate::AppState;

/// How long `/readyz` waits for Postgres. Under the probe timeouts an
/// orchestrator uses (Kubernetes: 1 s by default, raised in #431's
/// manifests), and it bounds the case where the server does not answer at
/// all rather than refuse.
pub const READY_DB_TIMEOUT: Duration = Duration::from_secs(2);

/// The process answers: no I/O, nothing to fail but the process itself.
/// Still 200 while draining — a stopping process is alive, and failing
/// this probe would have it killed before its grace period is over.
pub async fn healthz() -> StatusCode {
    StatusCode::OK
}

/// The process can serve: it is not stopping, and its runtime pool reaches
/// Postgres. 503 otherwise, with no body — nothing about the cause leaves
/// the process.
pub async fn readyz(State(state): State<AppState>) -> StatusCode {
    if state.shutdown.is_draining() {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    match tokio::time::timeout(
        READY_DB_TIMEOUT,
        sqlx::query("SELECT 1").execute(&state.db),
    )
    .await
    {
        Ok(Ok(_)) => StatusCode::OK,
        Ok(Err(_)) | Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}
