//! The acceptance of the CGU outside registration (#319). Registration by
//! password records it (`users.terms_accepted_version` / `terms_accepted_at`);
//! an account opened through Google, or before #319, has none on file. Such
//! an account keeps logging in, but once its age is declared (#318) its
//! session opens nothing until the CGU are accepted: every route extracting
//! `AuthUser` answers it with 403 `terms_not_accepted`, and `apps/web` sends
//! its holder to the acceptance page.
//!
//! The same route records a member's acknowledgement of a new version. The
//! CGU make continued use worth acceptance of a new version announced before
//! it applies; a full session is never held for it, `apps/web` shows the
//! notice on the home page until the member acknowledges it here.

use axum::extract::State;
use axum::{http::StatusCode, Json};

use manage_our_home_shared::dto::auth::TermsAcceptanceRequest;
use manage_our_home_shared::validation::auth::{validate_terms_acceptance, TERMS_VERSION};

use crate::error::{AppError, AppResult};
use crate::AppState;

use super::session::TermsSession;

/// `POST /auth/terms-acceptance`: records the version in force
/// (`TERMS_VERSION`) and now, after which a session held at the acceptance
/// page opens the whole app. 422 `terms_acceptance_required` unless the
/// body accepts — the registration's rule and code. An account that already
/// accepted the version in force keeps its date: the first acceptance of a
/// version is the one on file. A restricted session, or one awaiting its age
/// declaration, is a 401.
pub async fn accept(
    State(state): State<AppState>,
    session: TermsSession,
    Json(body): Json<TermsAcceptanceRequest>,
) -> AppResult<StatusCode> {
    validate_terms_acceptance(body.accepts_terms)
        .map_err(|code| AppError::Unprocessable(code.into()))?;

    sqlx::query!(
        r#"
        UPDATE users SET terms_accepted_version = $2, terms_accepted_at = now()
        WHERE id = $1 AND terms_accepted_version IS DISTINCT FROM $2
        "#,
        session.user_id,
        TERMS_VERSION
    )
    .execute(&state.db)
    .await?;

    Ok(StatusCode::NO_CONTENT)
}
