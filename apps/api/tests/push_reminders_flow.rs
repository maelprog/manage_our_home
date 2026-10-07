//! Reminders by notification (#306), against a real database: the
//! member's channel and devices (`/account/notifications`,
//! `/account/push-subscriptions`), the reminder pass sending on that
//! channel, and the art. 15 export of both.
//!
//! The pass is driven with recording senders, as in
//! `reminder_recipient_flow`: no email leaves and no push service is
//! called; each device's answer is chosen by the test.

mod common;

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Utc};
use common::{assert_status, call, json_body, set_cookie, test_router, test_state};
use manage_our_home::jobs::scheduled_notifications::{
    send_due_notifications, MAX_CONCURRENT_DEVICES, MAX_CONCURRENT_REMINDERS, NO_PUSH_SUBSCRIPTION,
    REMINDER_SUBJECT,
};
use manage_our_home::notifications::push::{
    PushOutcome, MAX_CONSECUTIVE_FAILURES, MIN_FAILING_DAYS,
};
use sqlx::PgPool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::Barrier;
use uuid::Uuid;

const FCM: &str = "https://fcm.googleapis.com/fcm/send/dVq3mJ8:APA91bH";
const MOZILLA: &str = "https://updates.push.services.mozilla.com/wpush/v2/gAAAAABk";

async fn register_verify_login(router: &axum::Router, db: &PgPool, email: &str) -> String {
    let password = "push-flow-password-1";
    call(
        router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": email, "password": password, "display_name": email, "declares_minimum_age": true, "accepts_terms": true})),
    )
    .await;
    let token = common::verification_token(db, email).await;
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
        Some(serde_json::json!({"email": email, "password": password})),
    )
    .await;
    set_cookie(&login).unwrap()
}

async fn settings(router: &axum::Router, cookie: &str) -> serde_json::Value {
    let res = call(
        router,
        Method::GET,
        "/account/notifications",
        Some(cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    json_body(res).await
}

async fn subscribe(router: &axum::Router, cookie: &str, endpoint: &str) -> StatusCode {
    call(
        router,
        Method::POST,
        "/account/push-subscriptions",
        Some(cookie),
        Some(serde_json::json!({ "endpoint": endpoint })),
    )
    .await
    .status()
}

// -- the member's settings ----------------------------------------------------

/// A new account starts on notifications, with no device, and is handed
/// the key a browser subscribes with.
#[sqlx::test]
async fn a_new_account_starts_on_notifications_without_a_device(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "new@example.test").await;

    let s = settings(&router, &cookie).await;
    assert_eq!(s["reminder_channel"], "push");
    assert_eq!(s["push_subscriptions"], 0);
    let key = s["vapid_public_key"].as_str().unwrap();
    // An uncompressed P-256 point: 65 bytes, 87 characters of base64url.
    assert_eq!(key.len(), 87, "{key}");
}

#[sqlx::test]
async fn the_channel_can_be_switched_to_email_or_both_and_nothing_else(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "switch@example.test").await;

    for channel in ["email", "both", "push"] {
        let res = call(
            &router,
            Method::PUT,
            "/account/notifications",
            Some(&cookie),
            Some(serde_json::json!({ "reminder_channel": channel })),
        )
        .await;
        assert_status(&res, StatusCode::NO_CONTENT);
        assert_eq!(
            settings(&router, &cookie).await["reminder_channel"],
            channel
        );
    }

    for channel in ["sms", "", "EMAIL"] {
        let res = call(
            &router,
            Method::PUT,
            "/account/notifications",
            Some(&cookie),
            Some(serde_json::json!({ "reminder_channel": channel })),
        )
        .await;
        assert_status(&res, StatusCode::BAD_REQUEST);
        assert_eq!(json_body(res).await["error"], "invalid_reminder_channel");
    }
    assert_eq!(settings(&router, &cookie).await["reminder_channel"], "push");
}

#[sqlx::test]
async fn the_settings_need_a_session(db: PgPool) {
    let router = test_router(db.clone());
    for (method, uri) in [
        (Method::GET, "/account/notifications"),
        (Method::PUT, "/account/notifications"),
        (Method::POST, "/account/push-subscriptions"),
        (Method::DELETE, "/account/push-subscriptions"),
    ] {
        let body = (method != Method::GET && method != Method::DELETE)
            .then(|| serde_json::json!({ "reminder_channel": "email", "endpoint": FCM }));
        let res = call(&router, method.clone(), uri, None, body).await;
        assert_status(&res, StatusCode::UNAUTHORIZED);
    }
}

// -- devices ------------------------------------------------------------------

