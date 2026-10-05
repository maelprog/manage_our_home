use lettre::message::Mailbox;
use lettre::{AsyncSmtpTransport, Tokio1Executor};
use manage_our_home::email::EmailSender;
use manage_our_home::{build_router, jobs, AppState};
use oauth2::basic::BasicClient;
use oauth2::{AuthUrl, ClientId, ClientSecret, RedirectUrl, TokenUrl};
use std::env;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    // Migrations run first, on their own connection, as their own role
    // (`MIGRATION_DATABASE_URL`) — never on the runtime pool. The runtime
    // role is `NOSUPERUSER NOBYPASSRLS` on any deployment that follows
    // apps/api/README.md, and under it a migration's DML reads its source
    // tables back empty and applies to nothing without saying so
    // (issue #105). `migrations::apply` refuses to start rather than let
    // that happen, and closes the elevated connection before returning.
    manage_our_home::migrations::apply(
        env::var(manage_our_home::migrations::MIGRATION_URL_VAR).ok(),
    )
    .await?;

    let database_url = env::var("DATABASE_URL")?;
    // Both runtime pools drop a connection that comes back with a
    // transaction still open on the server (`db::pool_options`, issue #188).
    let db = manage_our_home::db::pool_options()
        .max_connections(20)
        .connect(&database_url)
        .await?;

    // Local-dev convenience only (infra/.env.example): pre-verified logins
    // so a fresh stack is usable without completing email verification.
    if env::var("DEV_SEED_USERS")
        .map(|v| v == "true")
        .unwrap_or(false)
    {
        manage_our_home::dev_seed::seed_dev_users(&db).await?;
    }

    // Second pool, connected as `admin_role` (`BYPASSRLS`), for the seven
    // superadmin endpoints gated behind `SuperAdminUser` (Epic #8) and the
    // background jobs below: attachment reconcile (#215), account purge
    // (#139), retention purge (#138) and the event reminder worker (#293).
    // See apps/api/README.md for the role-setup snippet.
    let admin_database_url =
        env::var("ADMIN_DATABASE_URL").unwrap_or_else(|_| database_url.clone());
    let admin_db = manage_our_home::db::pool_options()
        .max_connections(5)
        .connect(&admin_database_url)
        .await?;

    let public_base_url =
        env::var("PUBLIC_BASE_URL").unwrap_or_else(|_| "http://localhost:8080".into());
    let frontend_base_url =
        env::var("FRONTEND_BASE_URL").unwrap_or_else(|_| "http://localhost:3000".into());

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

    let smtp_host = env::var("SMTP_HOST")?;
    let smtp_transport = match manage_our_home::email::smtp_mode(
        env::var("SMTP_ALLOW_INSECURE").ok().as_deref(),
        env::var("SMTP_PORT").ok().as_deref(),
    )? {
        manage_our_home::email::SmtpMode::Relay => {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp_host)?
                .credentials(lettre::transport::smtp::authentication::Credentials::new(
                    env::var("SMTP_USERNAME")?,
                    env::var("SMTP_PASSWORD")?,
                ))
                .build()
        }
        manage_our_home::email::SmtpMode::Insecure { port } => {
            tracing::warn!(
                "SMTP_ALLOW_INSECURE=true — plaintext unauthenticated SMTP to {smtp_host}:{port}, local dev only"
            );
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&smtp_host)
                .port(port)
                .build()
        }
    };
    let from_mailbox: Mailbox = env::var("SMTP_FROM")?.parse()?;
    let email = EmailSender::new(smtp_transport, from_mailbox);

    let storage = manage_our_home::storage::Storage::from_env().await?;

    // Refuse to start on a malformed trust list rather than fall back to a
    // default: this list decides whose `X-Forwarded-For` is believed, and a
    // typo either collapses every client onto one throttle bucket or hands
    // the header to a range nobody meant to trust (#178).
    let trusted_proxies = manage_our_home::client_ip::TrustedProxies::from_env()
        .map_err(|e| anyhow::anyhow!("{}: {e}", manage_our_home::client_ip::TRUSTED_PROXIES_VAR))?;

    // Reminders sent as a notification (#306). Unset: notifications are
    // off, and said so; set but unusable: refuse to start.
    let push = manage_our_home::notifications::push::Vapid::from_env()?.map(std::sync::Arc::new);
    if push.is_none() {
        tracing::warn!(
            "{} unset — reminders cannot be sent as notifications on this server",
            manage_our_home::notifications::push::PRIVATE_KEY_VAR
        );
    }

    let state = AppState {
        db,
        google_oauth,
        google_userinfo_url: manage_our_home::auth::oauth_google::GOOGLE_USERINFO_URL.to_string(),
        email: email.clone(),
        public_base_url,
        frontend_base_url,
        oauth_encryption_key: env::var("OAUTH_ENCRYPTION_KEY")?,
        message_encryption_key: env::var("MESSAGE_ENCRYPTION_KEY")?,
        calendar_feed_encryption_key: env::var("CALENDAR_FEED_ENCRYPTION_KEY")?,
        message_hubs: manage_our_home::messagerie::MessageHub::new(),
        message_ws_recheck_interval: std::time::Duration::from_secs(30),
        secure_cookies: env::var("SECURE_COOKIES")
            .map(|v| v == "true")
            .unwrap_or(true),
        storage,
        admin_db,
        trusted_proxies: std::sync::Arc::new(trusted_proxies),
        login_throttle: std::sync::Arc::new(manage_our_home::auth::throttle::LoginThrottle::new()),
        login_branches: std::sync::Arc::new(
            manage_our_home::auth::timing::BranchCounters::default(),
        ),
        body_read_limits: manage_our_home_http_guard::BodyReadLimits::PRODUCTION,
        upload_gate: manage_our_home_http_guard::UploadGate::production(),
        push,
    };

    // On the admin pool: `event_attachments` reads back empty without
    // BYPASSRLS, and the pass refuses to run rather than trust that (#215).
    tokio::spawn(jobs::attachment_reconcile::run(
        state.admin_db.clone(),
        state.storage.clone(),
    ));
    // On the admin pool: five of the tables the purge deletes from are
    // under forced RLS policies, and the pass refuses to run without
    // BYPASSRLS (#139). It also warns the holders of deactivated accounts
    // before their purge (#256) and tells the members who became owners of
    // a group (#323), hence the mailer; and it deletes the attachment
    // objects of the groups a purge leaves with no member (#323), hence the
    // storage.
    tokio::spawn(jobs::account_purge::run(
        state.admin_db.clone(),
        state.email.clone(),
        state.storage.clone(),
        state.frontend_base_url.clone(),
    ));
    // On the admin pool too: `invitations` is under a forced RLS policy,
    // and the pass refuses to run without BYPASSRLS (#138).
    tokio::spawn(jobs::retention_purge::run(state.admin_db.clone()));
    // On the admin pool: `scheduled_notifications` and `events` are under
    // forced RLS policies, and the passes refuse to run without BYPASSRLS
    // (#293).
    tokio::spawn(jobs::scheduled_notifications::run(
        state.admin_db.clone(),
        email,
        state.push.clone(),
    ));

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("listening on {}", listener.local_addr()?);
    // `into_make_service_with_connect_info` is what puts the peer address
    // in each request's extensions; without it `client_ip::resolve` has
    // nothing to check `X-Forwarded-For` against and every client shares
    // one throttle bucket (#178).
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}
