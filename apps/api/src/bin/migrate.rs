//! `migrate` — applies the schema migrations, then exits (#425).
//!
//! The server no longer migrates on boot: it checks that the schema
//! matches the migrations it carries and refuses to start otherwise
//! (`migrations::ensure_schema_current`). This command is what brings the
//! schema up to date, once per deployment, before the server starts — the
//! compose stack's one-shot `migrate` service, a Kubernetes Job. Only it
//! needs the migration role's credentials; the server's pods do not.
//!
//! Same guards as the pass the server used to make: `MIGRATION_DATABASE_URL`
//! is required with no `DATABASE_URL` fallback, and a role that is neither
//! SUPERUSER nor BYPASSRLS is refused before anything is applied (#105).
//! Concurrent runs are serialised by sqlx's advisory lock.
//!
//! ```sh
//! MIGRATION_DATABASE_URL=postgres://migration_role:...@postgres/manage_our_home \
//!   cargo run --bin migrate
//! ```

use manage_our_home::migrations::{apply, MIGRATION_URL_VAR};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    apply(std::env::var(MIGRATION_URL_VAR).ok()).await?;
    tracing::info!("migrations applied, schema up to date");
    Ok(())
}
