//! #154 — the row counts of a migration's statements, against a real
//! Postgres.
//!
//! The unit tests in `src/migrations.rs` pin the rendering. These pin the
//! two things only a database can answer:
//!
//! 1. **The bookkeeping.** `LoggingMigrate` replaces `sqlx-postgres`'s own
//!    `Migrate::apply`, and that method is where sqlx keeps the invariant
//!    of <https://github.com/launchbadge/sqlx/issues/1966>: the migration
//!    SQL and the `_sqlx_migrations` row are one transaction, so a
//!    migration is never recorded without its effects nor applied without
//!    its record. Getting that wrong replays a migration. Everything else
//!    in this file is secondary to it.
//! 2. **The counts themselves.** One count per `CommandComplete`, in
//!    execution order — which is what separates a backfill that matched
//!    nothing from the `CREATE TABLE` next to it.

use manage_our_home::migrations::LoggingMigrate;
use sqlx::migrate::{Migrate, Migration, MigrationType};
use sqlx::{PgPool, SqlStr};

/// A bookkeeping table of this test's own, so assertions count rows this
/// file wrote and not the ones `#[sqlx::test]` left behind.
const TABLE: &str = "_sqlx_migrations_154";

fn migration(version: i64, description: &'static str, sql: &'static str, no_tx: bool) -> Migration {
    Migration::new(
        version,
        description.into(),
        MigrationType::Simple,
        SqlStr::from_static(sql),
        no_tx,
    )
}

async fn recorded_versions(db: &PgPool) -> Vec<i64> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT version FROM {TABLE} ORDER BY version"
    )))
    .fetch_all(db)
    .await
    .unwrap()
}

async fn table_exists(db: &PgPool, name: &str) -> bool {
    sqlx::query_scalar::<_, bool>("SELECT to_regclass($1) IS NOT NULL")
        .bind(name)
        .fetch_one(db)
        .await
        .unwrap()
}

/// The load-bearing one: a migration that fails partway leaves **no**
/// trace — neither its effects nor its `_sqlx_migrations` row.
///
/// If the row survived a rolled-back migration, the pass would record
/// work that never happened and never retry it. If the effects survived
/// without the row, the next pass would replay them. Either way it is
/// sqlx#1966, and either way it is silent.
#[sqlx::test]
async fn a_failed_migration_records_nothing_and_changes_nothing(db: PgPool) {
    let mut conn = db.acquire().await.unwrap();
    let mut wrapped = LoggingMigrate(&mut conn);
    wrapped.ensure_migrations_table(TABLE).await.unwrap();

    let failing = migration(
        1,
        "half a migration",
        "CREATE TABLE mig154_fail (id int); INSERT INTO mig154_fail VALUES (1); SELECT 1 / 0;",
        false,
    );
    let err = wrapped
        .apply(TABLE, &failing)
        .await
        .expect_err("division by zero must fail the migration");
    assert!(
        err.to_string().contains("division by zero"),
        "the driver error must reach the caller: {err}"
    );
    drop(conn);

    assert!(
        recorded_versions(&db).await.is_empty(),
        "a rolled-back migration must not be recorded as applied"
    );
    assert!(
        !table_exists(&db, "mig154_fail").await,
        "the migration's own DDL must have rolled back with it"
    );
}

/// The other half of the same invariant: a migration that succeeds is
/// recorded exactly once, with the fields sqlx's own reader expects back.
///
/// `list_applied_migrations` and `dirty_version` here are
/// `sqlx-postgres`'s implementations, not ours — they are what
/// `Migrator::run_direct` consults on the next pass to decide whether to
/// replay. A version or checksum we wrote differently would surface there
/// as a replay or a `VersionMismatch`.
#[sqlx::test]
async fn a_successful_migration_is_recorded_once_and_read_back_by_sqlx(db: PgPool) {
    let mut conn = db.acquire().await.unwrap();
    let mut wrapped = LoggingMigrate(&mut conn);
    wrapped.ensure_migrations_table(TABLE).await.unwrap();

    let first = migration(1, "setup", "CREATE TABLE mig154_probe (tag text);", false);
    let second = migration(
        2,
        "seed",
        "INSERT INTO mig154_probe (tag) VALUES ('a'), ('b');",
        false,
    );
    wrapped.apply(TABLE, &first).await.unwrap();
    wrapped.apply(TABLE, &second).await.unwrap();

    assert_eq!(wrapped.dirty_version(TABLE).await.unwrap(), None);
    let applied = wrapped.list_applied_migrations(TABLE).await.unwrap();
    assert_eq!(
        applied.iter().map(|m| m.version).collect::<Vec<_>>(),
        vec![1, 2],
        "each migration must be recorded exactly once, in order"
    );
    assert_eq!(
        applied[0].checksum, first.checksum,
        "the checksum must round-trip, or the next pass reports a version mismatch"
    );
    drop(conn);

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM mig154_probe")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(rows, 2, "the migration's effects must be committed");

    // sqlx writes `-1` inside the transaction and overwrites it after the
    // commit. A value still at -1 would mean that second statement never
    // ran.
    let times: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT execution_time FROM {TABLE} ORDER BY version"
    )))
    .fetch_all(&db)
    .await
    .unwrap();
    assert!(
        times.iter().all(|t| *t >= 0),
        "execution_time must be filled in after the commit: {times:?}"
    );
}

