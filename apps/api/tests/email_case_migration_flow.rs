//! #459 — `0030_case_insensitive_email.sql` against a real Postgres.
//!
//! Each test migrates a throwaway database up to the version before 0030,
//! writes the rows an older build could have left there, then applies the
//! rest through `LoggingMigrate`, as `migrate` does.

use manage_our_home::migrations::{LoggingMigrate, MIGRATOR};
use sqlx::PgPool;
use uuid::Uuid;

const CASE_INSENSITIVE_EMAIL: i64 = 30;

async fn migrate_to(db: &PgPool, target: Option<i64>) -> Result<(), sqlx::migrate::MigrateError> {
    let mut conn = db.acquire().await.unwrap();
    let mut logging = LoggingMigrate(&mut conn);
    MIGRATOR.run_direct(target, &mut logging, false).await
}

async fn insert_user(db: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, display_name) VALUES ($1, 'x', $1) RETURNING id",
    )
    .bind(email)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn insert_invitation(db: &PgPool, created_by: Uuid, invited_email: &str) {
    let group_id: Uuid =
        sqlx::query_scalar("INSERT INTO groups (name, created_by) VALUES ('G', $1) RETURNING id")
            .bind(created_by)
            .fetch_one(db)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO invitations (group_id, invited_email, created_by, expires_at, token_hash)
         VALUES ($1, $2, $3, now() + interval '7 days',
                 sha256(convert_to(gen_random_uuid()::text, 'UTF8')))",
    )
    .bind(group_id)
    .bind(invited_email)
    .bind(created_by)
    .execute(db)
    .await
    .unwrap();
}

async fn emails(db: &PgPool) -> Vec<String> {
    sqlx::query_scalar(r#"SELECT email FROM users ORDER BY email COLLATE "C""#)
        .fetch_all(db)
        .await
        .unwrap()
}

async fn applied(db: &PgPool, version: i64) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM _sqlx_migrations WHERE version = $1)")
        .bind(version)
        .fetch_one(db)
        .await
        .unwrap()
}

/// Two accounts whose addresses differ only by case: the migration stops,
/// says how many, and changes nothing — no account merged or renamed in
/// silence, and the version not recorded, so it runs again once they are
/// resolved by hand.
#[sqlx::test(migrations = false)]
async fn accounts_equal_but_for_case_stop_the_migration(db: PgPool) {
    migrate_to(&db, Some(CASE_INSENSITIVE_EMAIL - 1))
        .await
        .unwrap();
    insert_user(&db, "Alice@Example.test").await;
    insert_user(&db, "alice@example.test").await;
    insert_user(&db, "ALICE@example.test").await;
    insert_user(&db, "Bob@Example.test").await;
    insert_user(&db, "bob@example.test").await;
    insert_user(&db, "Carol@Example.test").await;

    let err = migrate_to(&db, None)
        .await
        .expect_err("a collision must stop the migration")
        .to_string();
    assert!(
        err.contains("2 address(es) shared by 5 accounts differing only by case"),
        "{err}"
    );

    assert!(!applied(&db, CASE_INSENSITIVE_EMAIL).await);
    assert_eq!(
        emails(&db).await,
        [
            "ALICE@example.test",
            "Alice@Example.test",
            "Bob@Example.test",
            "Carol@Example.test",
            "alice@example.test",
            "bob@example.test",
        ],
        "nothing rewritten"
    );
}

/// Without collision, the stored addresses are lowercased — accounts and
/// invitations — and the database from then on refuses a second account
/// whose address differs only by case.
#[sqlx::test(migrations = false)]
async fn addresses_are_lowercased_and_case_variants_refused(db: PgPool) {
    migrate_to(&db, Some(CASE_INSENSITIVE_EMAIL - 1))
        .await
        .unwrap();
    let alice = insert_user(&db, "Alice@Example.test").await;
    insert_user(&db, "bob@example.test").await;
    insert_invitation(&db, alice, "Invitee@Example.TEST").await;

    migrate_to(&db, None).await.unwrap();

    assert_eq!(
        emails(&db).await,
        ["alice@example.test", "bob@example.test"]
    );
    let invited: Vec<String> = sqlx::query_scalar("SELECT invited_email FROM invitations")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(invited, ["invitee@example.test"]);

    let err = sqlx::query(
        "INSERT INTO users (email, password_hash, display_name) VALUES ('BOB@example.test', 'x', 'b')",
    )
    .execute(&db)
    .await
    .expect_err("a case variant of an existing address must be refused")
    .to_string();
    assert!(err.contains("users_email_lower_key"), "{err}");
}

/// The export's received invitations (0019, new version in 0030) compare
/// without case: an invitation stored with capitals — by a path other than
/// the API, which stores the lowercase form — still reaches the account.
#[sqlx::test]
async fn the_export_finds_an_invitation_stored_with_capitals(db: PgPool) {
    let inviter = insert_user(&db, "inviter@example.test").await;
    let invitee = insert_user(&db, "invitee@example.test").await;
    insert_invitation(&db, inviter, "Invitee@Example.TEST").await;

    let mut tx = manage_our_home::db::begin(&db).await.unwrap();
    sqlx::query("SELECT set_config('app.user_id', $1, true)")
        .bind(invitee.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let received: Vec<String> =
        sqlx::query_scalar("SELECT invited_email FROM account_export_received_invitations()")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    assert_eq!(received, ["Invitee@Example.TEST"]);
    tx.rollback().await.unwrap();
}
