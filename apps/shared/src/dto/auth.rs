//! Request/response shapes shared between `apps/api` (server-side handlers,
//! `apps/api/src/auth/mod.rs` / `oauth_google.rs`) and `apps/web` (forms +
//! the internal HTTP client calling the API). Kept field-for-field
//! identical to what `apps/api` used to define locally so there is exactly
//! one source of truth for the wire shape.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgotPasswordRequest {
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResendVerificationRequest {
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResetPasswordRequest {
    pub token: Uuid,
    pub new_password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetPasswordRequest {
    pub new_password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkGoogleRequest {
    pub code: String,
}

/// Response shape for `GET /auth/me`: `AuthUser`
/// (`apps/api/src/auth/session.rs`) minus the internal `session_id` — the
/// `me` handler (`apps/api/src/auth/mod.rs::me`) is a straight reshape of
/// that extractor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeResponse {
    pub user_id: Uuid,
    pub email: String,
    pub display_name: String,
    pub email_verified: bool,
    /// Whether the session's user is the global technical superadmin
    /// (`users.is_superadmin`). `apps/web` uses it to gate the `/admin` nav and
    /// route tree client-side (front epic F9, #24). Defaults to `false` when an
    /// older API omits the field, so a non-superadmin is the safe fallback.
    #[serde(default)]
    pub is_superadmin: bool,
}

/// Generic `{"error": "..."}` body used by every `apps/api` error response
/// (`apps/api/src/error.rs::AppError::into_response`), except `owner_of_groups`
/// (409 with an extra `groups` array) which isn't relevant to the Auth epic's
/// pages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}