/// A migration declared `-- no-transaction` runs, and is recorded,
/// outside any transaction. The repository has none today; the branch
/// exists because sqlx has it, and an untested branch of a migration
/// applier is discovered in production.
#[sqlx::test]
async fn a_no_tx_migration_applies_and_is_recorded(db: PgPool) {
    let mut conn = db.acquire().await.unwrap();
    let mut wrapped = LoggingMigrate(&mut conn);
    wrapped.ensure_migrations_table(TABLE).await.unwrap();

    let untransacted = migration(7, "no tx", "CREATE TABLE mig154_notx (id int);", true);
    wrapped.apply(TABLE, &untransacted).await.unwrap();
    drop(conn);

    assert_eq!(recorded_versions(&db).await, vec![7]);
    assert!(table_exists(&db, "mig154_notx").await);
}

/// What #154 asked for: one count per statement, in order, including the
/// zeroes.
///
/// The third statement is the failure the issue describes — a `WHERE`
/// that matches nothing. It succeeds, it is recorded, and the only thing
/// that says so is the `0` in this list.
#[sqlx::test]
async fn every_statement_reports_its_own_row_count(db: PgPool) {
    let mut conn = db.acquire().await.unwrap();
    let mut wrapped = LoggingMigrate(&mut conn);
    wrapped.ensure_migrations_table(TABLE).await.unwrap();

    let mixed = migration(
        1,
        "ddl then dml then a typo",
        "CREATE TABLE mig154_counts (tag text); \
         INSERT INTO mig154_counts (tag) VALUES ('a'), ('b'), ('c'); \
         UPDATE mig154_counts SET tag = 'z' WHERE tag = 'typoo';",
        false,
    );
    let (_, summary) = wrapped.apply_logging(TABLE, &mixed).await.unwrap();

    assert_eq!(summary.statements, 3);
    assert_eq!(summary.rows_affected, 3);
    assert_eq!(
        summary.per_statement, "[0, 3, 0]",
        "the DDL's 0, the insert's 3, and the backfill that matched nothing"
    );
}

/// The limit, asserted rather than claimed: DML inside a `DO` block, a
/// function or a trigger is one statement to the protocol, and its row
/// count never reaches us. A migration that hides its backfill in
/// PL/pgSQL is as silent after this change as before it.
#[sqlx::test]
async fn dml_inside_plpgsql_is_invisible_to_the_wrapper(db: PgPool) {
    let mut conn = db.acquire().await.unwrap();
    let mut wrapped = LoggingMigrate(&mut conn);
    wrapped.ensure_migrations_table(TABLE).await.unwrap();

    let buried = migration(
        1,
        "backfill in a DO block",
        "CREATE TABLE mig154_plpgsql (tag text); \
         DO $$ BEGIN INSERT INTO mig154_plpgsql (tag) VALUES ('a'), ('b'); END $$;",
        false,
    );
    let (_, summary) = wrapped.apply_logging(TABLE, &buried).await.unwrap();

    assert_eq!(summary.statements, 2);
    assert_eq!(
        summary.per_statement, "[0, 0]",
        "the DO block reports 0 although it inserted 2 rows"
    );
    drop(conn);

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM mig154_plpgsql")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(rows, 2, "the rows really were inserted");
}

/// End to end, over the repository's own migrations and the rows sqlx
/// itself wrote: `#[sqlx::test]` migrates the throwaway database through
/// `sqlx-postgres`'s `Migrate`, and the same set replayed through the
/// wrapper must find everything applied and apply nothing.
///
/// This is what pins the interoperability. A version, checksum or table
/// name we accounted for differently would show up here as a replay or a
/// `VersionMismatch`, not as a subtle drift noticed months later.
#[sqlx::test]
async fn the_repository_migrations_replay_through_the_wrapper_as_a_no_op(db: PgPool) {
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(before > 0, "the harness database must already be migrated");

    let mut conn = db.acquire().await.unwrap();
    let mut wrapped = LoggingMigrate(&mut conn);
    sqlx::migrate!("./migrations")
        .run_direct(None, &mut wrapped, false)
        .await
        .expect("an already-migrated database must replay cleanly");
    drop(conn);

    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(after, before, "nothing may be applied or recorded twice");
}
