//! End-to-end cover for the orphaned-attachment reconciliation pass (#58),
//! against a real MinIO and a real database.
//!
//! The unit tests in `attachment_reconcile` pin the classification itself;
//! what only a real stack can show is that the pass reads the bucket and
//! the table it thinks it does, deletes the object it named, and leaves
//! everything else alone.
//!
//! Every test here scopes the pass with `--prefix <group-id>/` (keys are
//! `{group_id}/{event_id}/{uuid}`). The bucket is shared across the whole
//! suite while `sqlx::test` gives each test its own database, so an
//! unscoped pass would see every *other* test's objects as orphans —
//! exactly the failure mode the prefix flag exists for.

mod common;

use axum::http::{Method, StatusCode};
use chrono::{Duration, Utc};
use common::{
    assert_status, call, call_upload, json_body, real_minio_from_env, set_cookie,
    test_router_with_storage, test_state,
};
use manage_our_home::attachment_reconcile::{reconcile, Options, DEFAULT_MIN_AGE_HOURS};
use manage_our_home::storage::Storage;
use sqlx::PgPool;
use uuid::Uuid;

const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";

async fn register_verify_login(router: &axum::Router, db: &PgPool, email: &str) -> String {
    call(
        router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({
            "email": email, "password": "owner-password1", "display_name": email
        })),
    )
    .await;
    let token = sqlx::query_scalar!(
        "SELECT token FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = $1",
        email
    )
    .fetch_one(db)
    .await
    .unwrap();
    call(
        router,
        Method::GET,
        &format!("/auth/verify-email?token={token}"),
        None,
        None,
    )
    .await;
    let login = call(
        router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": email, "password": "owner-password1"})),
    )
    .await;
    set_cookie(&login).unwrap()
}

/// A group with one event carrying two uploaded attachments, and the two
/// storage keys behind them.
struct Fixture {
    group_id: String,
    keys: Vec<String>,
}

async fn two_attachments(router: &axum::Router, db: &PgPool, cookie: &str) -> Fixture {
    let create_group = call(
        router,
        Method::POST,
        "/groups",
        Some(cookie),
        Some(serde_json::json!({"name": "Foyer"})),
    )
    .await;
    assert_status(&create_group, StatusCode::CREATED);
    let group_id = json_body(create_group).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let starts_at = Utc::now() + Duration::days(1);
    let create_event = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(cookie),
        Some(serde_json::json!({
            "title": "Rendez-vous médecin",
            "starts_at": starts_at,
            "ends_at": starts_at + Duration::hours(1),
        })),
    )
    .await;
    assert_status(&create_event, StatusCode::CREATED);
    let event_id = json_body(create_event).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    for filename in ["ordonnance.png", "analyse.png"] {
        let upload = call_upload(
            router,
            &format!("/groups/{group_id}/events/{event_id}/attachments"),
            cookie,
            filename,
            PNG_BYTES,
        )
        .await;
        assert_status(&upload, StatusCode::CREATED);
    }

    let keys = sqlx::query_scalar!(
        "SELECT storage_key FROM event_attachments WHERE event_id = $1 ORDER BY filename",
        Uuid::parse_str(&event_id).unwrap(),
    )
    .fetch_all(db)
    .await
    .unwrap();
    assert_eq!(keys.len(), 2);

    Fixture { group_id, keys }
}

/// Drops the metadata row while leaving the object in place — the exact
/// state every pre-#56 event deletion left behind, and the one nothing
/// could find afterwards.
async fn orphan_the_object(db: &PgPool, key: &str) {
    sqlx::query!("DELETE FROM event_attachments WHERE storage_key = $1", key)
        .execute(db)
        .await
        .unwrap();
}

async fn exists(s3: &aws_sdk_s3::Client, bucket: &str, key: &str) -> bool {
    s3.head_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .is_ok()
}

fn options(group_id: &str, apply: bool, min_age_hours: i64) -> Options {
    Options {
        apply,
        min_age_hours,
        prefix: Some(format!("{group_id}/")),
    }
}

/// AC (#58): the pass deletes the object nothing points at, and only that
/// one. `min_age_hours: 0` with `now` nudged forward puts the just-uploaded
/// objects past the cutoff without the test having to sleep out a real
/// in-flight window.
#[sqlx::test]
async fn the_pass_deletes_an_orphan_and_leaves_the_live_object_alone(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping the_pass_deletes_an_orphan_and_leaves_the_live_object_alone: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let storage = Storage::new(s3.clone(), bucket.clone());
    let router = test_router_with_storage(db.clone(), storage.clone());
    let cookie = register_verify_login(&router, &db, "reconcile-a@example.test").await;
    let fixture = two_attachments(&router, &db, &cookie).await;
    let (orphan, live) = (&fixture.keys[0], &fixture.keys[1]);

    orphan_the_object(&db, orphan).await;

    let mut conn = db.acquire().await.unwrap();
    let outcome = reconcile(
        &storage,
        &mut conn,
        &options(&fixture.group_id, true, 0),
        Utc::now() + Duration::minutes(5),
    )
    .await
    .unwrap();

    assert_eq!(outcome.deleted, vec![orphan.clone()]);
    assert_eq!(outcome.scan.matched, 1);
    assert!(
        !exists(&s3, &bucket, orphan).await,
        "the orphaned object should be gone"
    );
    assert!(
        exists(&s3, &bucket, live).await,
        "the object a row still points at must survive"
    );
}

