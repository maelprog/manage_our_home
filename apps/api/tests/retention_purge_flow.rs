//! End-to-end cover for the retention purge (#138), against a real
//! database.
//!
//! The unit tests in `jobs::retention_purge` pin the durations; what only
//! a real database can show is that each `DELETE` reads the column it
//! should, on the right side of its cutoff, and leaves the rest alone.
//! Every row is seeded one minute on either side of its cutoff.

mod common;

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Duration, Months, Utc};
use common::{
    assert_status, call, drop_prescribed_role, prescribed_role_pool, test_router_with_outbox,
};
use manage_our_home::jobs::retention_purge::{purge, PurgeCounts, RetentionCutoffs};
use sqlx::PgPool;
use uuid::Uuid;

async fn insert_user(db: &PgPool, email: &str, deactivated: bool) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, display_name, deactivated_at)
         VALUES ($1, 'not-a-real-hash', $1, CASE WHEN $2 THEN now() END)
         RETURNING id",
    )
    .bind(email)
    .bind(deactivated)
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

/// Returns the row's `token_hash`, its primary key (#335).
async fn insert_token(db: &PgPool, table: &str, user: Uuid, created: DateTime<Utc>) -> Vec<u8> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {table} (user_id, created_at, expires_at, token_hash)
         VALUES ($1, $2, $2 + interval '1 hour', gen_random_bytes(32)) RETURNING token_hash"
    )))
    .bind(user)
    .bind(created)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn insert_invitation(db: &PgPool, group: Uuid, by: Uuid, created: DateTime<Utc>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO invitations (group_id, invited_email, created_by, created_at, expires_at,
                                  token_hash)
         VALUES ($1, 'tiers@example.test', $2, $3, $3 + interval '7 days',
                 gen_random_bytes(32)) RETURNING id",
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
        "INSERT INTO sessions (user_id, last_seen_at, expires_at, revoked_at, token_hash)
         VALUES ($1, $2, $3, CASE WHEN $4 THEN now() END, gen_random_bytes(32)) RETURNING id",
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

    // #534: the notification centre keeps 30 days, read or not.
    let mut notif_kept = Uuid::nil();
    for (created, read) in [
        (now - Duration::days(30) - minute, false),
        (now - Duration::days(30) + minute, true),
    ] {
        notif_kept = sqlx::query_scalar(
            "INSERT INTO notifications (user_id, group_id, kind, body, created_at, read_at)
             VALUES ($1, $2, 'message', 'Nouveau message.', $3,
                     CASE WHEN $4 THEN now() END)
             RETURNING id",
        )
        .bind(live)
        .bind(group)
        .bind(created)
        .bind(read)
        .fetch_one(&db)
        .await
        .unwrap();
    }

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

    // #427: the login lock's counters live half an hour at most.
    let half_hour = Duration::minutes(30);
    for (byte, started) in [
        ("aa", now - half_hour - minute),
        ("bb", now - half_hour + minute),
    ] {
        sqlx::query(
            "INSERT INTO login_throttle (scope, pair, attempts, window_start)
             VALUES (decode(repeat($1, 32), 'hex'), decode(repeat($1, 32), 'hex'), 1, $2)",
        )
        .bind(byte)
        .bind(started)
        .execute(&db)
        .await
        .unwrap();
    }

    // #428: a sliding-window row goes once its newest admission is ten
    // minutes old (its window in `rate_limit::LIMITS`), whatever older ones it
    // holds; a CSP counter once its quarter-hour window is over.
    let ten_minutes = Duration::minutes(10);
    for (byte, admitted) in [
        ("aa", vec![now - half_hour, now - ten_minutes - minute]),
        ("bb", vec![now - half_hour, now - ten_minutes + minute]),
    ] {
        sqlx::query(
            "INSERT INTO rate_limit_log (kind, bucket, admitted)
             VALUES ('recipe_import', decode(repeat($1, 32), 'hex'), $2)",
        )
        .bind(byte)
        .bind(admitted)
        .execute(&db)
        .await
        .unwrap();
    }
    let quarter_hour = Duration::minutes(15);
    for (byte, started) in [
        ("aa", now - quarter_hour - minute),
        ("bb", now - quarter_hour + minute),
    ] {
        sqlx::query(
            "INSERT INTO csp_report_throttle (scope, requests, window_start)
             VALUES (decode(repeat($1, 32), 'hex'), 1, $2)",
        )
        .bind(byte)
        .bind(started)
        .execute(&db)
        .await
        .unwrap();
    }

    let counts = purge(&db, RetentionCutoffs::at(now)).await.unwrap();

    assert_eq!(
        counts,
        PurgeCounts {
            audit_log: 1,
            email_verification_tokens: 1,
            unverified_accounts: 0,
            password_reset_tokens: 1,
            invitations: 1,
            sessions: 4,
            notifications: 1,
            login_throttle: 1,
            rate_limit_log: 1,
            csp_report_throttle: 1,
        }
    );
    for (table, query) in [
        (
            "rate_limit_log",
            "SELECT encode(bucket, 'hex') FROM rate_limit_log",
        ),
        (
            "csp_report_throttle",
            "SELECT encode(scope, 'hex') FROM csp_report_throttle",
        ),
    ] {
        let left: Vec<String> = sqlx::query_scalar(query).fetch_all(&db).await.unwrap();
        assert_eq!(left, vec!["bb".repeat(32)], "{table}");
    }
    let throttle_left: Vec<String> =
        sqlx::query_scalar("SELECT encode(scope, 'hex') FROM login_throttle")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(throttle_left, vec!["bb".repeat(32)]);
    let audit: Vec<String> = sqlx::query_scalar("SELECT target_id FROM audit_log")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(audit, vec!["recent".to_string()]);
    let verif_left: Vec<Vec<u8>> =
        sqlx::query_scalar("SELECT token_hash FROM email_verification_tokens")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(verif_left, vec![verif_kept]);
    let reset_left: Vec<Vec<u8>> =
        sqlx::query_scalar("SELECT token_hash FROM password_reset_tokens")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(reset_left, vec![reset_kept]);
    assert_eq!(ids(&db, "SELECT id FROM invitations").await, vec![inv_kept]);
    assert_eq!(
        ids(&db, "SELECT id FROM notifications").await,
        vec![notif_kept]
    );
    let mut sessions = ids(&db, "SELECT id FROM sessions").await;
    sessions.sort();
    let mut expected = vec![s_active, s_almost_idle, s_almost_expired];
    expected.sort();
    assert_eq!(sessions, expected);
}

