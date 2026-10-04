//! The age declaration asked after the fact (#318). Registration by password
//! records the art. 8 GDPR declaration (#137, `users.age_declared_at`); an
//! account opened through Google, or before #137, has none on file. Such an
//! account keeps logging in, but its session opens nothing until the
//! declaration is made: every route extracting `AuthUser` answers it with
//! 403 `age_not_declared`, and `apps/web` sends its holder to the
//! declaration page. With `POST /auth/logout`, the route below is the only
//! one it may call.
//!
//! As at registration, under-15s are not accepted and no parental-consent
//! path exists: declining the declaration leaves the account where it is,
//! closed to its holder.

use axum::extract::State;
use axum::{http::StatusCode, Json};

use manage_our_home_shared::dto::auth::AgeDeclarationRequest;
use manage_our_home_shared::validation::auth::validate_age_declaration;

use crate::error::{AppError, AppResult};
use crate::AppState;

use super::session::AgeUndeclaredSession;

/// `POST /auth/age-declaration`: records the declaration (`age_declared_at
/// = now()`), after which the same session opens the whole app. 422
/// `age_declaration_required` unless the body declares the minimum age —
/// the registration's rule and code. Any session but that of an account
/// without declaration is a 401: the declaration is made once, and never
/// rewritten.
pub async fn declare(
    State(state): State<AppState>,
    session: AgeUndeclaredSession,
    Json(body): Json<AgeDeclarationRequest>,
) -> AppResult<StatusCode> {
    validate_age_declaration(body.declares_minimum_age)
        .map_err(|code| AppError::Unprocessable(code.into()))?;

    sqlx::query!(
        r#"
        UPDATE users SET age_declared_at = now()
        WHERE id = $1 AND age_declared_at IS NULL
        "#,
        session.user_id
    )
    .execute(&state.db)
    .await?;

    Ok(StatusCode::NO_CONTENT)
}
