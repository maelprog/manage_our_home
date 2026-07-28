mod common;

use axum::http::{Method, StatusCode};
use chrono::{Duration, Utc};
use common::{
    assert_status, call, call_upload, json_body, real_minio_from_env, set_cookie, test_router,
    test_router_with_storage,
};
use sqlx::PgPool;
use uuid::Uuid;

fn urlenc(s: &str) -> String {
    s.replace('+', "%2B").replace(':', "%3A")
}

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

/// AC: full CRUD lifecycle for a one-off event, scoped to a group.
#[sqlx::test]
async fn full_event_lifecycle(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let starts_at = Utc::now() + Duration::days(1);
    let ends_at = starts_at + Duration::hours(1);
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "Rendez-vous médecin",
            "starts_at": starts_at,
            "ends_at": ends_at,
        })),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let event = json_body(create).await;
    let event_id = event["id"].as_str().unwrap().to_string();
    assert_eq!(event["title"], "Rendez-vous médecin");

    let get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&get, StatusCode::OK);

    let update = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"title": "Rendez-vous dentiste"})),
    )
    .await;
    assert_status(&update, StatusCode::OK);
    let updated = json_body(update).await;
    assert_eq!(updated["title"], "Rendez-vous dentiste");

    let from = starts_at - Duration::hours(1);
    let to = starts_at + Duration::hours(2);
    let list = call(
        &router,
        Method::GET,
        &format!(
            "/groups/{group_id}/events?from={}&to={}",
            urlenc(&from.to_rfc3339()),
            urlenc(&to.to_rfc3339())
        ),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&list, StatusCode::OK);
    let list_body = json_body(list).await;
    assert_eq!(list_body["occurrences"].as_array().unwrap().len(), 1);

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::NO_CONTENT);

    let get_after_delete = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&get_after_delete, StatusCode::NOT_FOUND);
}

/// AC: a non-member of the group cannot read or write its events, even
/// with a valid session for another account.
#[sqlx::test]
async fn non_member_cannot_access_group_events(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner2@example.test", "owner-password1").await;
    let outsider_cookie =
        register_verify_login(&router, &db, "outsider@example.test", "outsider-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let starts_at = Utc::now() + Duration::days(1);
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&owner_cookie),
        Some(serde_json::json!({"title": "Privé", "starts_at": starts_at, "ends_at": starts_at + Duration::hours(1)})),
    )
    .await;
    let event_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let outsider_get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&outsider_cookie),
        None,
    )
    .await;
    assert_status(&outsider_get, StatusCode::FORBIDDEN);

    let outsider_create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&outsider_cookie),
        Some(serde_json::json!({"title": "Intrusion", "starts_at": starts_at, "ends_at": starts_at + Duration::hours(1)})),
    )
    .await;
    assert_status(&outsider_create, StatusCode::FORBIDDEN);
}

/// AC: a weekly RRULE expands into the expected number of occurrences
/// within the requested window, without materializing extra DB rows.
#[sqlx::test]
async fn recurring_event_expands_within_range(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "recur@example.test", "recur-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let starts_at = Utc::now() + Duration::days(1);
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "Cours de piano",
            "starts_at": starts_at,
            "ends_at": starts_at + Duration::minutes(30),
            "rrule": "FREQ=WEEKLY;COUNT=6",
        })),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);

    let from = starts_at - Duration::hours(1);
    let to = starts_at + Duration::weeks(10);
    let list = call(
        &router,
        Method::GET,
        &format!(
            "/groups/{group_id}/events?from={}&to={}",
            urlenc(&from.to_rfc3339()),
            urlenc(&to.to_rfc3339())
        ),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&list, StatusCode::OK);
    let occurrences = json_body(list).await["occurrences"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(occurrences, 6, "COUNT=6 must yield exactly 6 occurrences");

    // Only the base row exists in the DB — occurrences are expanded on read.
    let row_count: i64 = sqlx::query_scalar!("SELECT count(*) FROM events")
        .fetch_one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row_count, 1);
}

/// AC: an invalid RRULE is rejected at write time (400), not silently
/// swallowed into an empty expansion later.
#[sqlx::test]
async fn invalid_rrule_is_rejected(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "badrrule@example.test", "test-password-1234").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let starts_at = Utc::now() + Duration::days(1);
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "Invalide",
            "starts_at": starts_at,
            "ends_at": starts_at + Duration::hours(1),
            "rrule": "NOT_A_VALID_RRULE",
        })),
    )
    .await;
    assert_status(&create, StatusCode::BAD_REQUEST);
}

