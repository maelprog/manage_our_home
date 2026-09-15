mod common;

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Utc};
use common::{
    assert_status, call, call_upload, json_body, real_minio_from_env, set_cookie, test_router,
    test_router_with_storage,
};
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uuid::Uuid;

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

/// An all-day VEVENT, the shape Google emits for one: both bounds are
/// `VALUE=DATE`, and RFC 5545's DTEND is exclusive — so this names 1 June
/// 2026 and nothing else. `parse.rs` anchors both dates on midnight **UTC**,
/// which is 02:00 Paris in June; the import re-anchors them (#118).
const ICS_ALL_DAY_BODY: &str = "\
BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:flow-allday-1@google.com
DTSTAMP:20260101T090000Z
DTSTART;VALUE=DATE:20260601
DTEND;VALUE=DATE:20260602
SUMMARY:Anniversaire
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

/// The single event the feed produced, straight from the table — the
/// assertions below are about what was *stored*, which is exactly what the
/// two bugs got wrong, so they read the row rather than the API's view of it.
async fn stored_event(db: &PgPool) -> (DateTime<Utc>, DateTime<Utc>, bool, Vec<Uuid>) {
    let row: (DateTime<Utc>, DateTime<Utc>, bool) =
        sqlx::query_as("SELECT starts_at, ends_at, all_day FROM events")
            .fetch_one(db)
            .await
            .unwrap();
    let assignees: Vec<Uuid> = sqlx::query_scalar("SELECT user_id FROM event_assignees")
        .fetch_all(db)
        .await
        .unwrap();
    (row.0, row.1, row.2, assignees)
}

async fn user_id_of(db: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(db)
        .await
        .unwrap()
}

/// AC (#106): an imported event is assigned to the account that ran the
/// sync — the same one the INSERT already stores in `created_by`.
///
/// The mirror writes `events` directly and never wrote `event_assignees` at
/// all, so every imported event arrived with `assignee_ids = []` and the
/// Agenda rendered it "? —" in `--accent`. This was the third route to an
/// unassigned event and the only permanent one.
#[sqlx::test]
async fn an_imported_event_is_assigned_to_whoever_ran_the_import(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "cal-assign1@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let feed_url = spawn_ics_server(ICS_BODY, 1).await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports"),
        Some(&owner_cookie),
        Some(serde_json::json!({"label": "Foyer calendar", "feed_url": feed_url})),
    )
    .await;
    let import_id = json_body(create).await["id"].as_str().unwrap().to_string();
    let run = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports/{import_id}/import"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&run, StatusCode::OK);

    let owner_id = user_id_of(&db, "cal-assign1@example.test").await;
    let (_, _, _, assignees) = stored_event(&db).await;
    assert_eq!(assignees, vec![owner_id]);

    // And it reaches the reader: the Agenda's assignee pastille is driven by
    // `assignee_ids` off this endpoint.
    let events = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/events?from=2026-01-01T00:00:00Z&to=2027-01-01T00:00:00Z"),
        Some(&owner_cookie),
        None,
    )
    .await;
    let body = json_body(events).await;
    assert_eq!(
        body["occurrences"][0]["assignee_ids"],
        serde_json::json!([owner_id]),
    );
}

/// AC (#118): an all-day event from the feed is stored as the whole Paris
/// civil day it names, not as the UTC-midnight pair `parse.rs` produced.
///
/// The exclusive DTEND of 2 June means the event covers 1 June alone, so the
/// stored pair must be Paris midnight opening 1 June (22:00Z on 31 May, June
/// being CEST) to Paris midnight opening 2 June. Getting this wrong in the
/// other direction — handing the feed's instants to `normalize_all_day`
/// unchanged — yields 22:00Z on 2 June and a birthday that lasts two days.
#[sqlx::test]
async fn an_all_day_import_is_stored_as_a_whole_paris_day(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "cal-allday1@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let feed_url = spawn_ics_server(ICS_ALL_DAY_BODY, 1).await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports"),
        Some(&owner_cookie),
        Some(serde_json::json!({"label": "Foyer calendar", "feed_url": feed_url})),
    )
    .await;
    let import_id = json_body(create).await["id"].as_str().unwrap().to_string();
    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports/{import_id}/import"),
        Some(&owner_cookie),
        None,
    )
    .await;

    let (starts_at, ends_at, all_day, _) = stored_event(&db).await;
    assert!(all_day);
    assert_eq!(starts_at.to_rfc3339(), "2026-05-31T22:00:00+00:00");
    assert_eq!(ends_at.to_rfc3339(), "2026-06-01T22:00:00+00:00");
}

