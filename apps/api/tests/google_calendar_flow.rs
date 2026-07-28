mod common;

use axum::http::{Method, StatusCode};
use common::{
    assert_status, call, call_upload, json_body, real_minio_from_env, set_cookie, test_router,
    test_router_with_storage,
};
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

/// PNG magic bytes — `sniff_and_validate_mime` reads the signature rather
/// than decoding the image, so this is all an upload needs.
const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";

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
    assert_status(&owner_delete, StatusCode::OK);
    // Nothing was imported, and the events were not asked for anyway.
    assert_eq!(json_body(owner_delete).await["deleted_events"], 0);
}

/// Imports `ICS_BODY` through a fresh connection and returns
/// `(import_id, event_id)`. Every test below starts from the same place:
/// one connection that has actually run, so there is something to keep or
/// delete.
async fn import_one_event(
    router: &axum::Router,
    db: &PgPool,
    owner_cookie: &str,
    group_id: &str,
) -> (String, String) {
    let feed_url = spawn_ics_server(ICS_BODY, 1).await;
    let create = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports"),
        Some(owner_cookie),
        Some(serde_json::json!({"label": "Foyer calendar", "feed_url": feed_url})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let import_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let run = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports/{import_id}/import"),
        Some(owner_cookie),
        None,
    )
    .await;
    assert_status(&run, StatusCode::OK);
    assert_eq!(json_body(run).await["imported"], 1);

    let event_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT event_id FROM calendar_import_events WHERE calendar_import_id = $1::uuid",
    )
    .bind(&import_id)
    .fetch_one(db)
    .await
    .unwrap();
    (import_id, event_id.to_string())
}

async fn event_count(router: &axum::Router, cookie: &str, group_id: &str) -> usize {
    let events = call(
        router,
        Method::GET,
        &format!("/groups/{group_id}/events?from=2026-01-01T00:00:00Z&to=2027-01-01T00:00:00Z"),
        Some(cookie),
        None,
    )
    .await;
    assert_status(&events, StatusCode::OK);
    json_body(events).await["occurrences"]
        .as_array()
        .unwrap()
        .len()
}

/// AC (#55): asked for it, the delete takes the events the import created
/// with it — the bulk cleanup that otherwise has to be done one event at a
/// time through `/agenda/:id`.
#[sqlx::test]
async fn deleting_a_connection_removes_its_imported_events_when_asked(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "cal-owner5@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let (import_id, _) = import_one_event(&router, &db, &owner_cookie, &group_id).await;
    assert_eq!(event_count(&router, &owner_cookie, &group_id).await, 1);

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/calendar-imports/{import_id}?delete_events=true"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::OK);
    assert_eq!(
        json_body(delete).await["deleted_events"],
        1,
        "the response has to report what it removed, the front says so"
    );

    assert_eq!(
        event_count(&router, &owner_cookie, &group_id).await,
        0,
        "the imported event should be gone from the agenda, not just its mapping"
    );
    let imports = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/calendar-imports"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_eq!(
        json_body(imports).await["imports"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "the connection itself must still be gone"
    );
}

/// AC (#55): the flag is opt-in. Without it the v1 behaviour stands — the
/// events survive as ordinary family events, because they may carry local
/// work (a reminder, an attachment, a completion) with no Google
/// counterpart. Pinned so this branch never silently becomes the other.
#[sqlx::test]
async fn deleting_a_connection_keeps_its_imported_events_by_default(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "cal-owner6@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let (import_id, _) = import_one_event(&router, &db, &owner_cookie, &group_id).await;

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/calendar-imports/{import_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::OK);
    assert_eq!(json_body(delete).await["deleted_events"], 0);

    assert_eq!(
        event_count(&router, &owner_cookie, &group_id).await,
        1,
        "the default must still leave the imported events in the agenda"
    );
}

/// `delete_events=false` is the same branch as no flag at all — a front
/// that submits an unticked checkbox as `false` must not delete anything.
#[sqlx::test]
async fn an_explicit_false_keeps_the_events_too(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "cal-owner7@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let (import_id, _) = import_one_event(&router, &db, &owner_cookie, &group_id).await;

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/calendar-imports/{import_id}?delete_events=false"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::OK);
    assert_eq!(json_body(delete).await["deleted_events"], 0);
    assert_eq!(event_count(&router, &owner_cookie, &group_id).await, 1);
}

/// AC (#55): the bulk delete must not re-open #54 at feed scale. An
/// imported event that picked up an attachment locally leaves no object
/// behind when this path removes it.
///
/// Needs a real MinIO; skipped otherwise (see `real_minio_from_env`).
#[sqlx::test]
async fn deleting_the_imported_events_removes_their_attachment_objects(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping deleting_the_imported_events_removes_their_attachment_objects: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let router = test_router_with_storage(
        db.clone(),
        manage_our_home::storage::Storage::new(s3.clone(), bucket.clone()),
    );
    let owner_cookie =
        register_verify_login(&router, &db, "cal-owner8@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let (import_id, event_id) = import_one_event(&router, &db, &owner_cookie, &group_id).await;

    // Local work on an imported event, with no Google counterpart: exactly
    // what the opt-in is warning about — and its bytes live in MinIO.
    let upload = call_upload(
        &router,
        &format!("/groups/{group_id}/events/{event_id}/attachments"),
        &owner_cookie,
        "ordonnance.png",
        PNG_BYTES,
    )
    .await;
    assert_status(&upload, StatusCode::CREATED);
    let storage_key: String =
        sqlx::query_scalar("SELECT storage_key FROM event_attachments WHERE event_id = $1::uuid")
            .bind(&event_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(
        s3.head_object()
            .bucket(&bucket)
            .key(&storage_key)
            .send()
            .await
            .is_ok(),
        "the uploaded object should exist before the delete"
    );

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/calendar-imports/{import_id}?delete_events=true"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::OK);
    assert_eq!(json_body(delete).await["deleted_events"], 1);

    assert!(
        s3.head_object()
            .bucket(&bucket)
            .key(&storage_key)
            .send()
            .await
            .is_err(),
        "deleting the imported events should have taken their attachment objects too"
    );
}

/// Pins the ordering, as the per-event and per-group deletes do: objects
/// go first, so a storage failure aborts the whole delete rather than
/// leaving a removed connection whose bytes stay in the bucket.
/// `test_router`'s storage points at an unreachable endpoint.
#[sqlx::test]
async fn a_failed_object_delete_aborts_the_whole_connection_delete(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "cal-owner9@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let (import_id, event_id) = import_one_event(&router, &db, &owner_cookie, &group_id).await;

    let user_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind("cal-owner9@example.test")
        .fetch_one(&db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO event_attachments (event_id, uploaded_by, storage_key, filename, mime_type, size_bytes)
         VALUES ($1::uuid, $2, $3, 'ordonnance.png', 'image/png', 42)",
    )
    .bind(&event_id)
    .bind(user_id)
    .bind(format!("{group_id}/{event_id}/{}", uuid::Uuid::new_v4()))
    .execute(&db)
    .await
    .unwrap();

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/calendar-imports/{import_id}?delete_events=true"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::INTERNAL_SERVER_ERROR);

    assert_eq!(
        event_count(&router, &owner_cookie, &group_id).await,
        1,
        "a failed object delete must leave the event for the retry"
    );
    let imports = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/calendar-imports"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_eq!(
        json_body(imports).await["imports"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "…and the connection too, or the retry has nothing to retry through"
    );
}
