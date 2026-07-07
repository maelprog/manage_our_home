pub mod audit;
pub mod auth;
pub mod crypto;
pub mod email;
pub mod error;
pub mod groups;
pub mod jobs;

use axum::routing::{delete, get, post};
use axum::Router;
use oauth2::basic::BasicClient;
use sqlx::PgPool;
use tower_cookies::CookieManagerLayer;

use crate::email::EmailSender;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub google_oauth: BasicClient,
    pub email: EmailSender,
    /// Base URL of this API, used to build links sent in emails.
    pub public_base_url: String,
    /// Base URL of the SvelteKit frontend, used for post-OAuth redirects.
    pub frontend_base_url: String,
    /// Symmetric key passed to `pgp_sym_encrypt`/`pgp_sym_decrypt` for
    /// OAuth refresh tokens (pgcrypto, AC #3). Loaded from env, never
    /// logged.
    pub oauth_encryption_key: String,
    pub secure_cookies: bool,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/auth/register", post(auth::register))
        .route("/auth/verify-email", get(auth::verify_email))
        .route("/auth/login", post(auth::login))
        .route("/auth/google/start", get(auth::oauth_google::start))
        .route("/auth/google/callback", get(auth::oauth_google::callback))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/password/forgot", post(auth::forgot_password))
        .route("/auth/password/reset", post(auth::reset_password))
        .route("/settings/password/change", post(auth::change_password))
        .route("/settings/password/set", post(auth::set_password))
        .route("/settings/google/link", post(auth::oauth_google::link))
        .route("/account/delete", post(auth::delete_account))
        .route("/account/delete/cancel", post(auth::cancel_delete_account))
        .route("/groups", post(groups::create_group))
        .route("/groups/:id", get(groups::get_group).delete(groups::delete_group))
        .route("/groups/:id/invitations", post(groups::create_invitation))
        .route(
            "/groups/invitations/:token/accept",
            post(groups::accept_invitation),
        )
        .route(
            "/groups/:id/members/:user_id/role",
            post(groups::change_role),
        )
        .route(
            "/groups/:id/members/:user_id",
            delete(groups::remove_member),
        )
        .route("/groups/:id/leave", post(groups::leave_group))
        .layer(CookieManagerLayer::new())
        .with_state(state)
}
