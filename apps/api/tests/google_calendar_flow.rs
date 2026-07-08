mod common;

use axum::http::{Method, StatusCode};
use common::{assert_status, call, json_body, set_cookie, test_router};
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn register_verify_login(
    router: &axum::Router,
    db: &PgPool,
    email: &str,
    password: &str,
) -> String {
    call(
        router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": email, "password": password, "display_name": email})),
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
        Some(serde_json::json!({"email": email, "password": password})),
    )
    .await;
    set_cookie(&login).unwrap()
}

async fn create_group(router: &axum::Router, cookie: &str, name: &str) -> String {
    let res = call(
        router,
        Method::POST,
        "/groups",
        Some(cookie),
        Some(serde_json::json!({"name": name})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

const ICS_BODY: &str = "\
BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:flow-test-1@google.com
DTSTAMP:20260101T090000Z
DTSTART:20260601T140000Z
DTEND:20260601T150000Z
SUMMARY:Family dinner
LOCATION:Home
LAST-MODIFIED:20260101T090000Z
END:VEVENT
END:VCALENDAR
";

/// Serves a fixed ICS body over plain HTTP on 127.0.0.1, once per accepted
/// connection, until `max_requests` connections have been served — good
/// enough to stand in for a Google Calendar "secret address in iCal
/// format" feed without needing a real network dependency in the test
/// suite. `validate_feed_url` accepts http:// as well as https:// (see its
/// doc comment) specifically so this loopback server can be exercised
/// without TLS.
async fn spawn_ics_server(body: &'static str, max_requests: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..max_requests {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/calendar\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        }
    });
    format!("http://{addr}/basic.ics")
}

/// AC: only an admin/owner may create a calendar-import connection (Epic
/// #9's stricter permission bar — the feed URL is a bearer credential).
#[sqlx::test]
async fn only_admin_or_owner_can_create_calendar_import(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "cal-owner1@example.test", "owner-password1").await;
    let member_cookie =
        register_verify_login(&router, &db, "cal-member1@example.test", "member-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let invite = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(&owner_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    let token = json_body(invite).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    call(
        &router,
        Method::POST,
        &format!("/groups/invitations/{token}/accept"),
        Some(&member_cookie),
        None,
    )
    .await;

    let member_create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports"),
        Some(&member_cookie),
        Some(serde_json::json!({
            "label": "Mine",
            "feed_url": "https://calendar.google.com/calendar/ical/example/basic.ics"
        })),
    )
    .await;
    assert_status(&member_create, StatusCode::FORBIDDEN);

    let owner_create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "label": "Foyer calendar",
            "feed_url": "https://calendar.google.com/calendar/ical/example/basic.ics"
        })),
    )
    .await;
    assert_status(&owner_create, StatusCode::CREATED);
    let body = json_body(owner_create).await;
    assert_eq!(body["label"], "Foyer calendar");
    // The decrypted feed URL is never echoed back.
    assert!(body.get("feed_url").is_none());
}

/// AC: the stored feed URL is actually encrypted at rest (pgcrypto), not
/// just base64/plaintext-with-a-label.
#[sqlx::test]
async fn feed_url_is_encrypted_at_rest(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "cal-owner2@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "label": "Foyer calendar",
            "feed_url": "https://calendar.google.com/calendar/ical/example/basic.ics"
        })),
    )
    .await;

    let raw: Vec<u8> = sqlx::query_scalar("SELECT feed_url FROM calendar_imports LIMIT 1")
        .fetch_one(&db)
        .await
        .unwrap();
    let raw_str = String::from_utf8_lossy(&raw);
    assert!(!raw_str.contains("calendar.google.com"));
}

/// AC: triggering an import fetches the feed, upserts events by external
/// UID (idempotent across re-runs), and the resulting event is readable
/// through the normal Agenda endpoint.
#[sqlx::test]
async fn trigger_import_creates_and_dedupes_events(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "cal-owner3@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let feed_url = spawn_ics_server(ICS_BODY, 2).await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports"),
        Some(&owner_cookie),
        Some(serde_json::json!({"label": "Foyer calendar", "feed_url": feed_url})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let import_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let run1 = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports/{import_id}/import"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&run1, StatusCode::OK);
    let run1_body = json_body(run1).await;
    assert_eq!(run1_body["imported"], 1);
    assert_eq!(run1_body["updated"], 0);

    let events = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/events?from=2026-01-01T00:00:00Z&to=2027-01-01T00:00:00Z"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&events, StatusCode::OK);
    let events_body = json_body(events).await;
    let list = events_body["occurrences"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["title"], "Family dinner");
    assert_eq!(list[0]["location"], "Home");

    // Re-running against the same feed content (unchanged LAST-MODIFIED)
    // must be a no-op update-count-wise: the UID already has a mapped
    // event and its external_updated_at hasn't moved, so it's skipped
    // rather than duplicated or blindly rewritten.
    let run2 = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports/{import_id}/import"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&run2, StatusCode::OK);
    let run2_body = json_body(run2).await;
    assert_eq!(run2_body["imported"], 0);
    assert_eq!(run2_body["updated"], 0);
    assert_eq!(run2_body["skipped"], 1);

    let mapped: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM calendar_import_events WHERE calendar_import_id = $1::uuid",
    )
    .bind(&import_id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(mapped, 1);

    let events_after = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/events?from=2026-01-01T00:00:00Z&to=2027-01-01T00:00:00Z"),
        Some(&owner_cookie),
        None,
    )
    .await;
    let events_after_body = json_body(events_after).await;
    assert_eq!(
        events_after_body["occurrences"].as_array().unwrap().len(),
        1
    );
}

/// AC: only an admin/owner may delete a calendar-import connection.
#[sqlx::test]
async fn only_admin_or_owner_can_delete_calendar_import(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "cal-owner4@example.test", "owner-password1").await;
    let member_cookie =
        register_verify_login(&router, &db, "cal-member4@example.test", "member-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let invite = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(&owner_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    let token = json_body(invite).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    call(
        &router,
        Method::POST,
        &format!("/groups/invitations/{token}/accept"),
        Some(&member_cookie),
        None,
    )
    .await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "label": "Foyer calendar",
            "feed_url": "https://calendar.google.com/calendar/ical/example/basic.ics"
        })),
    )
    .await;
    let import_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let member_delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/calendar-imports/{import_id}"),
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&member_delete, StatusCode::FORBIDDEN);

    let owner_delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/calendar-imports/{import_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&owner_delete, StatusCode::NO_CONTENT);
}