#[sqlx::test]
async fn a_device_subscribes_and_all_devices_can_be_removed(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "devices@example.test").await;

    assert_eq!(subscribe(&router, &cookie, FCM).await, StatusCode::CREATED);
    assert_eq!(
        subscribe(&router, &cookie, MOZILLA).await,
        StatusCode::CREATED
    );
    // The same device again: still one row for it.
    assert_eq!(subscribe(&router, &cookie, FCM).await, StatusCode::CREATED);
    assert_eq!(settings(&router, &cookie).await["push_subscriptions"], 2);

    let res = call(
        &router,
        Method::DELETE,
        "/account/push-subscriptions",
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(settings(&router, &cookie).await["push_subscriptions"], 0);
}

async fn device_count(db: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM push_subscriptions")
        .fetch_one(db)
        .await
        .unwrap()
}

/// Controller's decision of 2026-10-01: 50 devices per account; the 51st
/// replaces the oldest.
#[sqlx::test]
async fn an_account_keeps_fifty_devices_and_the_51st_replaces_the_least_recently_used(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "cap@example.test").await;
    for i in 0..50 {
        let endpoint = format!("{FCM}-{i}");
        assert_eq!(
            subscribe(&router, &cookie, &endpoint).await,
            StatusCode::CREATED
        );
    }
    assert_eq!(settings(&router, &cookie).await["push_subscriptions"], 50);
    let set = |sql: &'static str, i: i32| {
        let db = db.clone();
        async move {
            sqlx::query(sql)
                .bind(format!("{FCM}-{i}"))
                .execute(&db)
                .await
                .unwrap();
        }
    };
    sqlx::query(
        "UPDATE push_subscriptions
         SET created_at = now() - interval '1 day', last_seen_at = now() - interval '1 hour'",
    )
    .execute(&db)
    .await
    .unwrap();
    // Subscribed first of all and last seen 20 days ago — older than the
    // device to replace below — but registered again just now: in use.
    set(
        "UPDATE push_subscriptions
         SET created_at = now() - interval '30 days', last_seen_at = now() - interval '20 days'
         WHERE endpoint = $1",
        17,
    )
    .await;
    assert_eq!(
        subscribe(&router, &cookie, &format!("{FCM}-17")).await,
        StatusCode::CREATED
    );
    // Not registered for weeks, but delivered to just now: in use.
    set(
        "UPDATE push_subscriptions
         SET last_seen_at = now() - interval '20 days', last_success_at = now() WHERE endpoint = $1",
        5,
    )
    .await;
    // Neither seen nor delivered to for ten days: the one to replace.
    set(
        "UPDATE push_subscriptions SET last_seen_at = now() - interval '10 days' WHERE endpoint = $1",
        30,
    )
    .await;

    assert_eq!(
        subscribe(&router, &cookie, &format!("{FCM}-50")).await,
        StatusCode::CREATED
    );

    assert_eq!(settings(&router, &cookie).await["push_subscriptions"], 50);
    let left = |i: i32| {
        let db = db.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM push_subscriptions WHERE endpoint = $1",
            )
            .bind(format!("{FCM}-{i}"))
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };
    assert_eq!(
        left(30).await,
        0,
        "the least recently used device is replaced"
    );
    assert_eq!(left(17).await, 1, "registered again: kept");
    assert_eq!(left(5).await, 1, "delivered to: kept");
    assert_eq!(left(50).await, 1);
}