/// AC: creating a reminder materializes a `scheduled_notifications` row
/// with the correct `fire_at` (occurrence start minus the offset).
#[sqlx::test]
async fn reminder_creates_scheduled_notification(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "reminder@example.test", "test-password-1234").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let starts_at = Utc::now() + Duration::days(1);
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&owner_cookie),
        Some(serde_json::json!({"title": "Anniversaire", "starts_at": starts_at, "ends_at": starts_at + Duration::hours(1)})),
    )
    .await;
    let event_id: Uuid = json_body(create).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let reminder = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events/{event_id}/reminders"),
        Some(&owner_cookie),
        Some(serde_json::json!({"offset_minutes": 60})),
    )
    .await;
    assert_status(&reminder, StatusCode::CREATED);

    let notifications = sqlx::query!(
        "SELECT fire_at, occurrence_at, status FROM scheduled_notifications WHERE event_id = $1",
        event_id
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(notifications.len(), 1);
    assert!(
        (notifications[0].occurrence_at - starts_at)
            .num_seconds()
            .abs()
            < 2
    );
    assert_eq!(notifications[0].status, "pending");
    let expected_fire_at = starts_at - Duration::minutes(60);
    assert!(
        (notifications[0].fire_at - expected_fire_at)
            .num_seconds()
            .abs()
            < 2
    );
}

/// AC: tasks-as-events — `completed` can only be toggled on an
/// `is_task` event, and setting it stamps `completed_at`.
#[sqlx::test]
async fn task_completion_toggles_completed_at(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "task@example.test", "test-password-1234").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let starts_at = Utc::now() + Duration::days(1);
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "Faire les courses",
            "starts_at": starts_at,
            "ends_at": starts_at + Duration::hours(1),
            "is_task": true,
        })),
    )
    .await;
    let event_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let complete = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"completed": true})),
    )
    .await;
    assert_status(&complete, StatusCode::OK);
    let body = json_body(complete).await;
    assert!(body["completed_at"].is_string());

    // A non-task event cannot be marked completed.
    let regular_create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&owner_cookie),
        Some(serde_json::json!({"title": "Cinéma", "starts_at": starts_at, "ends_at": starts_at + Duration::hours(2)})),
    )
    .await;
    let regular_id = json_body(regular_create).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let bad_complete = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/events/{regular_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"completed": true})),
    )
    .await;
    assert_status(&bad_complete, StatusCode::BAD_REQUEST);
}

/// Regression test: completing one occurrence of a recurring task must not
/// mark every other occurrence of the series as completed too (previously
/// `completed_at` lived on the single `events` row shared by the whole
/// series).
#[sqlx::test]
async fn recurring_task_completion_is_per_occurrence(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "recur-task@example.test",
        "test-password-1234",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let starts_at = Utc::now() + Duration::days(1);
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "Sortir les poubelles",
            "starts_at": starts_at,
            "ends_at": starts_at + Duration::minutes(10),
            "is_task": true,
            "rrule": "FREQ=WEEKLY;COUNT=4",
        })),
    )
    .await;
    let event_id = json_body(create).await["id"].as_str().unwrap().to_string();

    // occurrence_at is required when completing a recurring task.
    let missing_occurrence = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"completed": true})),
    )
    .await;
    assert_status(&missing_occurrence, StatusCode::BAD_REQUEST);

    let second_occurrence = starts_at + Duration::weeks(1);
    let complete_second = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"completed": true, "occurrence_at": second_occurrence})),
    )
    .await;
    assert_status(&complete_second, StatusCode::OK);

    let from = starts_at - Duration::hours(1);
    let to = starts_at + Duration::weeks(10);
    let list = call(
        &router,
        Method::GET,
        &format!(
            "/groups/{group_id}/events?from={}&to={}",
            urlenc(&from.to_rfc3339()),
            urlenc(&to.to_rfc3339())
        ),
        Some(&owner_cookie),
        None,
    )
    .await;
    let occurrences = json_body(list).await["occurrences"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(occurrences.len(), 4);
    for occurrence in &occurrences {
        let occurrence_starts_at: chrono::DateTime<Utc> = occurrence["occurrence_starts_at"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let is_second = occurrence_starts_at == second_occurrence;
        assert_eq!(
            occurrence["completed_at"].is_string(),
            is_second,
            "only the completed occurrence should report a completed_at"
        );
    }
}

/// PNG magic bytes. `sniff_and_validate_mime` reads the signature rather
/// than decoding the image, so this is all an upload needs to clear the
/// MIME allow-list.
const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";

/// `event_attachments` is RLS-scoped through its event's group, and the
/// policy applies to the pool's own role (FORCE ROW LEVEL SECURITY), so
/// test-side reads and writes have to set `app.family_id` the way
/// `scoped_tx` does. Runtime `sqlx::query` on purpose: test-only SQL that
/// would otherwise need a `.sqlx` offline cache entry.
async fn with_family_scope<'a>(
    db: &PgPool,
    group_id: &str,
) -> sqlx::Transaction<'a, sqlx::Postgres> {
    let mut tx = db.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.family_id', $1, true)")
        .bind(group_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx
}

