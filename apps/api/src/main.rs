use lettre::message::Mailbox;
use lettre::{AsyncSmtpTransport, Tokio1Executor};
use manage_our_home::email::EmailSender;
use manage_our_home::{build_router, jobs, AppState};
use oauth2::basic::BasicClient;
use oauth2::{AuthUrl, ClientId, ClientSecret, RedirectUrl, TokenUrl};
use sqlx::postgres::PgPoolOptions;
use std::env;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let database_url = env::var("DATABASE_URL")?;
    let db = PgPoolOptions::new()
        .max_connections(20)
        .connect(&database_url)
        .await?;
    sqlx::migrate!("./migrations").run(&db).await?;

    // Second pool, connected as `admin_role` (`BYPASSRLS`), exclusively for
    // the three superadmin endpoints gated behind `SuperAdminUser` (Epic
    // #8). See apps/api/README.md for the role-setup snippet.
    let admin_database_url =
        env::var("ADMIN_DATABASE_URL").unwrap_or_else(|_| database_url.clone());
    let admin_db = PgPoolOptions::new()
        .max_connections(5)
        .connect(&admin_database_url)
        .await?;

    let public_base_url =
        env::var("PUBLIC_BASE_URL").unwrap_or_else(|_| "http://localhost:8080".into());
    let frontend_base_url =
        env::var("FRONTEND_BASE_URL").unwrap_or_else(|_| "http://localhost:5173".into());

    let google_oauth = BasicClient::new(ClientId::new(env::var("GOOGLE_CLIENT_ID")?))
        .set_client_secret(ClientSecret::new(env::var("GOOGLE_CLIENT_SECRET")?))
        .set_auth_uri(AuthUrl::new(
            "https://accounts.google.com/o/oauth2/v2/auth".to_string(),
        )?)
        .set_token_uri(TokenUrl::new(
            "https://oauth2.googleapis.com/token".to_string(),
        )?)
        .set_redirect_uri(RedirectUrl::new(format!(
            "{public_base_url}/auth/google/callback"
        ))?);

    let smtp_transport = AsyncSmtpTransport::<Tokio1Executor>::relay(&env::var("SMTP_HOST")?)?
        .credentials(lettre::transport::smtp::authentication::Credentials::new(
            env::var("SMTP_USERNAME")?,
            env::var("SMTP_PASSWORD")?,
        ))
        .build();
    let from_mailbox: Mailbox = env::var("SMTP_FROM")?.parse()?;
    let email = EmailSender::new(smtp_transport, from_mailbox);

    let storage = manage_our_home::storage::Storage::from_env().await?;

    let state = AppState {
        db: db.clone(),
        google_oauth,
        email: email.clone(),
        public_base_url,
        frontend_base_url,
        oauth_encryption_key: env::var("OAUTH_ENCRYPTION_KEY")?,
        message_encryption_key: env::var("MESSAGE_ENCRYPTION_KEY")?,
        message_hubs: manage_our_home::messagerie::MessageHub::new(),
        secure_cookies: env::var("SECURE_COOKIES")
            .map(|v| v == "true")
            .unwrap_or(true),
        storage,
        admin_db,
    };

    tokio::spawn(jobs::account_purge::run(db.clone()));
    tokio::spawn(jobs::scheduled_notifications::run(db, email));

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("listening on {}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}
