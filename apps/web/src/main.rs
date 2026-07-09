mod app;
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
    let api_public_base_url = std::env::var("API_PUBLIC_BASE_URL")
        .unwrap_or_else(|_| "/api".to_string());
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
        .route("/register/check-email", get(routes::auth::register::check_email))
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
        .with_state(state);

    tracing::info!(%bind_addr, "starting manage_our_home_web");
    let listener = tokio::net::TcpListener::bind(&bind_addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
