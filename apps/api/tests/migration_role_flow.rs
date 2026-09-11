//! #105 — the guard that stands between a migration and a role RLS would
//! filter to nothing.
//!
//! The unit tests in `src/migrations.rs` pin the decision itself. These
//! run it against a real Postgres, and — the load-bearing half — first
//! reproduce the failure it exists to stop: `0013`'s backfill statement,
//! executed by a role shaped exactly as `apps/api/README.md` prescribes
//! for `DATABASE_URL`, reports `INSERT 0 0` over a database that visibly
//! holds an event needing the row.

use manage_our_home::migrations::ensure_migration_role;
use sqlx::PgPool;
use uuid::Uuid;

/// The statement `0013_backfill_event_assignees.sql` applies, verbatim in
/// substance. Copied rather than read from the file: this test is about
/// the shape of migration DML, and it must keep asserting even if that
/// file is one day superseded.
const BACKFILL: &str = "INSERT INTO event_assignees (event_id, user_id)
SELECT e.id, e.created_by
FROM events e
WHERE NOT EXISTS (
    SELECT 1 FROM event_assignees ea WHERE ea.event_id = e.id
)
ON CONFLICT DO NOTHING";

const GRANTED_TABLES: &str = "events, event_assignees, groups, users";

/// Creates a throwaway login-less role with the given attributes and
/// returns a transaction that has already switched to it, so
/// `current_user` — what the guard reads — is that role.
async fn tx_as_role<'a>(
    db: &'a PgPool,
    attributes: &str,
) -> (String, sqlx::Transaction<'a, sqlx::Postgres>) {
    let role = format!("mig_test_role_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE ROLE {role} NOSUPERUSER {attributes}"
    )))
    .execute(db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON {GRANTED_TABLES} TO {role}"
    )))
    .execute(db)
    .await
    .unwrap();

    let mut tx = db.begin().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("SET LOCAL ROLE {role}")))
        .execute(&mut *tx)
        .await
        .unwrap();
    (role, tx)
}

async fn drop_role(db: &PgPool, role: &str) {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "REVOKE ALL ON {GRANTED_TABLES} FROM {role}"
    )))
    .execute(db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP ROLE {role}")))
        .execute(db)
        .await
        .unwrap();
}

/// One group with one event that carries no `event_assignees` row —
/// precisely the state `0013` exists to repair.
async fn seed_event_without_assignee(db: &PgPool) -> Uuid {
    let user: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) \
         VALUES ('mig@example.test', 'x', 'Mig', true) RETURNING id"
    )
    .fetch_one(db)
    .await
    .unwrap();
    let group: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('Mig', $1) RETURNING id",
        user
    )
    .fetch_one(db)
    .await
    .unwrap();
    sqlx::query_scalar!(
        "INSERT INTO events (group_id, created_by, title, starts_at, ends_at) \
         VALUES ($1, $2, 'E', now(), now()) RETURNING id",
        group,
        user
    )
    .fetch_one(db)
    .await
    .unwrap()
}

/// The failure #105 reports, reproduced: a role owning the tables but
/// carrying neither `SUPERUSER` nor `BYPASSRLS` sees zero rows in
/// `events` (no `app.family_id` is set outside an HTTP request), so the
/// backfill's source is empty, nothing is inserted, and nothing says so.
/// The guard is what turns that silence into a refusal.
#[sqlx::test]
async fn backfill_dml_touches_nothing_under_the_role_the_readme_prescribes(db: PgPool) {
    let event = seed_event_without_assignee(&db).await;

    let (role, mut tx) = tx_as_role(&db, "NOBYPASSRLS").await;

    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(visible, 0, "RLS should hide every event from this role");

    let affected = sqlx::query(BACKFILL)
        .execute(&mut *tx)
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(affected, 0, "the backfill silently applies to nothing");

    let err = ensure_migration_role(&mut tx)
        .await
        .expect_err("the guard must refuse this role")
        .to_string();
    assert!(
        err.contains("MIGRATION_DATABASE_URL"),
        "the refusal must name the variable to fix: {err}"
    );

    tx.rollback().await.unwrap();
    drop_role(&db, &role).await;

    // And the row really was missing, i.e. the statement had work to do.
    let assignees: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_assignees WHERE event_id = $1")
            .bind(event)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(assignees, 0);
}

/// The same statement, on a role that bypasses RLS, does the work.
#[sqlx::test]
async fn backfill_dml_applies_under_a_bypassrls_role(db: PgPool) {
    let event = seed_event_without_assignee(&db).await;

    let (role, mut tx) = tx_as_role(&db, "BYPASSRLS").await;

    ensure_migration_role(&mut tx)
        .await
        .expect("the guard must accept a BYPASSRLS role");

    let affected = sqlx::query(BACKFILL)
        .execute(&mut *tx)
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(affected, 1, "the backfill must reach the unassigned event");

    let assigned: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_assignees WHERE event_id = $1")
            .bind(event)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(assigned, 1);

    tx.rollback().await.unwrap();
    drop_role(&db, &role).await;
}

/// The connection the suite itself runs on is a superuser, which the
/// guard must accept — otherwise it would refuse every environment the
/// project actually exercises and nobody would keep it.
#[sqlx::test]
async fn the_test_harness_connection_is_accepted(db: PgPool) {
    let mut conn = db.acquire().await.unwrap();
    ensure_migration_role(&mut conn)
        .await
        .expect("the suite's own superuser connection must pass");
}
