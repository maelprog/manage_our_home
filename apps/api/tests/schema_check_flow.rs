//! #425 — the boot check that replaced the server's own migration pass.
//!
//! The unit tests in `src/migrations.rs` pin the comparison itself
//! (`schema_drift`). These run `ensure_schema_current` against a real
//! `_sqlx_migrations`, so the query it reads the table with, and the
//! "never migrated" path where the table does not exist, are exercised.

use manage_our_home::migrations::{ensure_schema_current, MIGRATOR};
use sqlx::{Connection, PgPool};

/// `sqlx::test` migrates the database with the same embedded migrations
/// the binary carries: the check must accept it, or every deployment that
/// ran `migrate` would still refuse to start.
#[sqlx::test]
async fn a_fully_migrated_database_is_accepted(db: PgPool) {
    let mut conn = db.acquire().await.unwrap();
    ensure_schema_current(&mut conn)
        .await
        .expect("a migrated schema must pass the boot check");
}

/// The case the server must now refuse: a newer build started before its
/// migration was applied. Simulated by forgetting the latest row.
#[sqlx::test]
async fn a_database_behind_the_build_is_refused_by_version(db: PgPool) {
    let latest = MIGRATOR.iter().map(|m| m.version).max().unwrap();
    let mut conn = db.acquire().await.unwrap();
    #[allow(clippy::disallowed_methods)]
    let mut tx = conn.begin().await.unwrap();
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = $1")
        .bind(latest)
        .execute(&mut *tx)
        .await
        .unwrap();

    let err = ensure_schema_current(&mut tx)
        .await
        .expect_err("a pending migration must stop the boot")
        .to_string();
    assert!(
        err.contains(&format!("pending: {latest}")),
        "the refusal must name the pending version: {err}"
    );

    tx.rollback().await.unwrap();
}

/// A migration file edited after it was applied: sqlx's own migrator
/// would refuse it, so the boot check does too.
#[sqlx::test]
async fn an_edited_migration_is_refused(db: PgPool) {
    let first = MIGRATOR.iter().map(|m| m.version).min().unwrap();
    let mut conn = db.acquire().await.unwrap();
    #[allow(clippy::disallowed_methods)]
    let mut tx = conn.begin().await.unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET checksum = '\\x00' WHERE version = $1")
        .bind(first)
        .execute(&mut *tx)
        .await
        .unwrap();

    let err = ensure_schema_current(&mut tx)
        .await
        .expect_err("an edited migration must stop the boot")
        .to_string();
    assert!(
        err.contains(&format!("modified after being applied: {first}")),
        "{err}"
    );

    tx.rollback().await.unwrap();
}

/// A row sqlx recorded with `success = false` — a pass that stopped half
/// way. The check reads the column from the table, not a default.
#[sqlx::test]
async fn a_failed_migration_row_is_refused(db: PgPool) {
    let latest = MIGRATOR.iter().map(|m| m.version).max().unwrap();
    let mut conn = db.acquire().await.unwrap();
    #[allow(clippy::disallowed_methods)]
    let mut tx = conn.begin().await.unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET success = FALSE WHERE version = $1")
        .bind(latest)
        .execute(&mut *tx)
        .await
        .unwrap();

    let err = ensure_schema_current(&mut tx)
        .await
        .expect_err("a failed migration must stop the boot")
        .to_string();
    assert!(err.contains(&format!("failed: {latest}")), "{err}");
    assert!(!err.contains("pending"), "{err}");

    tx.rollback().await.unwrap();
}

/// The database is ahead of this build: a newer image migrated it.
#[sqlx::test]
async fn a_version_unknown_to_this_build_is_refused(db: PgPool) {
    let unknown = MIGRATOR.iter().map(|m| m.version).max().unwrap() + 1;
    let mut conn = db.acquire().await.unwrap();
    #[allow(clippy::disallowed_methods)]
    let mut tx = conn.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES ($1, 'from a newer build', TRUE, '\\x00', 0)",
    )
    .bind(unknown)
    .execute(&mut *tx)
    .await
    .unwrap();

    let err = ensure_schema_current(&mut tx)
        .await
        .expect_err("a version this build does not know must stop the boot")
        .to_string();
    assert!(
        err.contains(&format!("unknown to this build: {unknown}")),
        "{err}"
    );

    tx.rollback().await.unwrap();
}

/// A database nobody migrated has no `_sqlx_migrations` at all. That is
/// "everything pending", reported as such — not a "relation does not
/// exist" error the operator has to decode.
#[sqlx::test(migrations = false)]
async fn a_never_migrated_database_reports_every_migration_pending(db: PgPool) {
    let mut conn = db.acquire().await.unwrap();
    let err = ensure_schema_current(&mut conn)
        .await
        .expect_err("an empty database must stop the boot")
        .to_string();
    let first = MIGRATOR.iter().map(|m| m.version).min().unwrap();
    assert!(err.contains(&format!("pending: {first}, ")), "{err}");
    assert!(err.contains("`migrate`"), "{err}");
}
