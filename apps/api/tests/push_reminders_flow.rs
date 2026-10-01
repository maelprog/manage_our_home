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
    send_due_notifications, NO_PUSH_SUBSCRIPTION, REMINDER_SUBJECT,
};
use manage_our_home::notifications::push::{PushOutcome, MAX_CONSECUTIVE_FAILURES};
use sqlx::PgPool;
use std::sync::Mutex;
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
        Some(serde_json::json!({"email": email, "password": password, "display_name": email, "declares_minimum_age": true})),
    )
    .await;
    let token: Uuid = sqlx::query_scalar(
        "SELECT token FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = $1",
    )
    .bind(email)
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
async fn an_account_keeps_fifty_devices_and_the_51st_replaces_the_oldest(db: PgPool) {
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
    sqlx::query(
        "UPDATE push_subscriptions SET created_at = now() - interval '1 day' WHERE endpoint = $1",
    )
    .bind(format!("{FCM}-17"))
    .execute(&db)
    .await
    .unwrap();

    assert_eq!(
        subscribe(&router, &cookie, &format!("{FCM}-50")).await,
        StatusCode::CREATED
    );

    assert_eq!(settings(&router, &cookie).await["push_subscriptions"], 50);
    let oldest_left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM push_subscriptions WHERE endpoint = $1")
            .bind(format!("{FCM}-17"))
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(oldest_left, 0, "the oldest device is the one replaced");
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
/// is granted: the same account doing so changes nothing of the row.
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
    add_device(&db, member, &dead).await;
    add_device(&db, member, &flaky).await;
    sqlx::query("UPDATE push_subscriptions SET consecutive_failures = $2 WHERE endpoint = $1")
        .bind(&dead)
        .bind(MAX_CONSECUTIVE_FAILURES - 1)
        .execute(&db)
        .await
        .unwrap();
    due_reminder(&db, member).await;

    let failing = |_: &str| PushOutcome::Failed("push service unreachable".into());
    pass(&db, failing).await;
    assert_eq!(
        failures(&db, &dead).await,
        None,
        "the dead device is forgotten"
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
    assert!(devices[0]["created_at"].is_string());
}
