//! `reconcile-attachments` — deletes attachment objects in MinIO that no
//! `event_attachments` row points at (#58).
//!
//! Dry-run by default; `--apply` deletes. Filed as a script rather than a
//! job on purpose: the size of the historic backlog and the rate of the
//! ongoing drip are both unknown, and this is what measures them. If the
//! numbers say it needs a schedule, `src/jobs/` already has the
//! polling-worker shape (see `account_purge.rs`).
//!
//! Run it against a stack:
//!
//! ```sh
//! ADMIN_DATABASE_URL=postgres://admin_role:...@postgres/manage_our_home \
//!   cargo run --bin reconcile-attachments -- --min-age-hours 24
//! ```

use anyhow::Context;
use chrono::Utc;
use manage_our_home::attachment_reconcile::{
    format_summary, parse_args, reconcile, Invocation, Options, HELP,
};
use manage_our_home::storage::Storage;
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match parse_args(&args) {
        Ok(Invocation::Help) => {
            println!("{HELP}");
            return Ok(());
        }
        Ok(Invocation::Run(opts)) => opts,
        Err(e) => {
            eprintln!("{e}\n\n{HELP}");
            std::process::exit(2);
        }
    };

    run(opts).await
}

async fn run(opts: Options) -> anyhow::Result<()> {
    // Deliberately not falling back to `DATABASE_URL` the way `main.rs`
    // does. That fallback is harmless for the superadmin endpoints, which
    // fail visibly without the privilege; here it would silently produce
    // an empty `event_attachments` read — and an empty read means "every
    // object in the bucket is an orphan". Refusing to start is the only
    // safe behaviour when the operator hasn't said which role to use.
    let admin_database_url = std::env::var("ADMIN_DATABASE_URL").context(
        "ADMIN_DATABASE_URL is required and has no DATABASE_URL fallback: \
         event_attachments is FORCE ROW LEVEL SECURITY, so a non-BYPASSRLS \
         connection reads it back empty and every object would look orphaned",
    )?;
    let db = PgPoolOptions::new()
        .max_connections(2)
        .connect(&admin_database_url)
        .await?;
    let mut conn = db.acquire().await?;

    let storage = Storage::from_env()
        .await
        .context("MinIO configuration (MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET)")?;

    let outcome = reconcile(&storage, &mut conn, &opts, Utc::now()).await?;

    println!("{}", format_summary(&outcome.scan, &opts));
    if !outcome.scan.unknown_age.is_empty() {
        println!(
            "\nkept, no LastModified in the listing so their age is unknown:\n  {}",
            outcome.scan.unknown_age.join("\n  ")
        );
    }
    // Printed, not just logged. `reconcile` emits a `tracing` line per key
    // *before* deleting it, which is what survives a pass that dies
    // mid-batch — but that trace is at INFO and disappears under a
    // `RUST_LOG=error`. These are user files and the delete is
    // unrecoverable, so the record of what went can't hinge on a log level.
    if !outcome.deleted.is_empty() {
        println!(
            "\ndeleted {} object(s):\n  {}",
            outcome.deleted.len(),
            outcome.deleted.join("\n  ")
        );
    } else if opts.apply {
        println!("\ndeleted nothing");
    } else if !outcome.scan.orphans.is_empty() {
        println!(
            "\nwould delete:\n  {}",
            outcome.scan.orphan_keys().join("\n  ")
        );
    }

    Ok(())
}