/// A subscription that waited for another one of the same account (the
/// `users` row lock) is stamped when it is written, not when its
/// transaction began: it is the newest device, never the one replaced.
#[sqlx::test]
async fn a_subscription_that_waited_for_the_lock_is_not_taken_for_the_oldest(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "wait@example.test").await;
    for i in 0..50 {
        let endpoint = format!("{FCM}-w{i}");
        assert_eq!(
            subscribe(&router, &cookie, &endpoint).await,
            StatusCode::CREATED
        );
    }

    // Hold the account's row, as a concurrent subscription would.
    let mut holder = manage_our_home::db::begin(&db).await.unwrap();
    sqlx::query("SELECT id FROM users WHERE email = 'wait@example.test' FOR UPDATE")
        .execute(&mut *holder)
        .await
        .unwrap();
    let waiting = {
        let router = router.clone();
        let cookie = cookie.clone();
        tokio::spawn(async move { subscribe(&router, &cookie, &format!("{FCM}-late")).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    // While it waits, every other device is used after its transaction began.
    sqlx::query("UPDATE push_subscriptions SET last_seen_at = clock_timestamp()")
        .execute(&mut *holder)
        .await
        .unwrap();
    holder.commit().await.unwrap();

    assert_eq!(waiting.await.unwrap(), StatusCode::CREATED);
    assert_eq!(device_count(&db).await, 50);
    let late: i64 =
        sqlx::query_scalar("SELECT count(*) FROM push_subscriptions WHERE endpoint = $1")
            .bind(format!("{FCM}-late"))
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(late, 1, "the device just subscribed was replaced at once");
}

/// The ceiling holds when the subscriptions arrive all at once.
#[sqlx::test]
async fn the_ceiling_holds_under_concurrent_subscriptions(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "rush@example.test").await;
    let calls = (0..80).map(|i| {
        let router = router.clone();
        let cookie = cookie.clone();
        async move { subscribe(&router, &cookie, &format!("{FCM}-rush-{i}")).await }
    });
    let statuses = futures::future::join_all(calls).await;
    assert!(
        statuses.iter().all(|s| *s == StatusCode::CREATED),
        "{statuses:?}"
    );
    assert_eq!(device_count(&db).await, 50);
}

/// The page registers the device again on every view once the permission
/// is granted: the same account doing so moves `last_seen_at` only, and
/// keeps the subscription date and the delivery record.
#[sqlx::test]
async fn the_same_device_registered_again_keeps_its_dates(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "again@example.test").await;
    assert_eq!(subscribe(&router, &cookie, FCM).await, StatusCode::CREATED);
    sqlx::query(
        "UPDATE push_subscriptions
         SET created_at = now() - interval '3 days', last_success_at = now() - interval '1 day'",
    )
    .execute(&db)
    .await
    .unwrap();
    type Dates = (DateTime<Utc>, Option<DateTime<Utc>>);
    let dates = || async {
        sqlx::query_as::<_, Dates>("SELECT created_at, last_success_at FROM push_subscriptions")
            .fetch_one(&db)
            .await
            .unwrap()
    };
    let before = dates().await;

    assert_eq!(subscribe(&router, &cookie, FCM).await, StatusCode::CREATED);

    let after = dates().await;
    assert_eq!(before, after);
    assert!(after.1.is_some());
}

/// The server POSTs to whatever endpoint is stored: only a browser push
/// service's is taken.
#[sqlx::test]
async fn an_endpoint_that_is_not_a_push_service_is_refused(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "ssrf@example.test").await;

    for endpoint in [
        "http://169.254.169.254/latest/meta-data",
        "https://169.254.169.254/latest/meta-data",
        "https://api:8080/admin/users",
        "https://localhost/x",
        "http://fcm.googleapis.com/fcm/send/x",
        "https://fcm.googleapis.com.evil.test/x",
        "not a url",
    ] {
        let res = call(
            &router,
            Method::POST,
            "/account/push-subscriptions",
            Some(&cookie),
            Some(serde_json::json!({ "endpoint": endpoint })),
        )
        .await;
        assert_status(&res, StatusCode::BAD_REQUEST);
        assert_eq!(
            json_body(res).await["error"],
            "invalid_push_endpoint",
            "{endpoint}"
        );
    }
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM push_subscriptions")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(stored, 0);
}

/// A device signed into another account moves to it: one device, one
/// recipient.
#[sqlx::test]
async fn a_device_subscribed_again_by_another_account_moves_to_it(db: PgPool) {
    let router = test_router(db.clone());
    let first = register_verify_login(&router, &db, "first@example.test").await;
    let second = register_verify_login(&router, &db, "second@example.test").await;

    assert_eq!(subscribe(&router, &first, FCM).await, StatusCode::CREATED);
    assert_eq!(subscribe(&router, &second, FCM).await, StatusCode::CREATED);
    assert_eq!(settings(&router, &first).await["push_subscriptions"], 0);
    assert_eq!(settings(&router, &second).await["push_subscriptions"], 1);

    // Removing one's devices leaves the other account's alone.
    call(
        &router,
        Method::DELETE,
        "/account/push-subscriptions",
        Some(&first),
        None,
    )
    .await;
    assert_eq!(settings(&router, &second).await["push_subscriptions"], 1);
}