/// #570: each sliding-window limit is purged on its own window. A recipe
/// import row goes once its newest admission is ten minutes old, an Open
/// Food Facts row once its newest read is a minute old; an "account exists"
/// notice row, counted over a day, stays until its newest notice is a day
/// old.
#[sqlx::test]
async fn each_sliding_limit_is_purged_on_its_own_window(db: PgPool) {
    let now = Utc::now();
    let minute = Duration::minutes(1);
    for (kind, byte, newest) in [
        ("recipe_import", "aa", now - Duration::minutes(10) - minute),
        ("recipe_import", "bb", now - Duration::minutes(10) + minute),
        (
            "account_exists_notice",
            "cc",
            now - Duration::days(1) - minute,
        ),
        (
            "account_exists_notice",
            "dd",
            now - Duration::days(1) + minute,
        ),
        ("account_exists_notice", "ee", now - Duration::minutes(11)),
        ("openfoodfacts", "ff", now - Duration::seconds(61)),
        ("openfoodfacts", "ab", now - Duration::seconds(30)),
    ] {
        sqlx::query(
            "INSERT INTO rate_limit_log (kind, bucket, admitted)
             VALUES ($1, decode(repeat($2, 32), 'hex'), $3)",
        )
        .bind(kind)
        .bind(byte)
        .bind(vec![now - Duration::days(2), newest])
        .execute(&db)
        .await
        .unwrap();
    }

    let counts = purge(&db, RetentionCutoffs::at(now)).await.unwrap();

    assert_eq!(counts.rate_limit_log, 3);
    let mut left: Vec<String> = sqlx::query_scalar(
        "SELECT kind || ':' || encode(substr(bucket, 1, 1), 'hex') FROM rate_limit_log",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    left.sort();
    assert_eq!(
        left,
        [
            "account_exists_notice:dd",
            "account_exists_notice:ee",
            "openfoodfacts:ab",
            "recipe_import:bb"
        ]
    );
}

/// `invitations` is under a forced RLS policy: on a role that does not
/// bypass it, its `DELETE` would see no row and the pass would report
/// success. It refuses instead, before deleting anything anywhere.
async fn register(router: &axum::Router, email: &str) {
    let response = call(
        router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({
            "email": email, "password": "a-long-password1", "display_name": "Inscrit",
            "declares_minimum_age": true, "accepts_terms": true
        })),
    )
    .await;
    assert_status(&response, StatusCode::CREATED);
}

/// Dates an account's creation, and its verification tokens with it.
async fn backdate(db: &PgPool, email: &str, created: DateTime<Utc>) -> Uuid {
    let id: Uuid =
        sqlx::query_scalar("UPDATE users SET created_at = $2 WHERE email = $1 RETURNING id")
            .bind(email)
            .bind(created)
            .fetch_one(db)
            .await
            .unwrap();
    sqlx::query("UPDATE email_verification_tokens SET created_at = $2 WHERE user_id = $1")
        .bind(id)
        .bind(created)
        .execute(db)
        .await
        .unwrap();
    id
}