/// Serves `bodies` in order, one per accepted connection — a feed that
/// changes between two syncs. Same wire format as `spawn_ics_server`.
async fn spawn_changing_ics_server(bodies: &'static [&'static str]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for body in bodies {
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
    format!("http://{addr}/changing.ics")
}

/// The feed of the #161 re-import reproduction, before and after Google
/// moves « Garde » from 1 to 25 October. « Réunion » does not move.
const ICS_BEFORE_THE_MOVE: &str = "\
BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:flow-moved-1@google.com
DTSTAMP:20260101T090000Z
DTSTART:20261001T013000Z
DTEND:20261001T023000Z
SUMMARY:Garde
LAST-MODIFIED:20260101T090000Z
END:VEVENT
BEGIN:VEVENT
UID:flow-still-1@google.com
DTSTAMP:20260101T090000Z
DTSTART:20261015T160000Z
DTEND:20261015T170000Z
SUMMARY:Réunion
LAST-MODIFIED:20260101T090000Z
END:VEVENT
END:VCALENDAR
";
const ICS_AFTER_THE_MOVE: &str = "\
BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:flow-moved-1@google.com
DTSTAMP:20260201T090000Z
DTSTART:20261025T013000Z
DTEND:20261025T023000Z
SUMMARY:Garde
LAST-MODIFIED:20260201T090000Z
END:VEVENT
BEGIN:VEVENT
UID:flow-still-1@google.com
DTSTAMP:20260101T090000Z
DTSTART:20261015T160000Z
DTEND:20261015T170000Z
SUMMARY:Réunion
LAST-MODIFIED:20260101T090000Z
END:VEVENT
END:VCALENDAR
";

