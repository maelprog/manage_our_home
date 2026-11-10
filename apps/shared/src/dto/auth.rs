//! Request/response shapes shared between `apps/api` (server-side handlers,
//! `apps/api/src/auth/mod.rs` / `oauth_google.rs`) and `apps/web` (forms +
//! the internal HTTP client calling the API). Kept field-for-field
//! identical to what `apps/api` used to define locally so there is exactly
//! one source of truth for the wire shape.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
    /// `terms_acceptance_required`. The version recorded is the one in force
    /// that day, `validation::auth::terms_in_force_on` (#367).
    #[serde(default)]
    pub accepts_terms: bool,
}

/// Body of `POST /auth/terms-acceptance` (#319): the registration's box,
/// asked of an account with no acceptance on file — one opened through
/// Google, or before the acceptance was recorded — and of a member
/// acknowledging a new version. A missing field accepts nothing, answered
/// with the same 422 `terms_acceptance_required`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AgeDeclarationRequest {
    #[serde(default)]
    pub declares_minimum_age: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ForgotPasswordRequest {
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ResendVerificationRequest {
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ResetPasswordRequest {
    pub token: String,
    pub new_password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SetPasswordRequest {
    pub new_password: String,
}

/// Body of `POST /account/delete` (RGPD Art. 17 self-service erasure, front
/// epic F10). `current_password` is required — and verified — only for an
/// account that has a password; a Google-only account sends `None` (see
/// `validation::rgpd::validate_deletion_confirmation`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DeleteAccountRequest {
    pub current_password: Option<String>,
}

/// Response shape for `GET /auth/me`: `AuthUser`
/// (`apps/api/src/auth/session.rs`) minus the internal `session_id` — the
/// `me` handler (`apps/api/src/auth/mod.rs::me`) is a straight reshape of
/// that extractor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
    /// on file; `apps/web` checks whether it covers the version in force that
    /// day (`validation::auth::terms_in_force_on`,
    /// `validation::auth::terms_acceptance_covers`, #367) to tell a member who
    /// accepted an earlier text that the CGU changed.
    /// Defaults to `None`, which announces nothing.
    #[serde(default)]
    pub terms_accepted_version: Option<String>,
}

/// Generic `{"error": "..."}` body used by every `apps/api` error response
/// (`apps/api/src/error.rs::AppError::into_response`), except `owner_of_groups`
/// (409 with an extra `groups` array) which isn't relevant to the Auth epic's
/// pages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ErrorResponse {
    pub error: String,
}

/// Response of `GET /account/deactivated` (#289), the one read a restricted
/// session — opened by a correct login on a deactivated account — may make.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReactivationRequestBody {
    #[serde(default)]
    pub message: Option<String>,
}

/// One entry of `GET /auth/sessions` (#225): a live session of the caller.
/// Only what the table already keeps — no IP, no user-agent (arbitrated
/// 2026-10-02, data minimization; #754 keeps only whether the session was
/// opened in the app or on the web) — and never the token: `id` is the
/// internal `sessions.id`, which opens nothing since #222, and names the
/// row for apps/web; no route takes it (#753).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ActiveSession {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    /// Whether this is the session the request was made with.
    pub current: bool,
    /// The client the session was opened from (#754); `None` for a session
    /// opened before it was recorded.
    #[serde(default)]
    pub client: Option<SessionClient>,
}

/// The client a session was opened from (#754), `sessions.client`: the
/// Android app or the web. Deduced from the User-Agent at the opening
/// (`mobile_app::client_of_user_agent`), which is not kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SessionClient {
    Web,
    App,
}

impl SessionClient {
    /// The value `sessions.client` stores, one its CHECK allows; the JSON
    /// spells it the same way.
    pub fn as_str(self) -> &'static str {
        match self {
            SessionClient::Web => "web",
            SessionClient::App => "app",
        }
    }

    /// The client a `sessions.client` value names; `None` for any other
    /// text, which the column's CHECK keeps out.
    pub fn from_column(value: &str) -> Option<Self> {
        [SessionClient::Web, SessionClient::App]
            .into_iter()
            .find(|client| client.as_str() == value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_column_values_are_web_and_app() {
        assert_eq!(SessionClient::Web.as_str(), "web");
        assert_eq!(SessionClient::App.as_str(), "app");
    }

    #[test]
    fn a_column_value_reads_back_as_its_client() {
        for client in [SessionClient::Web, SessionClient::App] {
            assert_eq!(SessionClient::from_column(client.as_str()), Some(client));
        }
    }

    #[test]
    fn any_other_column_value_is_no_client() {
        assert_eq!(SessionClient::from_column(""), None);
        assert_eq!(SessionClient::from_column("App"), None);
        assert_eq!(SessionClient::from_column("android"), None);
    }
}
