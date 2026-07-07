mod common;

use axum::http::{Method, StatusCode};
use chrono::{Duration, Utc};
use common::{assert_status, call, json_body, set_cookie, test_router};
use sqlx::PgPool;
use uuid::Uuid;

fn urlenc(s: &str) -> String {
    s.replace('+', "%2B").replace(':', "%3A")
}

async fn register_verify_login(router: &axum::Router, db: &PgPool, email: &str, password: &str) -> String {
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
    call(router, Method::GET, &format!("/auth/verify-email?token={token}"), None, None).await;
    let login = call(router, Method::POST, "/auth/login", None, Some(serde_json::json!({"email": email, "password": password}))).await;
    set_cookie(&login).unwrap()
}

async fn create_group(router: &axum::Router, cookie: &str, name: &str) -> String {
    let res = call(router, Method::POST, "/groups", Some(cookie), Some(serde_json::json!({"name": name}))).await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

/// AC: full CRUD lifecycle for a one-off event, scoped to a group.
#[sqlx::test]
async fn full_event_lifecycle(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
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

    let get = call(&router, Method::GET, &format!("/groups/{group_id}/events/{event_id}"), Some(&owner_cookie), None).await;
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
        &format!("/groups/{group_id}/events?from={}&to={}", urlenc(&from.to_rfc3339()), urlenc(&to.to_rfc3339())),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&list, StatusCode::OK);
    let list_body = json_body(list).await;
    assert_eq!(list_body["occurrences"].as_array().unwrap().len(), 1);

    let delete = call(&router, Method::DELETE, &format!("/groups/{group_id}/events/{event_id}"), Some(&owner_cookie), None).await;
    assert_status(&delete, StatusCode::NO_CONTENT);

    let get_after_delete = call(&router, Method::GET, &format!("/groups/{group_id}/events/{event_id}"), Some(&owner_cookie), None).await;
    assert_status(&get_after_delete, StatusCode::NOT_FOUND);
}

/// AC: a non-member of the group cannot read or write its events, even
/// with a valid session for another account.
#[sqlx::test]
async fn non_member_cannot_access_group_events(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(&router, &db, "owner2@example.test", "owner-password1").await;
    let outsider_cookie = register_verify_login(&router, &db, "outsider@example.test", "outsider-password1").await;
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
    let owner_cookie = register_verify_login(&router, &db, "recur@example.test", "recur-password1").await;
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
        &format!("/groups/{group_id}/events?from={}&to={}", urlenc(&from.to_rfc3339()), urlenc(&to.to_rfc3339())),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&list, StatusCode::OK);
    let occurrences = json_body(list).await["occurrences"].as_array().unwrap().len();
    assert_eq!(occurrences, 6, "COUNT=6 must yield exactly 6 occurrences");

    // Only the base row exists in the DB — occurrences are expanded on read.
    let row_count: i64 = sqlx::query_scalar!("SELECT count(*) FROM events").fetch_one(&db).await.unwrap().unwrap();
    assert_eq!(row_count, 1);
}

/// AC: an invalid RRULE is rejected at write time (400), not silently
/// swallowed into an empty expansion later.
#[sqlx::test]
async fn invalid_rrule_is_rejected(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(&router, &db, "badrrule@example.test", "password1234").await;
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
    let owner_cookie = register_verify_login(&router, &db, "reminder@example.test", "password1234").await;
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
    let event_id: Uuid = json_body(create).await["id"].as_str().unwrap().parse().unwrap();

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
    assert!((notifications[0].occurrence_at - starts_at).num_seconds().abs() < 2);
    assert_eq!(notifications[0].status, "pending");
    let expected_fire_at = starts_at - Duration::minutes(60);
    assert!((notifications[0].fire_at - expected_fire_at).num_seconds().abs() < 2);
}

/// AC: tasks-as-events — `completed` can only be toggled on an
/// `is_task` event, and setting it stamps `completed_at`.
#[sqlx::test]
async fn task_completion_toggles_completed_at(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(&router, &db, "task@example.test", "password1234").await;
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
    let regular_id = json_body(regular_create).await["id"].as_str().unwrap().to_string();
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
    let owner_cookie = register_verify_login(&router, &db, "recur-task@example.test", "password1234").await;
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
        &format!("/groups/{group_id}/events?from={}&to={}", urlenc(&from.to_rfc3339()), urlenc(&to.to_rfc3339())),
        Some(&owner_cookie),
        None,
    )
    .await;
    let occurrences = json_body(list).await["occurrences"].as_array().unwrap().clone();
    assert_eq!(occurrences.len(), 4);
    for occurrence in &occurrences {
        let occurrence_starts_at: chrono::DateTime<Utc> =
            occurrence["occurrence_starts_at"].as_str().unwrap().parse().unwrap();
        let is_second = occurrence_starts_at == second_occurrence;
        assert_eq!(
            occurrence["completed_at"].is_string(),
            is_second,
            "only the completed occurrence should report a completed_at"
        );
    }
}
