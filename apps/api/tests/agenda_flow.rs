mod common;

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Duration, TimeZone, Utc};
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
    // #73: no `assignee_ids` in the request defaults to the creator.
    let owner_id: Uuid = sqlx::query_scalar!(
        "SELECT id FROM users WHERE email = $1",
        "owner@example.test"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(
        event["assignee_ids"].as_array().unwrap(),
        &[serde_json::json!(owner_id)]
    );

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

/// One RFC 3339 field of an event JSON body, as the instant it names.
fn instant(body: &serde_json::Value, field: &str) -> DateTime<Utc> {
    body[field]
        .as_str()
        .unwrap_or_else(|| panic!("{field} missing from {body}"))
        .parse::<DateTime<Utc>>()
        .unwrap()
}

/// #101: `all_day` is an invariant on the stored row, not a display flag.
/// The form's two `datetime-local` fields default to "now" and "now + 1 h",
/// so a birthday ticked "journée entière" used to be stored as the 08:00 →
/// 09:00 slot it was filled with and read as *finished* at 09:01 — the
/// dashboard keeps occurrences by `occurrence_ends_at` (#73). Both write
/// endpoints now store whole Europe/Paris civil days, and a later PATCH
/// that names neither timestamp must not drift them.
#[sqlx::test]
async fn an_all_day_event_is_stored_as_whole_paris_days(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    // 2026-09-03, 08:00 → 09:00 as the form would submit it. Paris is UTC+2
    // in September, so the civil day runs 09-02T22:00Z → 09-03T22:00Z.
    let asked_start = Utc.with_ymd_and_hms(2026, 9, 3, 6, 0, 0).unwrap();
    let asked_end = Utc.with_ymd_and_hms(2026, 9, 3, 7, 0, 0).unwrap();
    let day_start = Utc.with_ymd_and_hms(2026, 9, 2, 22, 0, 0).unwrap();
    let day_end = Utc.with_ymd_and_hms(2026, 9, 3, 22, 0, 0).unwrap();

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "Anniversaire de Léa",
            "starts_at": asked_start,
            "ends_at": asked_end,
            "all_day": true,
        })),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let event = json_body(create).await;
    let event_id = event["id"].as_str().unwrap().to_string();
    assert_eq!(instant(&event, "starts_at"), day_start);
    assert_eq!(instant(&event, "ends_at"), day_end);

    // A PATCH naming neither the flag nor the timestamps re-runs the
    // normalization on the row's own values: it must be a no-op, not a
    // one-day-per-edit drift.
    let retitled = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"title": "Anniversaire de Camille"})),
    )
    .await;
    assert_status(&retitled, StatusCode::OK);
    let retitled = json_body(retitled).await;
    assert_eq!(instant(&retitled, "starts_at"), day_start);
    assert_eq!(instant(&retitled, "ends_at"), day_end);

    // Editing the times while the flag stays on re-normalizes to the new day.
    let moved = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "starts_at": Utc.with_ymd_and_hms(2026, 9, 10, 14, 30, 0).unwrap(),
            "ends_at": Utc.with_ymd_and_hms(2026, 9, 10, 15, 30, 0).unwrap(),
        })),
    )
    .await;
    assert_status(&moved, StatusCode::OK);
    let moved = json_body(moved).await;
    assert_eq!(
        instant(&moved, "starts_at"),
        Utc.with_ymd_and_hms(2026, 9, 9, 22, 0, 0).unwrap()
    );
    assert_eq!(
        instant(&moved, "ends_at"),
        Utc.with_ymd_and_hms(2026, 9, 10, 22, 0, 0).unwrap()
    );

    // Unticking the box hands the timestamps back verbatim: normalization
    // applies to `all_day` rows and nothing else.
    let untick = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "all_day": false,
            "starts_at": asked_start,
            "ends_at": asked_end,
        })),
    )
    .await;
    assert_status(&untick, StatusCode::OK);
    let untick = json_body(untick).await;
    assert_eq!(instant(&untick, "starts_at"), asked_start);
    assert_eq!(instant(&untick, "ends_at"), asked_end);
}

/// #101: a backwards range is still a 400 on an `all_day` event — the
/// normalization runs *after* validation, so it repairs the day boundaries
/// of a sane request rather than papering over a nonsensical one.
#[sqlx::test]
async fn an_all_day_event_with_a_backwards_range_is_still_rejected(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "À l'envers",
            "starts_at": Utc.with_ymd_and_hms(2026, 9, 3, 6, 0, 0).unwrap(),
            "ends_at": Utc.with_ymd_and_hms(2026, 9, 1, 6, 0, 0).unwrap(),
            "all_day": true,
        })),
    )
    .await;
    assert_status(&create, StatusCode::BAD_REQUEST);
}

