pub mod agenda;
pub mod audit;
pub mod auth;
pub mod budget;
pub mod crypto;
pub mod email;
pub mod error;
pub mod google_calendar;
pub mod grocery_list;
pub mod groups;
pub mod jobs;
pub mod messagerie;
pub mod recipes;
pub mod rgpd;
pub mod stocks;
pub mod storage;
pub mod user_admin;

use axum::routing::{delete, get, post};
use axum::Router;
use oauth2::{basic::BasicClient, EndpointNotSet, EndpointSet};
use sqlx::PgPool;
use tower_cookies::CookieManagerLayer;

use crate::email::EmailSender;
use crate::storage::Storage;

/// Google's client: auth + token URLs configured, no device-auth/introspection/revocation.
pub type GoogleOauthClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub google_oauth: GoogleOauthClient,
    pub email: EmailSender,
    /// Base URL of this API, used to build links sent in emails.
    pub public_base_url: String,
    /// Base URL of the SvelteKit frontend, used for post-OAuth redirects.
    pub frontend_base_url: String,
    /// Symmetric key passed to `pgp_sym_encrypt`/`pgp_sym_decrypt` for
    /// OAuth refresh tokens (pgcrypto, AC #3). Loaded from env, never
    /// logged.
    pub oauth_encryption_key: String,
    /// Symmetric key passed to `pgp_sym_encrypt`/`pgp_sym_decrypt` for
    /// Messagerie content (pgcrypto). Dedicated and isolated from
    /// `oauth_encryption_key` per Epic #7 decision #1.
    pub message_encryption_key: String,
    /// Symmetric key passed to `pgp_sym_encrypt`/`pgp_sym_decrypt` for
    /// Google Calendar ICS feed URLs (Epic #9). Dedicated and isolated
    /// from `oauth_encryption_key`/`message_encryption_key`, same
    /// per-secret-key-per-epic pattern as Messagerie.
    pub calendar_feed_encryption_key: String,
    /// In-memory per-family broadcast registry for Messagerie's WS push
    /// (Epic #7 decision #3).
    pub message_hubs: messagerie::MessageHub,
    pub secure_cookies: bool,
    /// MinIO/S3 client for event file attachments (architecture.md epic #10).
    pub storage: Storage,
    /// Second pool, connected as `admin_role` (`BYPASSRLS`). Only ever
    /// touched by handlers gated behind `SuperAdminUser` (Epic #8) — see
    /// `src/user_admin/mod.rs` for why this is a narrow, deliberate
    /// exception to the RLS boundary rather than a general bypass.
    pub admin_db: PgPool,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/auth/register", post(auth::register))
        .route("/auth/verify-email", get(auth::verify_email))
        .route("/auth/verify-email/resend", post(auth::resend_verification))
        .route("/auth/login", post(auth::login))
        .route("/auth/me", get(auth::me))
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
        .route("/account/export", get(rgpd::export_account))
        .route("/privacy-policy", get(rgpd::privacy_policy))
        .route(
            "/groups",
            post(groups::create_group).get(groups::list_groups),
        )
        .route(
            "/groups/:id",
            get(groups::get_group)
                .patch(groups::rename_group)
                .delete(groups::delete_group),
        )
        .route(
            "/groups/:id/transfer-ownership",
            post(groups::transfer_ownership),
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
        .route(
            "/groups/:id/grocery-items/:item_id/price",
            post(budget::entries::set_grocery_item_price),
        )
        .route(
            "/groups/:id/budget-entries",
            post(budget::entries::create_budget_entry).get(budget::entries::list_budget_entries),
        )
        .route(
            "/groups/:id/budget-entries/summary",
            get(budget::entries::budget_summary),
        )
        .route(
            "/groups/:id/budget-entries/:entry_id",
            get(budget::entries::get_budget_entry)
                .patch(budget::entries::update_budget_entry)
                .delete(budget::entries::delete_budget_entry),
        )
        .route(
            "/groups/:id/messages",
            post(messagerie::messages::create_message).get(messagerie::messages::list_messages),
        )
        .route("/groups/:id/messages/ws", get(messagerie::ws::message_ws))
        .route(
            "/groups/:id/messages/:message_id",
            axum::routing::patch(messagerie::messages::update_message)
                .delete(messagerie::messages::delete_message),
        )
        .route(
            "/groups/:id/calendar-imports",
            post(google_calendar::imports::create_calendar_import)
                .get(google_calendar::imports::list_calendar_imports),
        )
        .route(
            "/groups/:id/calendar-imports/:import_id",
            delete(google_calendar::imports::delete_calendar_import),
        )
        .route(
            "/groups/:id/calendar-imports/:import_id/import",
            post(google_calendar::imports::trigger_calendar_import),
        )
        .route("/admin/groups", get(user_admin::admin::list_groups))
        .route("/admin/users", get(user_admin::admin::list_users))
        .route(
            "/admin/users/:id/deactivate",
            post(user_admin::admin::deactivate_user),
        )
        .layer(CookieManagerLayer::new())
        .with_state(state)
}
