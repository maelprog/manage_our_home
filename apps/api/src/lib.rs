pub mod agenda;
pub mod audit;
pub mod auth;
pub mod crypto;
pub mod email;
pub mod error;
pub mod grocery_list;
pub mod groups;
pub mod jobs;
pub mod recipes;
pub mod stocks;
pub mod storage;

use axum::routing::{delete, get, post};
use axum::Router;
use oauth2::basic::BasicClient;
use sqlx::PgPool;
use tower_cookies::CookieManagerLayer;

use crate::email::EmailSender;
use crate::storage::Storage;

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
    /// MinIO/S3 client for event file attachments (architecture.md epic #10).
    pub storage: Storage,
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
        .route(
            "/groups/:id",
            get(groups::get_group).delete(groups::delete_group),
        )
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
        .route(
            "/groups/:id/events",
            post(agenda::events::create_event).get(agenda::events::list_events),
        )
        .route(
            "/groups/:id/events/:event_id",
            get(agenda::events::get_event)
                .patch(agenda::events::update_event)
                .delete(agenda::events::delete_event),
        )
        .route(
            "/groups/:id/events/:event_id/reminders",
            post(agenda::reminders::create_reminder),
        )
        .route(
            "/groups/:id/events/:event_id/reminders/:reminder_id",
            delete(agenda::reminders::delete_reminder),
        )
        .route(
            "/groups/:id/events/:event_id/attachments",
            post(agenda::attachments::upload_attachment).get(agenda::attachments::list_attachments),
        )
        .route(
            "/groups/:id/events/:event_id/attachments/:attachment_id/download",
            get(agenda::attachments::download_attachment),
        )
        .route(
            "/groups/:id/events/:event_id/attachments/:attachment_id",
            delete(agenda::attachments::delete_attachment),
        )
        .route(
            "/groups/:id/stock-items",
            post(stocks::items::create_stock_item).get(stocks::items::list_stock_items),
        )
        .route(
            "/groups/:id/stock-items/:item_id",
            get(stocks::items::get_stock_item)
                .patch(stocks::items::update_stock_item)
                .delete(stocks::items::delete_stock_item),
        )
        .route(
            "/groups/:id/recipes",
            post(recipes::crud::create_recipe).get(recipes::crud::list_recipes),
        )
        .route(
            "/groups/:id/recipes/suggestions",
            get(recipes::suggestions::suggest_recipes),
        )
        .route(
            "/groups/:id/recipes/meal-history",
            get(recipes::meal_history::list_meal_history),
        )
        .route(
            "/groups/:id/recipes/:recipe_id",
            get(recipes::crud::get_recipe)
                .patch(recipes::crud::update_recipe)
                .delete(recipes::crud::delete_recipe),
        )
        .route(
            "/groups/:id/recipes/:recipe_id/meal-history",
            post(recipes::meal_history::log_meal),
        )
        .route(
            "/groups/:id/grocery-items",
            post(grocery_list::items::create_grocery_item)
                .get(grocery_list::items::list_grocery_items),
        )
        .route(
            "/groups/:id/grocery-items/generate",
            post(grocery_list::items::generate_grocery_items),
        )
        .route(
            "/groups/:id/grocery-items/:item_id",
            get(grocery_list::items::get_grocery_item)
                .patch(grocery_list::items::update_grocery_item)
                .delete(grocery_list::items::delete_grocery_item),
        )
        .route(
            "/groups/:id/grocery-items/:item_id/check",
            post(grocery_list::items::check_grocery_item),
        )
        .layer(CookieManagerLayer::new())
        .with_state(state)
}
