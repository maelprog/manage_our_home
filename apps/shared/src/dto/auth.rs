//! Request/response shapes shared between `apps/api` (server-side handlers,
//! `apps/api/src/auth/mod.rs` / `oauth_google.rs`) and `apps/web` (forms +
//! the internal HTTP client calling the API). Kept field-for-field
//! identical to what `apps/api` used to define locally so there is exactly
//! one source of truth for the wire shape.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub display_name: String,
    /// The art. 8 GDPR age declaration (#137): `true` when the person declared
    /// being at least `validation::auth::MINIMUM_AGE_YEARS` old. A body that
    /// omits the field declares nothing, which `register` answers with a 422
    /// `age_declaration_required` rather than a deserialization error — the
    /// refusal then says which rule was broken.
    #[serde(default)]
    pub declares_minimum_age: bool,
    /// The acceptance of the CGU (#319): `true` when the person ticked the
    /// box. Omitted, it accepts nothing, answered with a 422
    /// `terms_acceptance_required`. The version recorded is the one in force,
    /// `validation::auth::TERMS_VERSION`.
    #[serde(default)]
    pub accepts_terms: bool,
}

/// Body of `POST /auth/terms-acceptance` (#319): the registration's box,
/// asked of an account with no acceptance on file — one opened through
/// Google, or before the acceptance was recorded — and of a member
/// acknowledging a new version. A missing field accepts nothing, answered
/// with the same 422 `terms_acceptance_required`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermsAcceptanceRequest {
    #[serde(default)]
    pub accepts_terms: bool,
}

/// Body of `POST /auth/age-declaration` (#318): the same declaration as
/// [`RegisterRequest::declares_minimum_age`], asked after the fact of an
/// account that has none on file — one opened through Google, or before
/// #137. A missing field declares nothing, answered with the same 422
/// `age_declaration_required`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgeDeclarationRequest {
    #[serde(default)]
    pub declares_minimum_age: bool,
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
    pub token: String,
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

/// Body of `POST /account/delete` (RGPD Art. 17 self-service erasure, front
/// epic F10). `current_password` is required — and verified — only for an
/// account that has a password; a Google-only account sends `None` (see
/// `validation::rgpd::validate_deletion_confirmation`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteAccountRequest {
    pub current_password: Option<String>,
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
    /// Whether the account has a password (`users.password_hash IS NOT NULL`) —
    /// i.e. whether the RGPD deletion flow (front epic F10, #25) must ask for
    /// it, since `delete_account` only verifies `current_password` for such
    /// accounts and a Google-only account confirms by re-consent instead.
    /// Defaults to `false`: the password field is then simply not asked for, and
    /// the backend stays the authority (it 401s a missing password).
    #[serde(default)]
    pub has_password: bool,
    /// Set while a self-service deletion request is in its grace period
    /// (`users.deletion_requested_at`, front epic F10). `apps/web` renders the
    /// pending banner + cancel action from it instead of the request form.
    #[serde(default)]
    pub deletion_requested_at: Option<DateTime<Utc>>,
    /// The version of the CGU the account last accepted
    /// (`users.terms_accepted_version`, #319). A full session always has one
    /// on file; `apps/web` compares it with `validation::auth::TERMS_VERSION`
    /// to tell a member who accepted an earlier text that the CGU changed.
    /// Defaults to `None`, which announces nothing.
    #[serde(default)]
    pub terms_accepted_version: Option<String>,
}

/// Generic `{"error": "..."}` body used by every `apps/api` error response
/// (`apps/api/src/error.rs::AppError::into_response`), except `owner_of_groups`
/// (409 with an extra `groups` array) which isn't relevant to the Auth epic's
/// pages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// Response of `GET /account/deactivated` (#289), the one read a restricted
/// session — opened by a correct login on a deactivated account — may make.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeactivatedAccountResponse {
    pub deactivated_at: DateTime<Utc>,
    /// When the pending reactivation request was made; `None` if there is
    /// none (never made, or decided since).
    pub reactivation_requested_at: Option<DateTime<Utc>>,
    /// When a request was first refused since the deactivation: a new one
    /// then suspends nothing.
    #[serde(default)]
    pub reactivation_refused_at: Option<DateTime<Utc>>,
    /// The holder's own deletion request, if any: the account then goes 30
    /// days after it, whatever else.
    #[serde(default)]
    pub deletion_requested_at: Option<DateTime<Utc>>,
}

/// Body of `POST /account/deactivated/reactivation-request` (#289). The
/// note is optional (`validation::user_admin::validate_reactivation_message`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReactivationRequestBody {
    #[serde(default)]
    pub message: Option<String>,
}

/// One entry of `GET /auth/sessions` (#225): a live session of the caller.
/// Only what the table already keeps — no IP, no user-agent (arbitrated
/// 2026-10-02, data minimization) — and never the token: `id` is the
/// internal `sessions.id`, which opens nothing since #222, and is what
/// `POST /auth/sessions/:id/revoke` takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveSession {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    /// Whether this is the session the request was made with.
    pub current: bool,
}