/// The default is harmless: an orphan is reported, counted, and left on
/// disk. These are user files and the delete is unrecoverable, so an
/// operator has to opt in.
#[sqlx::test]
async fn a_dry_run_reports_the_orphan_without_deleting_it(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping a_dry_run_reports_the_orphan_without_deleting_it: \
             no MinIO in the environment"
        );
        return;
    };
    let storage = Storage::new(s3.clone(), bucket.clone());
    let router = test_router_with_storage(db.clone(), storage.clone());
    let cookie = register_verify_login(&router, &db, "reconcile-b@example.test").await;
    let fixture = two_attachments(&router, &db, &cookie).await;
    let orphan = &fixture.keys[0];

    orphan_the_object(&db, orphan).await;

    let mut conn = db.acquire().await.unwrap();
    let outcome = reconcile(
        &storage,
        &mut conn,
        &options(&fixture.group_id, false, 0),
        Utc::now() + Duration::minutes(5),
    )
    .await
    .unwrap();

    assert_eq!(outcome.scan.orphan_keys(), vec![orphan.clone()]);
    assert!(outcome.deleted.is_empty(), "a dry run deletes nothing");
    assert!(
        exists(&s3, &bucket, orphan).await,
        "a dry run must leave the object in place"
    );
}

/// Trap 2 of #58: `put_object` runs before the metadata row commits, so a
/// freshly written object with no row is indistinguishable from an upload
/// still in flight. Under the default 24h window the same orphan the test
/// above deletes is left alone.
#[sqlx::test]
async fn an_object_younger_than_the_window_is_left_alone(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping an_object_younger_than_the_window_is_left_alone: \
             no MinIO in the environment"
        );
        return;
    };
    let storage = Storage::new(s3.clone(), bucket.clone());
    let router = test_router_with_storage(db.clone(), storage.clone());
    let cookie = register_verify_login(&router, &db, "reconcile-c@example.test").await;
    let fixture = two_attachments(&router, &db, &cookie).await;
    let orphan = &fixture.keys[0];

    orphan_the_object(&db, orphan).await;

    let mut conn = db.acquire().await.unwrap();
    let outcome = reconcile(
        &storage,
        &mut conn,
        &options(&fixture.group_id, true, DEFAULT_MIN_AGE_HOURS),
        Utc::now(),
    )
    .await
    .unwrap();

    assert!(
        outcome.scan.orphans.is_empty(),
        "an object uploaded seconds ago may be a live upload: {:?}",
        outcome.scan.orphans
    );
    assert_eq!(outcome.scan.in_flight, 1);
    assert!(outcome.deleted.is_empty());
    assert!(
        exists(&s3, &bucket, orphan).await,
        "--apply must still respect the age window"
    );
}

/// Trap 1 of #58, and the one that empties the bucket if missed:
/// `event_attachments` is `FORCE ROW LEVEL SECURITY`, so an unscoped
/// `SELECT storage_key` on a connection without `BYPASSRLS` returns zero
/// rows rather than all of them — and zero known keys means every object
/// in the bucket classifies as an orphan.
///
/// The storage handed to the pass here points at an unreachable endpoint:
/// the abort has to happen at the connection check, before a single object
/// is listed, so any error that isn't about RLS means the guard didn't
/// fire first.
#[sqlx::test]
async fn a_connection_that_does_not_bypass_rls_aborts_before_listing(db: PgPool) {
    // Demonstrated without MinIO on purpose — the guard must hold on any
    // checkout, not only where a bucket happens to be configured.
    let unreachable_storage = test_state(db.clone()).storage;

    let role = format!("reconcile_test_role_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE ROLE {role} NOSUPERUSER NOBYPASSRLS"
    )))
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT ON event_attachments TO {role}"
    )))
    .execute(&db)
    .await
    .unwrap();

    let mut tx = db.begin().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("SET LOCAL ROLE {role}")))
        .execute(&mut *tx)
        .await
        .unwrap();

    let error = reconcile(
        &unreachable_storage,
        &mut tx,
        &options("whatever", true, 0),
        Utc::now(),
    )
    .await
    .expect_err("a connection that cannot bypass RLS must abort, not report an empty table");

    let message = error.to_string();
    assert!(
        message.contains("RLS"),
        "the abort must name the reason, got: {message}"
    );

    drop(tx);
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "REVOKE ALL ON event_attachments FROM {role}"
    )))
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP ROLE {role}")))
        .execute(&db)
        .await
        .unwrap();
}