async fn user_ids(db: &PgPool, email: &str) -> Vec<Uuid> {
    sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_all(db)
        .await
        .unwrap()
}

/// #834: an account whose address nobody confirmed goes a week after its
/// creation — a link mailed since does not push that back —, and its
/// address registers again as a new account. A verified account goes
/// nowhere, nor does an unverified one that shows any use or that the
/// account purge owns.
#[sqlx::test]
async fn an_unverified_account_goes_a_week_after_its_creation(db: PgPool) {
    let (router, _outbox) = test_router_with_outbox(db.clone());
    let now = Utc::now();
    let minute = Duration::minutes(1);
    let week = Duration::days(7);
    let long_ago = now - Duration::days(400);

    register(&router, "gone@example.test").await;
    let gone = backdate(&db, "gone@example.test", now - week - minute).await;

    // Created a minute short of the week; its first link is long expired,
    // so a resend mails a fresh one now.
    register(&router, "resent@example.test").await;
    let resent = backdate(&db, "resent@example.test", now - week + minute).await;
    let resend = call(
        &router,
        Method::POST,
        "/auth/verify-email/resend",
        None,
        Some(serde_json::json!({"email": "resent@example.test"})),
    )
    .await;
    assert_status(&resend, StatusCode::OK);
    let fresh_tokens: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM email_verification_tokens WHERE user_id = $1 AND created_at > $2",
    )
    .bind(resent)
    .bind(now - minute)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(fresh_tokens, 1, "the resend mailed no fresh link");

    register(&router, "verified@example.test").await;
    let verified = backdate(&db, "verified@example.test", long_ago).await;
    sqlx::query("UPDATE users SET email_verified = true WHERE id = $1")
        .bind(verified)
        .execute(&db)
        .await
        .unwrap();

    // Unverified and old, but used, or owned by the account purge.
    let google = insert_user(&db, "google@example.test", false).await;
    sqlx::query(
        "INSERT INTO oauth_identities (user_id, provider, provider_user_id)
         VALUES ($1, 'google', 'sub-1')",
    )
    .bind(google)
    .execute(&db)
    .await
    .unwrap();
    let member = insert_user(&db, "member@example.test", false).await;
    let group: Uuid =
        sqlx::query_scalar("INSERT INTO groups (name, created_by) VALUES ('G', $1) RETURNING id")
            .bind(verified)
            .fetch_one(&db)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO group_members (group_id, user_id, role)
         VALUES ($1, $2, 'owner'), ($1, $3, 'standard')",
    )
    .bind(group)
    .bind(verified)
    .bind(member)
    .execute(&db)
    .await
    .unwrap();
    let invited = insert_user(&db, "invited@example.test", false).await;
    let invitation = insert_invitation(&db, group, verified, now).await;
    sqlx::query("UPDATE invitations SET consumed_at = now(), consumed_by = $2 WHERE id = $1")
        .bind(invitation)
        .bind(invited)
        .execute(&db)
        .await
        .unwrap();
    let deactivated = insert_user(&db, "deactivated@example.test", true).await;
    let purged = insert_user(&db, "purged@example.test", false).await;
    sqlx::query("UPDATE users SET deleted_at = now() WHERE id = $1")
        .bind(purged)
        .execute(&db)
        .await
        .unwrap();
    for id in [google, member, invited, deactivated, purged] {
        sqlx::query("UPDATE users SET created_at = $2 WHERE id = $1")
            .bind(id)
            .bind(long_ago)
            .execute(&db)
            .await
            .unwrap();
    }

    let counts = purge(&db, RetentionCutoffs::at(now)).await.unwrap();
    assert_eq!(counts.unverified_accounts, 1);
    assert!(user_ids(&db, "gone@example.test").await.is_empty());
    let mut left = ids(&db, "SELECT id FROM users").await;
    left.sort();
    let mut expected = vec![
        resent,
        verified,
        google,
        member,
        invited,
        deactivated,
        purged,
    ];
    expected.sort();
    assert_eq!(left, expected);

    // Two minutes on, the week is up for `resent` too, however fresh its
    // link: counted from the creation, not from the last mail.
    let later = purge(&db, RetentionCutoffs::at(now + minute * 2))
        .await
        .unwrap();
    assert_eq!(later.unverified_accounts, 1);
    assert!(user_ids(&db, "resent@example.test").await.is_empty());

    // The address is free again: registering it makes a new account.
    register(&router, "gone@example.test").await;
    let again = user_ids(&db, "gone@example.test").await;
    assert_eq!(again.len(), 1);
    assert_ne!(again[0], gone);
    let fresh: (bool, DateTime<Utc>) =
        sqlx::query_as("SELECT email_verified, created_at FROM users WHERE id = $1")
            .bind(again[0])
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(!fresh.0, "a new account starts unverified");
    assert!(fresh.1 > now - minute, "a new account, not the old row");
}

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
