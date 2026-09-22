//! End-to-end cover for the retention purge (#138), against a real
//! database.
//!
//! The unit tests in `jobs::retention_purge` pin the durations; what only
//! a real database can show is that each `DELETE` reads the column it
//! should, on the right side of its cutoff, and leaves the rest alone.
//! Every row is seeded one minute on either side of its cutoff.

mod common;

use chrono::{DateTime, Duration, Months, Utc};
use common::{drop_prescribed_role, prescribed_role_pool};
use manage_our_home::jobs::retention_purge::{purge, PurgeCounts, RetentionCutoffs};
use sqlx::PgPool;
use uuid::Uuid;

async fn insert_user(db: &PgPool, email: &str, deleted: bool) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, display_name, deleted_at)
         VALUES ($1, 'not-a-real-hash', $1, CASE WHEN $2 THEN now() END)
         RETURNING id",
    )
    .bind(email)
    .bind(deleted)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn insert_audit(db: &PgPool, at: DateTime<Utc>, target: &str) {
    sqlx::query(
        "INSERT INTO audit_log (occurred_at, action, target_type, target_id)
         VALUES ($1, 'test', 'test', $2)",
    )
    .bind(at)
    .bind(target)
    .execute(db)
    .await
    .unwrap();
}

async fn insert_token(db: &PgPool, table: &str, user: Uuid, created: DateTime<Utc>) -> Uuid {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {table} (user_id, created_at, expires_at)
         VALUES ($1, $2, $2 + interval '1 hour') RETURNING token"
    )))
    .bind(user)
    .bind(created)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn insert_invitation(db: &PgPool, group: Uuid, by: Uuid, created: DateTime<Utc>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO invitations (group_id, invited_email, created_by, created_at, expires_at)
         VALUES ($1, 'tiers@example.test', $2, $3, $3 + interval '7 days') RETURNING id",
    )
    .bind(group)
    .bind(by)
    .bind(created)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn insert_session(
    db: &PgPool,
    user: Uuid,
    last_seen: DateTime<Utc>,
    expires: DateTime<Utc>,
    revoked: bool,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO sessions (user_id, last_seen_at, expires_at, revoked_at)
         VALUES ($1, $2, $3, CASE WHEN $4 THEN now() END) RETURNING id",
    )
    .bind(user)
    .bind(last_seen)
    .bind(expires)
    .bind(revoked)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn ids(db: &PgPool, sql: &'static str) -> Vec<Uuid> {
    sqlx::query_scalar(sql).fetch_all(db).await.unwrap()
}

#[sqlx::test]
async fn each_table_loses_exactly_the_rows_past_their_retention(db: PgPool) {
    let now = Utc::now();
    let minute = Duration::minutes(1);
    let live = insert_user(&db, "live@example.test", false).await;
    let deactivated = insert_user(&db, "deactivated@example.test", true).await;
    let group: Uuid =
        sqlx::query_scalar("INSERT INTO groups (name, created_by) VALUES ('G', $1) RETURNING id")
            .bind(live)
            .fetch_one(&db)
            .await
            .unwrap();

    let six_months_ago = now.checked_sub_months(Months::new(6)).unwrap();
    insert_audit(&db, six_months_ago - minute, "old").await;
    insert_audit(&db, six_months_ago + minute, "recent").await;

    insert_token(
        &db,
        "email_verification_tokens",
        live,
        now - Duration::hours(48) - minute,
    )
    .await;
    let verif_kept = insert_token(
        &db,
        "email_verification_tokens",
        live,
        now - Duration::hours(48) + minute,
    )
    .await;
    insert_token(
        &db,
        "password_reset_tokens",
        live,
        now - Duration::hours(1) - minute,
    )
    .await;
    let reset_kept = insert_token(
        &db,
        "password_reset_tokens",
        live,
        now - Duration::hours(1) + minute,
    )
    .await;

    insert_invitation(&db, group, live, now - Duration::days(30) - minute).await;
    let inv_kept = insert_invitation(&db, group, live, now - Duration::days(30) + minute).await;

    let far = now + Duration::days(20);
    let idle_edge = now - Duration::days(7);
    // Gone: revoked, expired, idle past the timeout, deactivated account.
    insert_session(&db, live, now, far, true).await;
    insert_session(&db, live, now - Duration::hours(2), now - minute, false).await;
    insert_session(&db, live, idle_edge - minute, far, false).await;
    insert_session(&db, deactivated, now, far, false).await;
    // Kept: active, one minute short of idle, one minute short of expiry.
    let s_active = insert_session(&db, live, now, far, false).await;
    let s_almost_idle = insert_session(&db, live, idle_edge + minute, far, false).await;
    let s_almost_expired = insert_session(&db, live, now, now + minute, false).await;

    let counts = purge(&db, RetentionCutoffs::at(now)).await.unwrap();

    assert_eq!(
        counts,
        PurgeCounts {
            audit_log: 1,
            email_verification_tokens: 1,
            password_reset_tokens: 1,
            invitations: 1,
            sessions: 4,
        }
    );
    let audit: Vec<String> = sqlx::query_scalar("SELECT target_id FROM audit_log")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(audit, vec!["recent".to_string()]);
    assert_eq!(
        ids(&db, "SELECT token FROM email_verification_tokens").await,
        vec![verif_kept]
    );
    assert_eq!(
        ids(&db, "SELECT token FROM password_reset_tokens").await,
        vec![reset_kept]
    );
    assert_eq!(ids(&db, "SELECT id FROM invitations").await, vec![inv_kept]);
    let mut sessions = ids(&db, "SELECT id FROM sessions").await;
    sessions.sort();
    let mut expected = vec![s_active, s_almost_idle, s_almost_expired];
    expected.sort();
    assert_eq!(sessions, expected);
}

/// `invitations` is under a forced RLS policy: on a role that does not
/// bypass it, its `DELETE` would see no row and the pass would report
/// success. It refuses instead, before deleting anything anywhere.
#[sqlx::test]
async fn the_pass_refuses_a_role_that_does_not_bypass_rls(db: PgPool) {
    let now = Utc::now();
    insert_audit(&db, now - Duration::days(400), "old").await;
    let (role, app_db) = prescribed_role_pool(&db).await;

    let result = purge(&app_db, RetentionCutoffs::at(now)).await;
    drop_prescribed_role(&db, app_db, &role).await;

    assert!(result.is_err(), "{result:?}");
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(left, 1);
}
