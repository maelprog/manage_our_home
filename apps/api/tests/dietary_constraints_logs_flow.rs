//! No trace of an allergy in the logs, `audit_log` or the notifications
//! (#817). A binary of its own: the test installs the process-wide log
//! subscriber, and holds only while no other test writes to it.

mod common;
mod dietary;

use std::sync::{Arc, Mutex};

use axum::http::{Method, StatusCode};
use common::{call, test_router};
use dietary::{family, send, TAG};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

/// What the API writes to its logs while `run` goes, at every level.
#[derive(Clone)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// No log line, no `audit_log` row, no notification carries a tag or a
/// level, on the paths that succeed and on those that are refused.
#[sqlx::test]
async fn no_log_audit_or_notification_carries_an_allergy(db: PgPool) {
    let router = test_router(db.clone());
    let f = family(&router, &db, "trace").await;
    let audit_before: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log")
        .fetch_one(&db)
        .await
        .unwrap();

    let buffer = Buffer(Arc::new(Mutex::new(Vec::new())));
    let writer = buffer.clone();
    // The process-wide subscriber, not a scoped one: a scoped subscriber
    // misses what runs on another thread or in another task, and would
    // let a log line through unseen.
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .init();
    {
        // Each step with the answer it gets: a step refused for another
        // reason would not run the path it is there to watch.
        let steps: Vec<(Method, &str, Option<Value>, StatusCode)> = vec![
            // Refused: the first, without consent.
            (
                Method::POST,
                "/me/allergies",
                Some(json!({ "tag": TAG })),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                Method::POST,
                "/me/allergies",
                Some(json!({"tag": TAG, "consent": true})),
                StatusCode::CREATED,
            ),
            // Refused: already there.
            (
                Method::POST,
                "/me/allergies",
                Some(json!({ "tag": TAG })),
                StatusCode::CONFLICT,
            ),
            (
                Method::PUT,
                "/me/allergies/sharing",
                Some(json!({"tag": TAG, "group_id": f.group, "share": "public"})),
                StatusCode::OK,
            ),
            (
                Method::PUT,
                "/me/allergies/sharing",
                Some(json!({"tag": TAG, "group_id": f.group, "share": "anonymous"})),
                StatusCode::OK,
            ),
            // Refused: no such level, no such family.
            (
                Method::PUT,
                "/me/allergies/sharing",
                Some(json!({"tag": TAG, "group_id": f.group, "share": "secret"})),
                StatusCode::BAD_REQUEST,
            ),
            (
                Method::PUT,
                "/me/allergies/sharing",
                Some(json!({"tag": TAG, "group_id": Uuid::nil(), "share": "public"})),
                StatusCode::NOT_FOUND,
            ),
            (Method::GET, "/me/allergies", None, StatusCode::OK),
            (
                Method::POST,
                "/me/dislikes",
                Some(json!({"tag": "en:mushrooms"})),
                StatusCode::CREATED,
            ),
            (
                Method::DELETE,
                "/me/allergies",
                Some(json!({ "tag": TAG })),
                StatusCode::NO_CONTENT,
            ),
            // Refused: already gone.
            (
                Method::DELETE,
                "/me/allergies",
                Some(json!({ "tag": TAG })),
                StatusCode::NOT_FOUND,
            ),
            (
                Method::POST,
                "/me/allergies",
                Some(json!({"tag": TAG, "consent": true})),
                StatusCode::CREATED,
            ),
            (
                Method::DELETE,
                "/me/dietary-constraints",
                None,
                StatusCode::NO_CONTENT,
            ),
        ];
        for (method, uri, body, expected) in steps {
            let step = format!("{method} {uri}");
            let response = call(&router, method, uri, Some(&f.holder), body).await;
            assert_eq!(response.status(), expected, "{step}");
        }
        let (status, _) = send(
            &router,
            Method::GET,
            &format!("/groups/{}/allergies", f.group),
            &f.owner,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let logs = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    // The capture does see the requests: sqlx logs each statement it runs.
    assert!(logs.contains("user_allergies"), "nothing captured: {logs}");
    // The levels: as a value would be written in a log line, not as the
    // name of the function the family's read calls.
    for secret in [
        TAG,
        "peanut",
        "mushroom",
        "=anonymous",
        "=public",
        "\"anonymous\"",
        "\"public\"",
        "'anonymous'",
        "'public'",
    ] {
        assert!(!logs.contains(secret), "{secret} in the logs:\n{logs}");
    }

    let audit_after: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(audit_after, audit_before, "an audit row was written");
    let carrying: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE to_jsonb(audit_log)::text LIKE '%peanut%'",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(carrying, 0);
    let notifications: i64 = sqlx::query_scalar("SELECT count(*) FROM notifications")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(notifications, 0);
}
