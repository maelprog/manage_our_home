mod app;
mod family;
mod layout;
mod routes;
mod state;

use axum::routing::{get, post};
use axum::Router;

use state::AppState;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let api_internal_base_url = std::env::var("API_INTERNAL_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    let api_public_base_url =
        std::env::var("API_PUBLIC_BASE_URL").unwrap_or_else(|_| "/api".to_string());
    let bind_addr = std::env::var("WEB_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());

    let state = AppState {
        http: reqwest::Client::new(),
        api_internal_base_url,
        api_public_base_url,
    };

    let app = Router::new()
        .route("/", get(routes::home::get))
        .route("/logout", post(routes::home::logout))
        .route("/auth/google/callback", get(routes::home::google_callback))
        .route(
            "/register",
            get(routes::auth::register::get).post(routes::auth::register::post),
        )
        .route(
            "/register/check-email",
            get(routes::auth::register::check_email),
        )
        .route("/verify-email", get(routes::auth::verify_email::get))
        .route(
            "/login",
            get(routes::auth::login::get).post(routes::auth::login::post),
        )
        .route(
            "/forgot-password",
            get(routes::auth::forgot_password::get).post(routes::auth::forgot_password::post),
        )
        .route(
            "/reset-password",
            get(routes::auth::reset_password::get).post(routes::auth::reset_password::post),
        )
        // RGPD self-service (front epic F10): the account hub, the export
        // download, and the grace-period deletion flow. `/privacy-policy` is the
        // one page here that needs no session — it must be readable before
        // registering (linked from the login/register footers).
        .route("/privacy-policy", get(routes::privacy::get))
        .route("/account", get(routes::account::get))
        .route("/account/export", get(routes::account::export::get))
        .route(
            "/account/export/download",
            get(routes::account::export::download),
        )
        .route(
            "/account/delete",
            get(routes::account::delete::get).post(routes::account::delete::post),
        )
        .route(
            "/account/delete/cancel",
            post(routes::account::delete::cancel),
        )
        .route("/agenda", get(routes::agenda::calendar::get))
        .route(
            "/agenda/new",
            get(routes::agenda::new::get).post(routes::agenda::new::post),
        )
        .route("/agenda/:id", get(routes::agenda::detail::get))
        .route(
            "/agenda/:id/edit",
            get(routes::agenda::edit::get).post(routes::agenda::edit::post),
        )
        .route("/agenda/:id/delete", post(routes::agenda::detail::delete))
        .route(
            "/agenda/:id/complete",
            post(routes::agenda::detail::complete),
        )
        .route(
            "/agenda/:id/reminders",
            post(routes::agenda::reminders::add),
        )
        .route(
            "/agenda/:id/reminders/:rid/delete",
            post(routes::agenda::reminders::delete),
        )
        .route(
            "/agenda/:id/attachments",
            post(routes::agenda::attachments::upload),
        )
        .route(
            "/agenda/:id/attachments/:aid/download",
            get(routes::agenda::attachments::download),
        )
        .route(
            "/agenda/:id/attachments/:aid/delete",
            post(routes::agenda::attachments::delete),
        )
        .route("/stocks", get(routes::stocks::list::get))
        .route(
            "/stocks/new",
            get(routes::stocks::new::get).post(routes::stocks::new::post),
        )
        .route("/stocks/:id", get(routes::stocks::detail::get))
        .route(
            "/stocks/:id/edit",
            get(routes::stocks::edit::get).post(routes::stocks::edit::post),
        )
        .route("/stocks/:id/adjust", post(routes::stocks::detail::adjust))
        .route("/stocks/:id/delete", post(routes::stocks::detail::delete))
        .route("/recipes", get(routes::recipes::list::get))
        .route(
            "/recipes/new",
            get(routes::recipes::new::get).post(routes::recipes::new::post),
        )
        .route("/recipes/:id", get(routes::recipes::detail::get))
        .route(
            "/recipes/:id/edit",
            get(routes::recipes::edit::get).post(routes::recipes::edit::post),
        )
        .route("/recipes/:id/log", post(routes::recipes::detail::log))
        .route("/recipes/:id/delete", post(routes::recipes::detail::delete))
        .route("/grocery-list", get(routes::grocery_list::list::get))
        .route("/grocery-list/add", post(routes::grocery_list::list::add))
        .route(
            "/grocery-list/generate",
            post(routes::grocery_list::list::generate),
        )
        .route("/grocery-list/:id", get(routes::grocery_list::edit::get))
        .route(
            "/grocery-list/:id/check",
            post(routes::grocery_list::list::check),
        )
        .route(
            "/grocery-list/:id/edit",
            post(routes::grocery_list::edit::post),
        )
        .route(
            "/grocery-list/:id/delete",
            post(routes::grocery_list::edit::delete),
        )
        .route(
            "/grocery-list/:id/price",
            post(routes::grocery_list::list::price),
        )
        .route("/budget", get(routes::budget::list::get))
        .route(
            "/budget/new",
            get(routes::budget::new::get).post(routes::budget::new::post),
        )
        .route("/budget/:id", get(routes::budget::edit::get))
        .route("/budget/:id/edit", post(routes::budget::edit::post))
        .route("/budget/:id/delete", post(routes::budget::edit::delete))
        .route(
            "/messagerie",
            get(routes::messagerie::thread::get).post(routes::messagerie::thread::post),
        )
        .route(
            "/messagerie/:id/edit",
            post(routes::messagerie::thread::edit),
        )
        .route(
            "/messagerie/:id/delete",
            post(routes::messagerie::thread::delete),
        )
        .route("/admin/groups", get(routes::admin::groups::get))
        .route("/admin/users", get(routes::admin::users::get))
        .route("/admin/users/:id", get(routes::admin::users::detail))
        .route(
            "/admin/users/:id/deactivate",
            post(routes::admin::users::deactivate),
        )
        .route("/groups", get(routes::groups::list::get))
        .route("/groups/join", post(routes::groups::list::join))
        .route("/groups/switch", post(routes::groups::switch))
        .route(
            "/groups/new",
            get(routes::groups::new::get).post(routes::groups::new::post),
        )
        .route(
            "/groups/invitations/:token/accept",
            get(routes::groups::invitations::get).post(routes::groups::invitations::post),
        )
        .route("/groups/:id/members", get(routes::groups::members::get))
        .route(
            "/groups/:id/members/invite",
            post(routes::groups::members::invite),
        )
        .route(
            "/groups/:id/members/:user_id/role",
            post(routes::groups::members::change_role),
        )
        .route(
            "/groups/:id/members/:user_id/remove",
            post(routes::groups::members::remove),
        )
        .route("/groups/:id/settings", get(routes::groups::settings::get))
        .route(
            "/groups/:id/settings/rename",
            post(routes::groups::settings::rename),
        )
        .route(
            "/groups/:id/settings/transfer",
            post(routes::groups::settings::transfer),
        )
        .route(
            "/groups/:id/settings/leave",
            post(routes::groups::settings::leave),
        )
        .route(
            "/groups/:id/settings/delete",
            post(routes::groups::settings::delete),
        )
        .with_state(state);

    tracing::info!(%bind_addr, "starting manage_our_home_web");
    let listener = tokio::net::TcpListener::bind(&bind_addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