/// #73: an event can be assigned to several family members, and the
/// assignment can be changed on update; an assignee id that isn't actually
/// a member of the family is dropped rather than accepted verbatim.
#[sqlx::test]
async fn event_assignment_to_several_members(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "assign-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let owner_id: Uuid = sqlx::query_scalar!(
        "SELECT id FROM users WHERE email = $1",
        "assign-owner@example.test"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    // A second member, added directly (no invitation flow needed for this
    // test — same shortcut `event_delete_aborts_when_the_attachment_object_
    // cannot_be_removed` takes for the row it needs).
    let member_cookie = register_verify_login(
        &router,
        &db,
        "assign-member@example.test",
        "member-password1",
    )
    .await;
    let member_id: Uuid = sqlx::query_scalar!(
        "SELECT id FROM users WHERE email = $1",
        "assign-member@example.test"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let mut tx = with_family_scope(&db, &group_id).await;
    sqlx::query("INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'standard')")
        .bind(Uuid::parse_str(&group_id).unwrap())
        .bind(member_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let _ = &member_cookie; // only its side effect (membership row) matters here

    let starts_at = Utc::now() + Duration::days(1);
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "Sortie vélo",
            "starts_at": starts_at,
            "ends_at": starts_at + Duration::hours(1),
            "assignee_ids": [owner_id, member_id],
        })),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let event = json_body(create).await;
    let event_id = event["id"].as_str().unwrap().to_string();
    let mut assignees: Vec<Uuid> = event["assignee_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| Uuid::parse_str(v.as_str().unwrap()).unwrap())
        .collect();
    assignees.sort();
    let mut expected = [owner_id, member_id];
    expected.sort();
    assert_eq!(assignees, expected);

    // An outsider id isn't a member of this family: it's dropped rather
    // than accepted, and since that leaves nothing, the update falls back
    // to the creator (`resolve_assignees`).
    let outsider = Uuid::new_v4();
    let update = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"assignee_ids": [outsider]})),
    )
    .await;
    assert_status(&update, StatusCode::OK);
    let updated = json_body(update).await;
    assert_eq!(
        updated["assignee_ids"].as_array().unwrap(),
        &[serde_json::json!(owner_id)]
    );

    // Omitting `assignee_ids` entirely on a further update leaves the
    // (just-reset-to-creator) assignment untouched.
    let noop_update = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"title": "Sortie à vélo"})),
    )
    .await;
    assert_status(&noop_update, StatusCode::OK);
    let noop_body = json_body(noop_update).await;
    assert_eq!(
        noop_body["assignee_ids"].as_array().unwrap(),
        &[serde_json::json!(owner_id)]
    );
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

/// AC (#62): pins the upload ordering. The row is written inside the
/// transaction *before* the object exists, so a storage failure has to
/// take the row down with it — the transaction is dropped, never
/// committed, and the caller sees a 500 against an event with no
/// attachment rather than a row pointing at bytes that were never stored.
///
/// This is a regression guard, not a red-first test: the previous ordering
/// (object first) also left no row here, because it failed at
/// `put_object` before reaching the INSERT. What the guard catches is a
/// future edit that commits the row before the object is confirmed
/// written — verified by mutation, see the PR.
///
/// `test_router`'s storage points at an unreachable endpoint, which is
/// exactly the failure being pinned.
#[sqlx::test]
async fn an_upload_that_cannot_reach_storage_leaves_no_attachment_row(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "upload-nostorage@example.test",
        "owner-password1",
    )
    .await;
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
    assert_status(&upload, StatusCode::INTERNAL_SERVER_ERROR);

    assert_eq!(
        attachment_count(&db, &group_id, &event_id).await,
        0,
        "a row must never outlive the object it points at: the upload failed, \
         so the transaction carrying the row has to roll back with it"
    );

    // The event itself is untouched — only the attachment failed.
    let get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&get, StatusCode::OK);
}

/// The other half of the ordering guard: with a reachable bucket the
/// reordered path still stores the bytes and still commits the row. A
/// rollback that fired on the happy path would be invisible to the test
/// above, which only ever sees failures.
///
/// Needs a real MinIO; skipped otherwise (see `real_minio_from_env`).
#[sqlx::test]
async fn a_successful_upload_stores_the_object_and_commits_the_row(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping a_successful_upload_stores_the_object_and_commits_the_row: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let router = test_router_with_storage(
        db.clone(),
        manage_our_home::storage::Storage::new(s3.clone(), bucket.clone()),
    );
    let owner_cookie =
        register_verify_login(&router, &db, "upload-ok@example.test", "owner-password1").await;
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

    assert_eq!(attachment_count(&db, &group_id, &event_id).await, 1);
    let storage_key = attachment_storage_key(&db, &group_id, &event_id).await;
    assert!(
        s3.head_object()
            .bucket(&bucket)
            .key(&storage_key)
            .send()
            .await
            .is_ok(),
        "the committed row must point at an object that is actually there"
    );
}