/// #161, second cause: a re-import rewrites `starts_at` from the feed and
/// leaves a locally added `rrule` alone, without going back through
/// `validate` — so it can move a series past its own `UNTIL`. That series
/// no longer unrolls, and it used to fail `GET /events` for the whole
/// window. The re-import is left as it is; the window answers 200 with
/// every other event, and the moved series is rendered as its own row.
#[sqlx::test]
async fn a_reimport_that_moves_a_series_past_its_until_does_not_take_the_window_down(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "cal-moved1@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let feed_url = spawn_changing_ics_server(&[ICS_BEFORE_THE_MOVE, ICS_AFTER_THE_MOVE]).await;

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
    let import_path = format!("/groups/{group_id}/calendar-imports/{import_id}/import");

    let first = call(
        &router,
        Method::POST,
        &import_path,
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&first, StatusCode::OK);
    assert_eq!(json_body(first).await["imported"], 2);

    let garde_id: Uuid = sqlx::query_scalar("SELECT id FROM events WHERE title = 'Garde'")
        .fetch_one(&db)
        .await
        .unwrap();
    let patch = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/events/{garde_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"rrule": "FREQ=DAILY;UNTIL=20261010T235959Z"})),
    )
    .await;
    assert_status(&patch, StatusCode::OK);

    let second = call(
        &router,
        Method::POST,
        &import_path,
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&second, StatusCode::OK);
    assert_eq!(json_body(second).await["updated"], 1);
    let (moved_to, still_rrule): (DateTime<Utc>, Option<String>) =
        sqlx::query_as("SELECT starts_at, rrule FROM events WHERE id = $1")
            .bind(garde_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(moved_to.to_rfc3339(), "2026-10-25T01:30:00+00:00");
    assert_eq!(
        still_rrule.as_deref(),
        Some("FREQ=DAILY;UNTIL=20261010T235959Z")
    );

    let list = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/events?from=2026-10-01T00:00:00Z&to=2026-10-31T23:59:59Z"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&list, StatusCode::OK);
    let body = json_body(list).await;
    let mut seen: Vec<(String, String)> = body["occurrences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            (
                o["title"].as_str().unwrap().to_string(),
                o["occurrence_starts_at"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    seen.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(
        seen,
        vec![
            ("Réunion".to_string(), "2026-10-15T16:00:00Z".to_string()),
            ("Garde".to_string(), "2026-10-25T01:30:00Z".to_string()),
        ],
        "{body}"
    );
}

/// The feed of the #175 reproduction, before and after Google turns both
/// events into all-day ones. Each keeps its day.
const ICS_BEFORE_ALL_DAY: &str = "\
BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:flow-hourly-1@google.com
DTSTAMP:20260101T090000Z
DTSTART:20260905T090000Z
DTEND:20260905T100000Z
SUMMARY:Relève
LAST-MODIFIED:20260101T090000Z
END:VEVENT
BEGIN:VEVENT
UID:flow-daily-1@google.com
DTSTAMP:20260101T090000Z
DTSTART:20260912T080000Z
DTEND:20260912T090000Z
SUMMARY:Marché
LAST-MODIFIED:20260101T090000Z
END:VEVENT
END:VCALENDAR
";
const ICS_AFTER_ALL_DAY: &str = "\
BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:flow-hourly-1@google.com
DTSTAMP:20260201T090000Z
DTSTART;VALUE=DATE:20260905
DTEND;VALUE=DATE:20260906
SUMMARY:Relève
LAST-MODIFIED:20260201T090000Z
END:VEVENT
BEGIN:VEVENT
UID:flow-daily-1@google.com
DTSTAMP:20260201T090000Z
DTSTART;VALUE=DATE:20260912
DTEND;VALUE=DATE:20260913
SUMMARY:Marché
LAST-MODIFIED:20260201T090000Z
END:VEVENT
END:VCALENDAR
";

/// #175: a re-import rewrites `all_day` from the feed without going through
/// `validate`, so an hour-bound event given `FREQ=HOURLY;COUNT=5` by `PATCH`
/// became an all-day row holding that rule once the feed turned it into a
/// `VALUE=DATE` event — five entries listed on its one day, one reminder,
/// and a 400 on a `PATCH` of its title. The re-import now drops such a rule,
/// and only such a rule: a `FREQ=DAILY` series turned all-day keeps its own.
#[sqlx::test]
async fn a_reimport_turning_a_row_all_day_drops_a_rule_stepping_by_less_than_a_day(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "cal-allday2@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let feed_url = spawn_changing_ics_server(&[ICS_BEFORE_ALL_DAY, ICS_AFTER_ALL_DAY]).await;

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
    let import_path = format!("/groups/{group_id}/calendar-imports/{import_id}/import");

    let first = call(
        &router,
        Method::POST,
        &import_path,
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&first, StatusCode::OK);
    assert_eq!(json_body(first).await["imported"], 2);

    let id_of = |title: &'static str| {
        let db = db.clone();
        async move {
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM events WHERE title = $1")
                .bind(title)
                .fetch_one(&db)
                .await
                .unwrap()
        }
    };
    let (hourly_id, daily_id) = (id_of("Relève").await, id_of("Marché").await);
    for (id, rule) in [
        (hourly_id, "FREQ=HOURLY;COUNT=5"),
        (daily_id, "FREQ=DAILY;COUNT=3"),
    ] {
        let patch = call(
            &router,
            Method::PATCH,
            &format!("/groups/{group_id}/events/{id}"),
            Some(&owner_cookie),
            Some(serde_json::json!({"rrule": rule})),
        )
        .await;
        assert_status(&patch, StatusCode::OK);
    }

    let second = call(
        &router,
        Method::POST,
        &import_path,
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&second, StatusCode::OK);
    assert_eq!(json_body(second).await["updated"], 2);

    let row = |id: Uuid| {
        let db = db.clone();
        async move {
            sqlx::query_as::<_, (bool, Option<String>)>(
                "SELECT all_day, rrule FROM events WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };
    assert_eq!(row(hourly_id).await, (true, None));
    assert_eq!(
        row(daily_id).await,
        (true, Some("FREQ=DAILY;COUNT=3".to_string()))
    );

    let list = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/events?from=2026-09-01T00:00:00Z&to=2026-09-30T23:59:59Z"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&list, StatusCode::OK);
    let body = json_body(list).await;
    let mut seen: Vec<(String, String)> = body["occurrences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            (
                o["title"].as_str().unwrap().to_string(),
                o["occurrence_starts_at"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    seen.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(
        seen,
        vec![
            ("Relève".to_string(), "2026-09-04T22:00:00Z".to_string()),
            ("Marché".to_string(), "2026-09-11T22:00:00Z".to_string()),
            ("Marché".to_string(), "2026-09-12T22:00:00Z".to_string()),
            ("Marché".to_string(), "2026-09-13T22:00:00Z".to_string()),
        ],
        "{body}"
    );

    // The row is an ordinary all-day event again: a `PATCH` keeping its flag
    // is taken.
    let rename = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/events/{hourly_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"title": "Relève du matin"})),
    )
    .await;
    assert_status(&rename, StatusCode::OK);
}

/// AC (#106 + #118): a row written by the *old* import is repaired on the
/// next sync, even though the feed hasn't changed.
///
/// This is the arbitration's second point, and the reason the fix is not
/// only in the two write paths: when `LAST-MODIFIED` hasn't moved, the sync
/// counts the row `skipped` and never rewrites it, so a bad row would stay
/// bad until Google happened to edit that event. The row is put back into
/// its pre-fix shape by hand here — UTC-midnight bounds, no assignee — which
/// is exactly what the mirror used to write.
#[sqlx::test]
async fn the_next_sync_repairs_a_row_the_old_import_wrote(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "cal-repair1@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let feed_url = spawn_ics_server(ICS_ALL_DAY_BODY, 2).await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports"),
        Some(&owner_cookie),
        Some(serde_json::json!({"label": "Foyer calendar", "feed_url": feed_url})),
    )
    .await;
    let import_id = json_body(create).await["id"].as_str().unwrap().to_string();
    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports/{import_id}/import"),
        Some(&owner_cookie),
        None,
    )
    .await;

    sqlx::query(
        "UPDATE events SET starts_at = '2026-06-01T00:00:00Z', ends_at = '2026-06-02T00:00:00Z'",
    )
    .execute(&db)
    .await
    .unwrap();
    sqlx::query("DELETE FROM event_assignees")
        .execute(&db)
        .await
        .unwrap();

    let run = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports/{import_id}/import"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&run, StatusCode::OK);
    let run_body = json_body(run).await;
    // Still reported as unchanged: the counters say what the *feed* did, and
    // the feed did nothing. The repair is the deployment catching up.
    assert_eq!(run_body["imported"], 0);
    assert_eq!(run_body["updated"], 0);
    assert_eq!(run_body["skipped"], 1);

    let owner_id = user_id_of(&db, "cal-repair1@example.test").await;
    let (starts_at, ends_at, _, assignees) = stored_event(&db).await;
    assert_eq!(starts_at.to_rfc3339(), "2026-05-31T22:00:00+00:00");
    assert_eq!(ends_at.to_rfc3339(), "2026-06-01T22:00:00+00:00");
    assert_eq!(assignees, vec![owner_id]);
}