/// The device's record is the first account's: when it moves, the second
/// account starts a fresh one. Kept, the dates and failures of deliveries
/// made to the first account would appear in the second's art. 15 export.
#[sqlx::test]
async fn a_device_moved_to_another_account_carries_none_of_its_history(db: PgPool) {
    let router = test_router(db.clone());
    let first = register_verify_login(&router, &db, "previous@example.test").await;
    let second = register_verify_login(&router, &db, "next@example.test").await;
    assert_eq!(subscribe(&router, &first, FCM).await, StatusCode::CREATED);
    sqlx::query(
        "UPDATE push_subscriptions
         SET created_at = now() - interval '30 days',
             last_success_at = now() - interval '3 days',
             consecutive_failures = 2,
             failing_since = now() - interval '2 days'",
    )
    .execute(&db)
    .await
    .unwrap();

    assert_eq!(subscribe(&router, &second, FCM).await, StatusCode::CREATED);

    let res = call(&router, Method::GET, "/account/export", Some(&second), None).await;
    assert_status(&res, StatusCode::OK);
    let doc = json_body(res).await;
    let devices = doc["push_subscriptions"].as_array().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["endpoint"], FCM);
    assert!(devices[0]["last_success_at"].is_null(), "{devices:?}");
    assert_eq!(devices[0]["consecutive_failures"], 0, "{devices:?}");
    assert!(devices[0]["failing_since"].is_null(), "{devices:?}");
    let created_at: DateTime<Utc> = devices[0]["created_at"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .expect("the export carries the subscription date as a timestamp");
    assert!(
        created_at > Utc::now() - chrono::Duration::days(1),
        "subscription date of the first account kept: {created_at}"
    );

    let res = call(&router, Method::GET, "/account/export", Some(&first), None).await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(
        json_body(res).await["push_subscriptions"],
        serde_json::json!([])
    );
}

/// Without a VAPID key no browser can subscribe; the API says so rather
/// than store an endpoint nothing will ever be sent to.
#[sqlx::test]
async fn a_server_without_a_key_takes_no_subscription(db: PgPool) {
    let mut state = test_state(db.clone());
    state.push = None;
    let router = manage_our_home::build_router(state);
    let cookie = register_verify_login(&router, &db, "nokey@example.test").await;

    assert!(settings(&router, &cookie).await["vapid_public_key"].is_null());
    let res = call(
        &router,
        Method::POST,
        "/account/push-subscriptions",
        Some(&cookie),
        Some(serde_json::json!({ "endpoint": FCM })),
    )
    .await;
    assert_status(&res, StatusCode::CONFLICT);
    assert_eq!(json_body(res).await["error"], "push_not_configured");
}

// -- the reminder pass ----------------------------------------------------------

async fn insert_user(db: &PgPool, email: &str, channel: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, display_name, age_declared_at, reminder_channel)
         VALUES ($1, 'not-a-real-hash', $1, now() - interval '1 year', $2)
         RETURNING id",
    )
    .bind(email)
    .bind(channel)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn add_device(db: &PgPool, user: Uuid, endpoint: &str) {
    sqlx::query("INSERT INTO push_subscriptions (user_id, endpoint) VALUES ($1, $2)")
        .bind(user)
        .bind(endpoint)
        .execute(db)
        .await
        .unwrap();
}

