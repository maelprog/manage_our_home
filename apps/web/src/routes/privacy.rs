//! `GET /privacy-policy` — the public privacy policy (front epic F10, issue
//! #25). Deliberately **not** under `routes::account`: a prospective user must be
//! able to read it before registering, so this is one of the three pages besides
//! the auth entry points that render without a session (linked from the login and
//! register footers).
//!
//! The content is not duplicated here: `apps/api` serves `docs/privacy-policy.md`
//! verbatim as `text/markdown` (compiled in via `include_str!`), and the page is
//! rendered by `routes::legal::public_document`, shared with the legal notice and
//! the CGU since #132 — so the deployed policy, the document in source control
//! and the API response can never drift, and the renderer never lets raw HTML
//! through.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;

use crate::layout::CurrentUserOpt;
use crate::routes::legal::public_document;
use crate::state::AppState;

pub async fn get(
    CurrentUserOpt(me): CurrentUserOpt,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    public_document(
        me,
        &state,
        &headers,
        "/privacy-policy",
        "Politique de confidentialité",
    )
    .await
}
