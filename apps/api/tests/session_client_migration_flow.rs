//! #754 — `0052_session_client.sql` against a real Postgres.
//!
//! The database is migrated up to the version before 0052, a session is
//! opened as an older build would have, then the rest is applied through
//! `LoggingMigrate`, as `migrate` does.

use manage_our_home::migrations::{LoggingMigrate, MIGRATOR};
use sqlx::PgPool;
use uuid::Uuid;

const SESSION_CLIENT: i64 = 52;

async fn migrate_to(db: &PgPool, target: Option<i64>) {
    let mut conn = db.acquire().await.unwrap();
    let mut logging = LoggingMigrate(&mut conn);
    MIGRATOR
        .run_direct(target, &mut logging, false)
        .await
        .unwrap();
}

/// The sessions open before the migration have no known client: `NULL`,
/// not a guess. The column then takes `web` and `app`, and nothing else.
#[sqlx::test(migrations = false)]
async fn existing_sessions_have_no_client_and_new_ones_only_web_or_app(db: PgPool) {
    migrate_to(&db, Some(SESSION_CLIENT - 1)).await;
    let user: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, display_name)
         VALUES ('old-session@example.test', 'x', 'Old') RETURNING id",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let insert = "INSERT INTO sessions (user_id, expires_at, token_hash)
                  VALUES ($1, now() + interval '1 day', gen_random_bytes(32))
                  RETURNING id";
    let old: Uuid = sqlx::query_scalar(insert)
        .bind(user)
        .fetch_one(&db)
        .await
        .unwrap();

    migrate_to(&db, None).await;

    let client: Option<String> = sqlx::query_scalar("SELECT client FROM sessions WHERE id = $1")
        .bind(old)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(client, None);

    let with_client = "INSERT INTO sessions (user_id, expires_at, token_hash, client)
                       VALUES ($1, now() + interval '1 day', gen_random_bytes(32), $2)";
    for accepted in ["web", "app"] {
        sqlx::query(with_client)
            .bind(user)
            .bind(accepted)
            .execute(&db)
            .await
            .unwrap_or_else(|e| panic!("{accepted}: {e}"));
    }
    for refused in ["android", "Web", ""] {
        let err = sqlx::query(with_client)
            .bind(user)
            .bind(refused)
            .execute(&db)
            .await
            .expect_err(refused);
        assert!(err.to_string().contains("check"), "{refused}: {err}");
    }
}