async fn attachment_storage_key(db: &PgPool, group_id: &str, event_id: &str) -> String {
    let mut tx = with_family_scope(db, group_id).await;
    let key = sqlx::query_scalar("SELECT storage_key FROM event_attachments WHERE event_id = $1")
        .bind(Uuid::parse_str(event_id).unwrap())
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    key
}

async fn attachment_count(db: &PgPool, group_id: &str, event_id: &str) -> i64 {
    let mut tx = with_family_scope(db, group_id).await;
    let count = sqlx::query_scalar("SELECT count(*) FROM event_attachments WHERE event_id = $1")
        .bind(Uuid::parse_str(event_id).unwrap())
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    count
}

async fn create_event(router: &axum::Router, cookie: &str, group_id: &str) -> String {
    let starts_at = Utc::now() + Duration::days(1);
    let res = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(cookie),
        Some(serde_json::json!({
            "title": "Réunion de classe",
            "starts_at": starts_at,
            "ends_at": starts_at + Duration::hours(1),
        })),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

/// AC (#54): deleting an event deletes the objects behind its attachments,
/// not just the rows. `event_attachments` cascades from `events`, so the
/// rows go on their own — nothing ever reads `storage_key` again, which is
/// what made the leak unfindable.
///
/// Needs a real MinIO; skipped otherwise (see `real_minio_from_env`).
#[sqlx::test]
async fn deleting_an_event_removes_its_attachment_objects(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping deleting_an_event_removes_its_attachment_objects: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let router = test_router_with_storage(
        db.clone(),
        manage_our_home::storage::Storage::new(s3.clone(), bucket.clone()),
    );
    let owner_cookie =
        register_verify_login(&router, &db, "attach-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let event_id = create_event(&router, &owner_cookie, &group_id).await;

    let upload = call_upload(
        &router,
        &format!("/groups/{group_id}/events/{event_id}/attachments"),
        &owner_cookie,
        "ordonnance.png",
        PNG_BYTES,
    )
    .await;
    assert_status(&upload, StatusCode::CREATED);

    let storage_key = attachment_storage_key(&db, &group_id, &event_id).await;
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
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::NO_CONTENT);

    assert_eq!(attachment_count(&db, &group_id, &event_id).await, 0);
    assert!(
        s3.head_object()
            .bucket(&bucket)
            .key(&storage_key)
            .send()
            .await
            .is_err(),
        "deleting the event should have removed its attachment object from storage, \
         not just the row pointing at it"
    );
}

/// AC (#54): pins the ordering choice. Objects go first, the event row
/// after, so a storage failure aborts the whole delete: the caller sees an
/// error and can retry against an event that is still there, rather than a
/// "deleted" event whose bytes are unreachable forever.
///
/// `test_router`'s storage points at an unreachable endpoint, which is
/// exactly the failure being pinned here.
#[sqlx::test]
async fn event_delete_aborts_when_the_attachment_object_cannot_be_removed(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "leak-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let event_id = create_event(&router, &owner_cookie, &group_id).await;

    // Insert the attachment row directly: the upload path would need a
    // reachable MinIO, and this test wants an unreachable one.
    let user_id: Uuid = sqlx::query_scalar!(
        "SELECT id FROM users WHERE email = $1",
        "leak-owner@example.test"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let mut tx = with_family_scope(&db, &group_id).await;
    sqlx::query(
        "INSERT INTO event_attachments (event_id, uploaded_by, storage_key, filename, mime_type, size_bytes)
         VALUES ($1, $2, $3, 'ordonnance.png', 'image/png', 42)",
    )
    .bind(Uuid::parse_str(&event_id).unwrap())
    .bind(user_id)
    .bind(format!("{group_id}/{event_id}/{}", Uuid::new_v4()))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::INTERNAL_SERVER_ERROR);

    let get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&get, StatusCode::OK);
    assert_eq!(
        attachment_count(&db, &group_id, &event_id).await,
        1,
        "a failed object delete must leave the attachment row for the retry"
    );
}
