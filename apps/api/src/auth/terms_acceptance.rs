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
use chrono::Utc;

use manage_our_home_shared::dto::auth::TermsAcceptanceRequest;
use manage_our_home_shared::validation::auth::{
    paris_day, terms_in_force_on, validate_terms_acceptance,
};

use crate::error::{AppError, AppResult};
use crate::AppState;

use super::session::TermsSession;

/// The CGU version in force now (#367). Read from the clock on every call,
/// so a version announced ahead takes effect on its day — in Europe/Paris —
/// without a restart or a deploy.
pub fn terms_in_force_now() -> &'static str {
    terms_in_force_on(paris_day(Utc::now()))
}

/// `POST /auth/terms-acceptance`: records the version in force
/// ([`terms_in_force_now`]) and now, after which a session held at the
/// acceptance page opens the whole app. 422 `terms_acceptance_required`
/// unless the body accepts — the registration's rule and code. An account
/// whose acceptance on file already covers the version in force keeps it:
/// the first acceptance of a version is the one on file, and an acceptance
/// of a later version — recorded before a rollback to a release that
/// predates it (#367) — is never replaced by an earlier one. Versions are
/// `YYYY-MM-DD` dates, so comparing them as text in the `"C"` collation
/// orders them as dates. A restricted session, or one awaiting its age
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
        WHERE id = $1
          AND (terms_accepted_version IS NULL
               OR terms_accepted_version COLLATE "C" < $2)
        "#,
        session.user_id,
        terms_in_force_now()
    )
    .execute(&state.db)
    .await?;

    Ok(StatusCode::NO_CONTENT)
}