/// AC: the repair converges. A second sync over an already-repaired row must
/// leave it exactly where it is — a transform that re-applied itself would
/// walk the event a day earlier on every sync, which is the failure mode a
/// non-idempotent normalisation would have shipped silently.
#[sqlx::test]
async fn repairing_an_already_repaired_row_is_a_no_op(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "cal-repair2@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let feed_url = spawn_ics_server(ICS_ALL_DAY_BODY, 3).await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports"),
        Some(&owner_cookie),
        Some(serde_json::json!({"label": "Foyer calendar", "feed_url": feed_url})),
    )
    .await;
    let import_id = json_body(create).await["id"].as_str().unwrap().to_string();
    for _ in 0..3 {
        call(
            &router,
            Method::POST,
            &format!("/groups/{group_id}/calendar-imports/{import_id}/import"),
            Some(&owner_cookie),
            None,
        )
        .await;
    }

    let (starts_at, ends_at, _, assignees) = stored_event(&db).await;
    assert_eq!(starts_at.to_rfc3339(), "2026-05-31T22:00:00+00:00");
    assert_eq!(ends_at.to_rfc3339(), "2026-06-01T22:00:00+00:00");
    assert_eq!(assignees.len(), 1);
}

/// AC: the sync fills an assignee in, it never replaces one. An imported
/// event can pick up local work with no Google counterpart — the delete
/// confirmation page (`apps/web/src/routes/agenda/imports.rs`) is built
/// around that fact — so a member's deliberate assignment must survive the
/// next sync rather than being reset to whoever happens to press the button.
#[sqlx::test]
async fn a_sync_does_not_overwrite_an_assignment_made_locally(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "cal-keep1@example.test", "owner-password1").await;
    let member_cookie =
        register_verify_login(&router, &db, "cal-keep2@example.test", "member-password1").await;
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

    let feed_url = spawn_ics_server(ICS_BODY, 2).await;
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports"),
        Some(&owner_cookie),
        Some(serde_json::json!({"label": "Foyer calendar", "feed_url": feed_url})),
    )
    .await;
    let import_id = json_body(create).await["id"].as_str().unwrap().to_string();
    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports/{import_id}/import"),
        Some(&owner_cookie),
        None,
    )
    .await;

    // The family reassigns the imported event to the other member.
    let member_id = user_id_of(&db, "cal-keep2@example.test").await;
    let event_id: Uuid = sqlx::query_scalar("SELECT id FROM events")
        .fetch_one(&db)
        .await
        .unwrap();
    let patch = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({ "assignee_ids": [member_id] })),
    )
    .await;
    assert_status(&patch, StatusCode::OK);

    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/calendar-imports/{import_id}/import"),
        Some(&owner_cookie),
        None,
    )
    .await;

    let (_, _, _, assignees) = stored_event(&db).await;
    assert_eq!(assignees, vec![member_id]);
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