/// A group owned by `by`, one event of theirs starting in 9 minutes, and
/// its reminder's notification due a minute ago. Returns its id.
async fn due_reminder(db: &PgPool, by: Uuid) -> Uuid {
    let group: Uuid = sqlx::query_scalar(
        "INSERT INTO groups (name, created_by) VALUES ('Maison', $1) RETURNING id",
    )
    .bind(by)
    .fetch_one(db)
    .await
    .unwrap();
    let event: Uuid = sqlx::query_scalar(
        "INSERT INTO events (group_id, created_by, title, starts_at, ends_at)
         VALUES ($1, $2, 'IRM', now() + interval '9 minutes', now() + interval '1 hour')
         RETURNING id",
    )
    .bind(group)
    .bind(by)
    .fetch_one(db)
    .await
    .unwrap();
    let reminder: Uuid = sqlx::query_scalar(
        "INSERT INTO event_reminders (event_id, offset_minutes) VALUES ($1, 10) RETURNING id",
    )
    .bind(event)
    .fetch_one(db)
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO scheduled_notifications (event_reminder_id, event_id, occurrence_at, fire_at)
         VALUES ($1, $2, now() + interval '9 minutes', now() - interval '1 minute')
         RETURNING id",
    )
    .bind(reminder)
    .bind(event)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn notification(db: &PgPool, id: Uuid) -> (String, i32, Option<String>) {
    sqlx::query_as("SELECT status, attempts, last_error FROM scheduled_notifications WHERE id = $1")
        .bind(id)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn devices(db: &PgPool, user: Uuid) -> Vec<(String, bool)> {
    sqlx::query_as(
        "SELECT endpoint, last_success_at IS NOT NULL FROM push_subscriptions
         WHERE user_id = $1 ORDER BY endpoint",
    )
    .bind(user)
    .fetch_all(db)
    .await
    .unwrap()
}

/// What one pass sent: the emails (recipient, subject) and the pushes
/// (endpoint, TTL). Each device answers what `answer` gives for its
/// endpoint.
async fn pass(
    db: &PgPool,
    answer: impl Fn(&str) -> PushOutcome,
) -> (Vec<(String, String)>, Vec<(String, i64)>) {
    let emails = Mutex::new(Vec::new());
    let pushes = Mutex::new(Vec::new());
    let send = |to: String, subject: String, _body: String| {
        emails.lock().unwrap().push((to, subject));
        async { Ok::<(), anyhow::Error>(()) }
    };
    let send_push = |endpoint: String, ttl: i64| {
        let outcome = answer(&endpoint);
        pushes.lock().unwrap().push((endpoint, ttl));
        async move { outcome }
    };
    send_due_notifications(db, send, send_push).await.unwrap();
    (emails.into_inner().unwrap(), pushes.into_inner().unwrap())
}

/// On notifications, every device gets a push without payload and no
/// email goes out. A device whose push service answers 410 is forgotten;
/// one that took it is stamped.
#[sqlx::test]
async fn a_member_on_notifications_gets_a_push_on_every_device_and_no_email(db: PgPool) {
    let member = insert_user(&db, "push@example.test", "push").await;
    add_device(&db, member, FCM).await;
    add_device(&db, member, MOZILLA).await;
    let due = due_reminder(&db, member).await;

    let (emails, pushes) = pass(&db, |endpoint| {
        if endpoint == MOZILLA {
            PushOutcome::Gone
        } else {
            PushOutcome::Delivered
        }
    })
    .await;

    assert!(emails.is_empty(), "{emails:?}");
    let mut sent_to: Vec<&str> = pushes.iter().map(|(e, _)| e.as_str()).collect();
    sent_to.sort();
    assert_eq!(sent_to, [FCM, MOZILLA]);
    // Held until the occurrence starts, 9 minutes away: a little under.
    for (_, ttl) in &pushes {
        assert!((480..=540).contains(ttl), "{ttl}");
    }
    assert_eq!(notification(&db, due).await, ("sent".into(), 0, None));
    assert_eq!(devices(&db, member).await, [(FCM.to_string(), true)]);
}

/// The controller's decision of 2026-10-01: no device, no reminder — and
/// no email in its place. Retired without an attempt.
#[sqlx::test]
async fn a_member_on_notifications_without_a_device_gets_nothing_and_no_email(db: PgPool) {
    let member = insert_user(&db, "nodevice@example.test", "push").await;
    let due = due_reminder(&db, member).await;

    let (emails, pushes) = pass(&db, |_| PushOutcome::Delivered).await;

    assert!(emails.is_empty(), "{emails:?}");
    assert!(pushes.is_empty(), "{pushes:?}");
    assert_eq!(
        notification(&db, due).await,
        ("failed".into(), 0, Some(NO_PUSH_SUBSCRIPTION.into()))
    );
}

/// Every device expired: forgotten, and the reminder retired like for a
/// member without any.
#[sqlx::test]
async fn a_member_whose_devices_all_expired_loses_them_and_the_reminder(db: PgPool) {
    let member = insert_user(&db, "expired@example.test", "push").await;
    add_device(&db, member, FCM).await;
    let due = due_reminder(&db, member).await;

    let (emails, _) = pass(&db, |_| PushOutcome::Gone).await;

    assert!(emails.is_empty());
    assert!(devices(&db, member).await.is_empty());
    assert_eq!(
        notification(&db, due).await,
        ("failed".into(), 0, Some(NO_PUSH_SUBSCRIPTION.into()))
    );
}

/// A push service down is retried on the next pass, and the device kept.
#[sqlx::test]
async fn a_push_service_down_is_retried(db: PgPool) {
    let member = insert_user(&db, "down@example.test", "push").await;
    add_device(&db, member, FCM).await;
    let due = due_reminder(&db, member).await;

    pass(&db, |_| {
        PushOutcome::Failed("push service answered 503".into())
    })
    .await;
    assert_eq!(
        notification(&db, due).await,
        (
            "pending".into(),
            1,
            Some("push service answered 503".into())
        )
    );
    assert_eq!(devices(&db, member).await, [(FCM.to_string(), false)]);

    pass(&db, |_| PushOutcome::Delivered).await;
    assert_eq!(notification(&db, due).await.0, "sent");
}

async fn failures(db: &PgPool, endpoint: &str) -> Option<i32> {
    sqlx::query_scalar("SELECT consecutive_failures FROM push_subscriptions WHERE endpoint = $1")
        .bind(endpoint)
        .fetch_optional(db)
        .await
        .unwrap()
}

/// A device that keeps failing without ever answering 404/410 is counted,
/// reset by a delivery, and forgotten at the ceiling.
#[sqlx::test]
async fn a_device_failing_in_a_row_is_forgotten_at_the_ceiling(db: PgPool) {
    let member = insert_user(&db, "flaky@example.test", "push").await;
    let dead = format!("{FCM}-dead");
    let flaky = format!("{FCM}-flaky");
    // Many failures, but all of them in the last minutes: an outage.
    let burst = format!("{FCM}-burst");
    for endpoint in [&dead, &flaky, &burst] {
        add_device(&db, member, endpoint).await;
    }
    let failing_for = |endpoint: &str, failures: i32, days: i32| {
        let db = db.clone();
        let endpoint = endpoint.to_string();
        async move {
            sqlx::query(
                "UPDATE push_subscriptions
                 SET consecutive_failures = $2,
                     failing_since = now() - make_interval(days => $3) - interval '1 minute'
                 WHERE endpoint = $1",
            )
            .bind(endpoint)
            .bind(failures)
            .bind(days)
            .execute(&db)
            .await
            .unwrap();
        }
    };
    failing_for(&dead, MAX_CONSECUTIVE_FAILURES - 1, MIN_FAILING_DAYS as i32).await;
    failing_for(&burst, 60, 0).await;
    due_reminder(&db, member).await;

    let failing = |_: &str| PushOutcome::Failed("push service unreachable".into());
    pass(&db, failing).await;
    assert_eq!(
        failures(&db, &dead).await,
        None,
        "the device failing for a week is forgotten"
    );
    assert_eq!(
        failures(&db, &burst).await,
        Some(61),
        "an outage forgets nothing"
    );
    assert_eq!(failures(&db, &flaky).await, Some(1));

    pass(&db, failing).await;
    assert_eq!(failures(&db, &flaky).await, Some(2));

    pass(&db, |_| PushOutcome::Delivered).await;
    assert_eq!(
        failures(&db, &flaky).await,
        Some(0),
        "a delivery resets the count"
    );
}

/// On email, the email goes and no device is pushed, subscribed or not.
#[sqlx::test]
async fn a_member_on_email_gets_the_email_only(db: PgPool) {
    let member = insert_user(&db, "mail@example.test", "email").await;
    add_device(&db, member, FCM).await;
    let due = due_reminder(&db, member).await;

    let (emails, pushes) = pass(&db, |_| PushOutcome::Delivered).await;

    assert_eq!(
        emails,
        [(
            "mail@example.test".to_string(),
            REMINDER_SUBJECT.to_string()
        )]
    );
    assert!(pushes.is_empty(), "{pushes:?}");
    assert_eq!(notification(&db, due).await, ("sent".into(), 0, None));
}

/// On both, both go.
#[sqlx::test]
async fn a_member_on_both_gets_the_email_and_the_push(db: PgPool) {
    let member = insert_user(&db, "both@example.test", "both").await;
    add_device(&db, member, FCM).await;
    let due = due_reminder(&db, member).await;

    let (emails, pushes) = pass(&db, |_| PushOutcome::Delivered).await;

    assert_eq!(emails.len(), 1);
    assert_eq!(pushes.len(), 1);
    assert_eq!(notification(&db, due).await, ("sent".into(), 0, None));
}

// -- pushing concurrently (#321) ------------------------------------------------

/// A pass whose pushes are answered by `answer`, an async fake push
/// service, bounded by `within`: a pass still running then fails the test
/// instead of hanging it.
async fn pass_within<A, AFut>(db: &PgPool, within: Duration, answer: A)
where
    A: Fn(String) -> AFut,
    AFut: std::future::Future<Output = PushOutcome>,
{
    let send = |_to: String, _subject: String, _body: String| async { Ok::<(), anyhow::Error>(()) };
    let send_push = |endpoint: String, _ttl: i64| answer(endpoint);
    tokio::time::timeout(within, send_due_notifications(db, send, send_push))
        .await
        .expect("the pass is still waiting on a push service that never answers")
        .unwrap();
}

/// A member's push service that does not answer holds back no one else:
/// each account's service here answers only once the other's has been
/// called. Taken one after the other, whichever went first would wait
/// forever.
#[sqlx::test]
async fn a_slow_push_service_holds_back_no_other_members_reminder(db: PgPool) {
    let slow = insert_user(&db, "slow@example.test", "push").await;
    let quick = insert_user(&db, "quick@example.test", "push").await;
    add_device(&db, slow, FCM).await;
    add_device(&db, quick, MOZILLA).await;
    let slows = due_reminder(&db, slow).await;
    let quicks = due_reminder(&db, quick).await;

    let both_called = Barrier::new(2);
    pass_within(&db, Duration::from_secs(10), |_| async {
        both_called.wait().await;
        PushOutcome::Delivered
    })
    .await;

    assert_eq!(notification(&db, slows).await, ("sent".into(), 0, None));
    assert_eq!(notification(&db, quicks).await, ("sent".into(), 0, None));
}

/// The devices of one member are pushed at once too: a device whose push
/// service hangs does not hold back the member's others.
#[sqlx::test]
async fn a_members_devices_are_pushed_at_once(db: PgPool) {
    let member = insert_user(&db, "devices@example.test", "push").await;
    for i in 0..3 {
        add_device(&db, member, &format!("{FCM}-{i}")).await;
    }
    let due = due_reminder(&db, member).await;

    let all_called = Barrier::new(3);
    pass_within(&db, Duration::from_secs(10), |_| async {
        all_called.wait().await;
        PushOutcome::Delivered
    })
    .await;

    assert_eq!(notification(&db, due).await, ("sent".into(), 0, None));
    assert_eq!(devices(&db, member).await.len(), 3);
    assert!(devices(&db, member).await.iter().all(|(_, ok)| *ok));
}

/// Counts the pushes in flight, and the most seen at once. Each push is
/// held until `bound` of them were in flight together — or, should the
/// pass never let that many through, for a few seconds: none answers
/// before the bound is reached, so `most` reaches it however slowly the
/// pass gets its pushes started. It is then held a little longer, for a
/// pass that would start more than `bound` to be seen doing so.
struct InFlight {
    bound: usize,
    now: AtomicUsize,
    most: AtomicUsize,
}

impl InFlight {
    fn up_to(bound: usize) -> Self {
        Self {
            bound,
            now: AtomicUsize::new(0),
            most: AtomicUsize::new(0),
        }
    }

    async fn push(&self) -> PushOutcome {
        let now = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
        let held_until = tokio::time::Instant::now() + Duration::from_secs(5);
        while self.most.load(Ordering::SeqCst) < self.bound
            && tokio::time::Instant::now() < held_until
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.now.fetch_sub(1, Ordering::SeqCst);
        PushOutcome::Delivered
    }
}

/// Concurrent, but bounded: exactly `MAX_CONCURRENT_REMINDERS` reminders
/// pushed at once, never more, and every one of them sent.
#[sqlx::test]
async fn reminders_are_pushed_at_once_up_to_a_bound(db: PgPool) {
    let mut due = Vec::new();
    for i in 0..3 * MAX_CONCURRENT_REMINDERS {
        let member = insert_user(&db, &format!("bound{i}@example.test"), "push").await;
        add_device(&db, member, &format!("{FCM}-{i}")).await;
        due.push(due_reminder(&db, member).await);
    }

    let in_flight = InFlight::up_to(MAX_CONCURRENT_REMINDERS);
    pass_within(&db, Duration::from_secs(30), |_| in_flight.push()).await;

    let most = in_flight.most.load(Ordering::SeqCst);
    assert_eq!(most, MAX_CONCURRENT_REMINDERS, "{most} pushed at once");
    for id in due {
        assert_eq!(notification(&db, id).await.0, "sent");
    }
}

/// Same bound per member: exactly `MAX_CONCURRENT_DEVICES` of their
/// devices pushed at once, never more.
#[sqlx::test]
async fn a_members_devices_are_pushed_at_once_up_to_a_bound(db: PgPool) {
    let member = insert_user(&db, "many@example.test", "push").await;
    for i in 0..3 * MAX_CONCURRENT_DEVICES {
        add_device(&db, member, &format!("{FCM}-{i}")).await;
    }
    let due = due_reminder(&db, member).await;

    let in_flight = InFlight::up_to(MAX_CONCURRENT_DEVICES);
    pass_within(&db, Duration::from_secs(30), |_| in_flight.push()).await;

    let most = in_flight.most.load(Ordering::SeqCst);
    assert_eq!(most, MAX_CONCURRENT_DEVICES, "{most} pushed at once");
    assert_eq!(notification(&db, due).await.0, "sent");
    assert!(devices(&db, member).await.iter().all(|(_, ok)| *ok));
}

// -- a reminder the database fails on (#366) -------------------------------------

/// Makes every write to the given notifications fail in the database, as
/// a lost connection or a lock timeout would.
async fn refuse_writes_to(db: &PgPool, ids: &[Uuid]) {
    sqlx::query("CREATE TABLE refused_notifications (id uuid PRIMARY KEY)")
        .execute(db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO refused_notifications SELECT unnest($1::uuid[])")
        .bind(ids)
        .execute(db)
        .await
        .unwrap();
    sqlx::query(
        "CREATE FUNCTION refuse_notification_write() RETURNS trigger AS $$
         BEGIN
             IF OLD.id IN (SELECT id FROM refused_notifications) THEN
                 RAISE EXCEPTION 'write refused for the test';
             END IF;
             RETURN NEW;
         END
         $$ LANGUAGE plpgsql",
    )
    .execute(db)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER refuse_notification_write BEFORE UPDATE ON scheduled_notifications
         FOR EACH ROW EXECUTE FUNCTION refuse_notification_write()",
    )
    .execute(db)
    .await
    .unwrap();
}

/// A notification the database fails on stops none of the others (#321):
/// they are sent, it stays pending for the next pass, and the pass reports
/// how many failed. Two fail on each side of the pass: on the reading side,
/// retiring a deactivated member's; on the pushing side, marking a mailed
/// one sent. With two on each side, a pass that stopped at its first
/// failure on either side would report fewer, whatever order the due
/// notifications are read in (their query has no `ORDER BY`): the
/// scenario runs with the failing ones written first, then last.
async fn a_database_error_on_one_reminder_stops_none_of_the_others(
    db: PgPool,
    failing_written_first: bool,
) {
    let others = |db: PgPool| async move {
        let mut others = Vec::new();
        for i in 0..3 {
            let member = insert_user(&db, &format!("other{i}@example.test"), "push").await;
            add_device(&db, member, &format!("{FCM}-{i}")).await;
            others.push(due_reminder(&db, member).await);
        }
        others
    };
    let failing = |db: PgPool| async move {
        let mut failing = Vec::new();
        for i in 0..2 {
            let gone = insert_user(&db, &format!("gone{i}@example.test"), "email").await;
            failing.push(due_reminder(&db, gone).await);
            sqlx::query("UPDATE users SET deactivated_at = now() WHERE id = $1")
                .bind(gone)
                .execute(&db)
                .await
                .unwrap();
            let mailed = insert_user(&db, &format!("mailed{i}@example.test"), "email").await;
            failing.push(due_reminder(&db, mailed).await);
        }
        failing
    };
    let (failing, others) = if failing_written_first {
        let failing = failing(db.clone()).await;
        (failing, others(db.clone()).await)
    } else {
        let others = others(db.clone()).await;
        (failing(db.clone()).await, others)
    };
    refuse_writes_to(&db, &failing).await;

    let send = |_to: String, _subject: String, _body: String| async { Ok::<(), anyhow::Error>(()) };
    let send_push = |_endpoint: String, _ttl: i64| async { PushOutcome::Delivered };
    let error = send_due_notifications(&db, send, send_push)
        .await
        .expect_err("the pass reports the reminders it could not see through");

    assert_eq!(
        error.to_string(),
        "4 due reminder(s) could not be sent or settled, see above"
    );
    for id in failing {
        assert_eq!(notification(&db, id).await, ("pending".into(), 0, None));
    }
    for id in others {
        assert_eq!(notification(&db, id).await, ("sent".into(), 0, None));
    }
}

#[sqlx::test]
async fn a_database_error_stops_no_other_reminder_failing_ones_written_first(db: PgPool) {
    a_database_error_on_one_reminder_stops_none_of_the_others(db, true).await;
}

#[sqlx::test]
async fn a_database_error_stops_no_other_reminder_failing_ones_written_last(db: PgPool) {
    a_database_error_on_one_reminder_stops_none_of_the_others(db, false).await;
}

// -- RGPD -----------------------------------------------------------------------

/// Art. 15: the channel is in the profile, and every device is listed.
#[sqlx::test]
async fn the_export_carries_the_channel_and_the_devices(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "export@example.test").await;
    assert_eq!(subscribe(&router, &cookie, FCM).await, StatusCode::CREATED);

    let res = call(&router, Method::GET, "/account/export", Some(&cookie), None).await;
    assert_status(&res, StatusCode::OK);
    let doc = json_body(res).await;
    assert_eq!(doc["profile"]["reminder_channel"], "push");
    let devices = doc["push_subscriptions"].as_array().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["endpoint"], FCM);
    assert_eq!(devices[0]["platform"], "web");
    assert_eq!(devices[0]["consecutive_failures"], 0);
    assert!(devices[0]["last_seen_at"].is_string());
    assert!(devices[0]["failing_since"].is_null());
    assert!(devices[0]["created_at"].is_string());
}
