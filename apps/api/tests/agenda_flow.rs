mod common;

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Duration, TimeZone, Utc};
use common::{
    assert_status, call, call_upload, drop_prescribed_role, json_body, prescribed_role_pool,
    real_minio_from_env, set_cookie, test_router, test_router_with_storage,
};
use manage_our_home::storage::MAX_ATTACHMENT_SIZE_BYTES;
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

/// #101, round 2: the invariant has to hold for every **occurrence** of a
/// recurring all-day event, not just for the stored row.
///
/// Anchoring the row on Paris midnight puts its `starts_at` on the DST
/// cliff (22:00Z in summer, 23:00Z in winter). Unrolled in UTC — which is
/// what this path still does, on a midnight stand-in — every later
/// occurrence would keep September's offset and land at 22:00Z, i.e. 23:00
/// on the *previous* day once the clocks go back. The event then vanishes
/// from a dashboard window that starts at Paris midnight, which is #101's
/// own symptom one level up.
///
/// This is the reproduction from the review of PR #115, verbatim.
#[sqlx::test]
async fn a_recurring_all_day_event_lands_on_its_civil_day_after_the_clocks_change(db: PgPool) {
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
            "title": "Loyer",
            "starts_at": "2026-09-05T06:00:00Z",
            "ends_at": "2026-09-05T07:00:00Z",
            "all_day": true,
            "rrule": "FREQ=MONTHLY",
        })),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let event = json_body(create).await;
    // Paris is UTC+2 in September: the civil day of the 5th.
    assert_eq!(
        instant(&event, "starts_at"),
        Utc.with_ymd_and_hms(2026, 9, 4, 22, 0, 0).unwrap()
    );
    assert_eq!(
        instant(&event, "ends_at"),
        Utc.with_ymd_and_hms(2026, 9, 5, 22, 0, 0).unwrap()
    );

    // The window the dashboard renders on 2026-11-05: it opens at Paris
    // midnight, which is 23:00Z on the 4th now that Paris is UTC+1.
    let from = Utc.with_ymd_and_hms(2026, 11, 4, 23, 0, 0).unwrap();
    let to = Utc.with_ymd_and_hms(2026, 11, 7, 22, 59, 59).unwrap();
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
    let body = json_body(list).await;
    let occurrences = body["occurrences"].as_array().unwrap();
    assert_eq!(
        occurrences.len(),
        1,
        "the November occurrence is missing from the window that opens on its own day: {body}"
    );
    let occ = &occurrences[0];
    assert_eq!(instant(occ, "occurrence_starts_at"), from);
    assert_eq!(
        instant(occ, "occurrence_ends_at"),
        Utc.with_ymd_and_hms(2026, 11, 5, 23, 0, 0).unwrap()
    );
}

/// #116: the same promise for an **hour-bound** recurring event, end to
/// end. « Tous les mois à 9 h » has to keep reading 09:00 in Paris once the
/// clocks go back, `occurrence_ends_at` included — the reproduction
/// measured on a live stack at the verification of #115, replayed here
/// against a real database and through the whole `GET /events` path, which
/// is where the occurrence's end is derived from the stored duration.
#[sqlx::test]
async fn a_recurring_hourly_event_keeps_its_paris_wall_clock_after_the_clocks_change(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    // 09:00 → 10:00 Paris on 2026-09-05, where Paris is UTC+2.
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "Point famille",
            "starts_at": "2026-09-05T07:00:00Z",
            "ends_at": "2026-09-05T08:00:00Z",
            "all_day": false,
            "rrule": "FREQ=MONTHLY",
        })),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);

    // November, where Paris is UTC+1: 09:00 local is 08:00Z. Unrolled in
    // UTC the occurrence came back at 07:00Z — 08:00 in Paris.
    let from = Utc.with_ymd_and_hms(2026, 11, 4, 23, 0, 0).unwrap();
    let to = Utc.with_ymd_and_hms(2026, 11, 7, 22, 59, 59).unwrap();
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
    let body = json_body(list).await;
    let occurrences = body["occurrences"].as_array().unwrap();
    assert_eq!(
        occurrences.len(),
        1,
        "expected one November occurrence: {body}"
    );
    let occ = &occurrences[0];
    assert_eq!(
        instant(occ, "occurrence_starts_at"),
        Utc.with_ymd_and_hms(2026, 11, 5, 8, 0, 0).unwrap()
    );
    assert_eq!(
        instant(occ, "occurrence_ends_at"),
        Utc.with_ymd_and_hms(2026, 11, 5, 9, 0, 0).unwrap()
    );
}

/// #116, round 2: a series anchored on the **repeated hour** must survive
/// both ends of the round trip.
///
/// Paris repeats 02:30 on 2026-10-25 — 00:30Z in CEST, then 01:30Z in CET —
/// and the web form reaches the first of the two (`paris_local_to_utc`
/// resolves with `earliest()`). Naming that hour as a bare wall clock is
/// ambiguous, and `rrule` rejects an ambiguous `DTSTART;TZID=` rather than
/// picking a side: unrolling from a formatted wall clock turned a « garde
/// de nuit, 02:30, tous les mois » into a 400 on write, and into a **500 on
/// the whole window** on read, because `list_events` reports an expansion
/// failure as `AppError::Internal` for every event in the range at once.
#[sqlx::test]
async fn a_recurring_event_anchored_on_the_repeated_hour_survives_write_and_read(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    // 02:30 → 03:30 Paris on 2026-10-25, taking the first of the two 02:30.
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "Garde de nuit",
            "starts_at": "2026-10-25T00:30:00Z",
            "ends_at": "2026-10-25T01:30:00Z",
            "all_day": false,
            "rrule": "FREQ=MONTHLY",
        })),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);

    // A second, ordinary event in the same window: the 500 took the whole
    // list down, so its presence is what shows the blast radius.
    let plain = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "Courses",
            "starts_at": "2026-11-25T09:00:00Z",
            "ends_at": "2026-11-25T10:00:00Z",
            "all_day": false,
        })),
    )
    .await;
    assert_status(&plain, StatusCode::CREATED);

    // The window opens in October, so the series' own start is inside it.
    let from = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
    let to = Utc.with_ymd_and_hms(2026, 11, 30, 0, 0, 0).unwrap();
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
    let body = json_body(list).await;
    // Naming every occurrence rather than counting them: a count couples
    // this test to whatever else happens to be seeded in the window, and
    // says nothing about which occurrence went missing when it breaks.
    assert_eq!(
        occurrence_titles_and_starts(&body),
        vec![
            // Its own start, the first of the two 02:30 (CEST).
            ("Garde de nuit".into(), "2026-10-25T00:30:00Z".into()),
            // November has no repeated hour: 02:30 Paris is 01:30Z.
            ("Garde de nuit".into(), "2026-11-25T01:30:00Z".into()),
            ("Courses".into(), "2026-11-25T09:00:00Z".into()),
        ],
        "{body}"
    );
}

/// #116, round 3: the **second** pass of the repeated hour, end to end.
///
/// Unrolling walks the wall clock, and 02:30 maps back to the *first* pass
/// of 2026-10-25. A series anchored on the second pass therefore regenerated
/// its own start an hour early, `rrule` dropped it as earlier than
/// `DTSTART`, and `GET /events` answered **200 OK with the first occurrence
/// missing** — no error anywhere. The web form never reaches this instant
/// (its `paris_local_to_utc` resolves `02:30` with `earliest()`), so a check
/// through the form could not have caught it. A direct API call does reach
/// it, as below, and so can a calendar re-import on an imported event later
/// given a rule by `PATCH`: `google_calendar/imports.rs` rewrites
/// `starts_at` from the feed without touching `rrule`.
#[sqlx::test]
async fn a_recurring_event_on_the_second_pass_of_the_repeated_hour_renders_its_own_start(
    db: PgPool,
) {
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
            "title": "Relève",
            "starts_at": "2026-10-25T01:30:00Z",
            "ends_at": "2026-10-25T02:30:00Z",
            "all_day": false,
            "rrule": "FREQ=DAILY;COUNT=3",
        })),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);

    let from = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
    let to = Utc.with_ymd_and_hms(2026, 11, 30, 0, 0, 0).unwrap();
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
    let body = json_body(list).await;
    // Three days at 02:30 Paris, starting on its own start — the second
    // 02:30 of the 25th (CET), then two ordinary ones.
    assert_eq!(
        occurrence_titles_and_starts(&body),
        vec![
            ("Relève".into(), "2026-10-25T01:30:00Z".into()),
            ("Relève".into(), "2026-10-26T01:30:00Z".into()),
            ("Relève".into(), "2026-10-27T01:30:00Z".into()),
        ],
        "{body}"
    );
}

/// #116, round 4: an hourly series across the hour Paris **skips**, end to
/// end.
///
/// On 2026-03-29 Paris goes from 01:59 CET to 03:00 CEST. `rrule` reads the
/// missing 02:00 with the offset before the gap — 01:00Z, the very instant
/// 03:00 names — so unrolled in Paris the series produced 01:00Z twice, and
/// `COUNT` spent a unit on the copy. `list_events` does not deduplicate: the
/// same occurrence came back twice in `GET /events`, and the last one went
/// missing. The web form offers no hourly rule (`parse_rrule`); a direct API
/// call, as below, does.
#[sqlx::test]
async fn an_hourly_event_across_the_skipped_hour_is_listed_once_per_instant(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    // 00:00 → 00:15 Paris on 2026-03-29, every hour, four times.
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "Biberon",
            "starts_at": "2026-03-28T23:00:00Z",
            "ends_at": "2026-03-28T23:15:00Z",
            "all_day": false,
            "rrule": "FREQ=HOURLY;COUNT=4",
        })),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);

    let from = Utc.with_ymd_and_hms(2026, 3, 28, 0, 0, 0).unwrap();
    let to = Utc.with_ymd_and_hms(2026, 3, 30, 0, 0, 0).unwrap();
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
    let body = json_body(list).await;
    // 00:00 and 01:00 CET, then 03:00 and 04:00 CEST: four distinct instants.
    assert_eq!(
        occurrence_titles_and_starts(&body),
        vec![
            ("Biberon".into(), "2026-03-28T23:00:00Z".into()),
            ("Biberon".into(), "2026-03-29T00:00:00Z".into()),
            ("Biberon".into(), "2026-03-29T01:00:00Z".into()),
            ("Biberon".into(), "2026-03-29T02:00:00Z".into()),
        ],
        "{body}"
    );
}

/// Every occurrence of a `GET /events` body as `(title, occurrence start)`,
/// ordered by start then title — what the caller actually means when it
/// wants to say "these occurrences and no others".
fn occurrence_titles_and_starts(body: &serde_json::Value) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = body["occurrences"]
        .as_array()
        .expect("occurrences array")
        .iter()
        .map(|o| {
            (
                o["title"].as_str().unwrap_or_default().to_string(),
                o["occurrence_starts_at"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    rows.sort_by(|a, b| (&a.1, &a.0).cmp(&(&b.1, &b.0)));
    rows
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

/// #120: a non-empty title was an invariant of the web forms alone
/// (`validate_event_form`), so `POST` and `PATCH` took `"  "` and stored it.
/// The API holds it now, on both verbs, and a PATCH that carries no title at
/// all still leaves the stored one alone.
#[sqlx::test]
async fn an_event_title_that_is_blank_is_refused_on_write(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let events_path = format!("/groups/{group_id}/events");

    let blank = call(
        &router,
        Method::POST,
        &events_path,
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "  ",
            "starts_at": "2026-09-10T08:00:00Z",
            "ends_at": "2026-09-10T09:00:00Z",
        })),
    )
    .await;
    assert_status(&blank, StatusCode::BAD_REQUEST);
    assert_eq!(json_body(blank).await["error"], "title_required");

    let created = call(
        &router,
        Method::POST,
        &events_path,
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "Anniversaire",
            "starts_at": "2026-09-10T08:00:00Z",
            "ends_at": "2026-09-10T09:00:00Z",
        })),
    )
    .await;
    assert_status(&created, StatusCode::CREATED);
    let event_id = json_body(created).await["id"].as_str().unwrap().to_string();
    let event_path = format!("/groups/{group_id}/events/{event_id}");

    let blanked = call(
        &router,
        Method::PATCH,
        &event_path,
        Some(&owner_cookie),
        Some(serde_json::json!({"title": "\t"})),
    )
    .await;
    assert_status(&blanked, StatusCode::BAD_REQUEST);
    assert_eq!(json_body(blanked).await["error"], "title_required");

    // No `title` field at all: nothing to check, and the stored title stays.
    let untouched = call(
        &router,
        Method::PATCH,
        &event_path,
        Some(&owner_cookie),
        Some(serde_json::json!({"location": "Maison"})),
    )
    .await;
    assert_status(&untouched, StatusCode::OK);
    assert_eq!(json_body(untouched).await["title"], "Anniversaire");
}

/// #161: an all-day series is unrolled on a UTC-midnight stand-in for its
/// date, so its rule has to be validated there — not on the Paris midnight
/// the row stores, which sits two hours earlier. The reproduction from the
/// issue: an all-day event on 2026-09-05 « jusqu'au 2026-09-04 » was a 201,
/// then a 500 on the whole month.
#[sqlx::test]
async fn an_all_day_series_until_the_day_before_is_refused_on_write(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let events_path = format!("/groups/{group_id}/events");
    let post = |rrule: &'static str| {
        call(
            &router,
            Method::POST,
            &events_path,
            Some(&owner_cookie),
            Some(serde_json::json!({
                "title": "Anniversaire",
                "starts_at": "2026-09-04T22:00:00Z",
                "ends_at": "2026-09-04T23:00:00Z",
                "all_day": true,
                "rrule": rrule,
            })),
        )
    };

    let until_the_day_before = post("FREQ=DAILY;UNTIL=20260904T235959Z").await;
    assert_status(&until_the_day_before, StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(until_the_day_before).await["error"],
        "invalid_rrule"
    );

    let until_its_own_day = post("FREQ=DAILY;UNTIL=20260905T235959Z").await;
    assert_status(&until_its_own_day, StatusCode::CREATED);

    // A PATCH that turns an hour-bound series all-day is checked on the
    // all-day anchor as well: the same rule from the same instant is valid
    // hour-bound, and not all-day.
    let hour_bound = call(
        &router,
        Method::POST,
        &events_path,
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "Garde",
            "starts_at": "2026-09-04T22:00:00Z",
            "ends_at": "2026-09-04T23:00:00Z",
            "rrule": "FREQ=DAILY;UNTIL=20260904T235959Z",
        })),
    )
    .await;
    assert_status(&hour_bound, StatusCode::CREATED);
    let event_id = json_body(hour_bound).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let to_all_day = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"all_day": true})),
    )
    .await;
    assert_status(&to_all_day, StatusCode::BAD_REQUEST);
}

/// The mirror `PATCH`, in the other direction. An all-day rule is checked on
/// everything an hour-bound one is and more — it has to step by whole days
/// (#171), and its `UNTIL` is read on the day it names, never later than the
/// instant the rule carries (#162) — so a rule a row holds while all-day is a
/// rule it can keep once it is hour-bound, and `{"all_day": false}` never
/// costs it its recurrence. The claim was read off `validate` and never
/// replayed over HTTP until here.
#[sqlx::test]
async fn turning_an_all_day_series_hour_bound_keeps_its_rule(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    // « Jusqu'au 2026-09-05 » on an all-day event of that very day: accepted
    // all-day, and the `UNTIL` sits inside the Paris day the row stores
    // (2026-09-04T22:00Z), which is where an hour-bound reading differs.
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "Anniversaire",
            "starts_at": "2026-09-04T22:00:00Z",
            "ends_at": "2026-09-04T23:00:00Z",
            "all_day": true,
            "rrule": "FREQ=DAILY;UNTIL=20260905T235959Z",
        })),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let event_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let event_path = format!("/groups/{group_id}/events/{event_id}");
    let to_hour_bound = call(
        &router,
        Method::PATCH,
        &event_path,
        Some(&owner_cookie),
        Some(serde_json::json!({"all_day": false})),
    )
    .await;
    assert_status(&to_hour_bound, StatusCode::OK);
    let body = json_body(to_hour_bound).await;
    assert_eq!(body["all_day"], false);
    assert_eq!(body["rrule"], "FREQ=DAILY;UNTIL=20260905T235959Z");

    // And a `PATCH` that leaves it all-day without replacing the rule is
    // still a 200: `update_event` re-validates the merged pair, and the rule
    // was accepted all-day in the first place.
    let back_to_all_day = call(
        &router,
        Method::PATCH,
        &event_path,
        Some(&owner_cookie),
        Some(serde_json::json!({"all_day": true})),
    )
    .await;
    assert_status(&back_to_all_day, StatusCode::OK);
}

/// #165: an all-day series has two readers — `list_events`, and the reminders
/// (`refill_notifications`) — and at #165 they did not unroll it the same
/// way, so write had to refuse what either refused. Since #169 both go
/// through `recurrence::expand_series`, and since #162 through one
/// construction. The rules below stay refused: none of them is an RRULE
/// value, and a row holding one is still not unrolled as it is stored.
/// The reproduction from the issue:
/// `FREQ=WEEKLY\nEXDATE:…` on an all-day event was a 201, then
/// `POST /reminders` on it a 500. Every such rule is a 400 now, on create
/// and on update, all-day or not, so the reminders never meet one written
/// through the API.
#[sqlx::test]
async fn a_rule_the_reminders_cannot_unroll_is_refused_on_write(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let events_path = format!("/groups/{group_id}/events");

    // Tomorrow, so the reminders window (30 days from now) holds occurrences.
    let starts_at = Utc::now() + Duration::days(1);
    let refused = [
        "FREQ=WEEKLY\nEXDATE:20260927T000000Z",
        "FREQ=WEEKLY\nRDATE:20260927T000000Z",
        "FREQ=WEEKLY\nDTSTART:20260101T000000Z",
        "FREQ=WEEKLY\r\nEXDATE:20260927T000000Z",
        "FREQ=WEEKLY\rEXDATE:20260927T000000Z",
        "FREQ=WEEKLY\r",
        "FREQ=WEEKLY\nRRULE:FREQ=DAILY",
        // One line, read one way by `list_events` and refused by the
        // reminders until #169; unrolled by neither since #162.
        "FREQ=WEEKLY;BYDAY=X:MO",
        // A `:` on one line, read two ways at #165: all-day on a Saturday,
        // Mondays for `list_events` and Saturdays for the reminders;
        // hour-bound, stored as written and unrolled as `FREQ=DAILY`.
        "BYDAY=1:WKST=MO;FREQ=WEEKLY",
        "FREQ=WEEKLY;X:FREQ=DAILY",
        "RRULE:FREQ=DAILY",
    ];
    for rule in refused {
        for all_day in [true, false] {
            let create = call(
                &router,
                Method::POST,
                &events_path,
                Some(&owner_cookie),
                Some(serde_json::json!({
                    "title": "Sport",
                    "starts_at": starts_at,
                    "ends_at": starts_at + Duration::hours(1),
                    "all_day": all_day,
                    "rrule": rule,
                })),
            )
            .await;
            assert_eq!(
                create.status(),
                StatusCode::BAD_REQUEST,
                "{rule:?}, all_day = {all_day}"
            );
            assert_eq!(json_body(create).await["error"], "invalid_rrule");
        }
    }

    // A valid all-day series takes a reminder…
    let create = call(
        &router,
        Method::POST,
        &events_path,
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "Sport",
            "starts_at": starts_at,
            "ends_at": starts_at + Duration::hours(1),
            "all_day": true,
            "rrule": "FREQ=WEEKLY",
        })),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let event_id: Uuid = json_body(create).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let event_path = format!("{events_path}/{event_id}");
    let reminders_path = format!("{event_path}/reminders");
    let reminder = call(
        &router,
        Method::POST,
        &reminders_path,
        Some(&owner_cookie),
        Some(serde_json::json!({"offset_minutes": 60})),
    )
    .await;
    assert_status(&reminder, StatusCode::CREATED);

    // …cannot be given one of those rules by PATCH either…
    for rule in refused {
        let patch = call(
            &router,
            Method::PATCH,
            &event_path,
            Some(&owner_cookie),
            Some(serde_json::json!({"rrule": rule})),
        )
        .await;
        assert_eq!(patch.status(), StatusCode::BAD_REQUEST, "{rule:?}");
    }
    let stored: Option<String> = sqlx::query_scalar("SELECT rrule FROM events WHERE id = $1")
        .bind(event_id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(stored.as_deref(), Some("FREQ=WEEKLY"));

    // …and so a second reminder on it still schedules instead of failing.
    let second = call(
        &router,
        Method::POST,
        &reminders_path,
        Some(&owner_cookie),
        Some(serde_json::json!({"offset_minutes": 1440})),
    )
    .await;
    assert_status(&second, StatusCode::CREATED);
    let scheduled: i64 =
        sqlx::query_scalar("SELECT count(*) FROM scheduled_notifications WHERE event_id = $1")
            .bind(event_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(scheduled > 0);
}

/// #170: `FREQ=WEEKLY;BYDAY=éA` made `rrule` panic inside `validate`, all-day
/// or not, and the handler went down with it. A non-ASCII rule is a 400
/// `invalid_rrule` now, on create and on update, and the stored rule stays.
#[sqlx::test]
async fn a_non_ascii_rule_is_refused_on_write(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let events_path = format!("/groups/{group_id}/events");

    let starts_at = Utc::now() + Duration::days(1);
    let refused = ["FREQ=WEEKLY;BYDAY=éA", "FREQ=MONTHLY;BYDAY=1éA"];
    for rule in refused {
        for all_day in [true, false] {
            let create = call(
                &router,
                Method::POST,
                &events_path,
                Some(&owner_cookie),
                Some(serde_json::json!({
                    "title": "Sport",
                    "starts_at": starts_at,
                    "ends_at": starts_at + Duration::hours(1),
                    "all_day": all_day,
                    "rrule": rule,
                })),
            )
            .await;
            assert_eq!(
                create.status(),
                StatusCode::BAD_REQUEST,
                "{rule:?}, all_day = {all_day}"
            );
            assert_eq!(json_body(create).await["error"], "invalid_rrule");
        }
    }

    for all_day in [true, false] {
        let create = call(
            &router,
            Method::POST,
            &events_path,
            Some(&owner_cookie),
            Some(serde_json::json!({
                "title": "Sport",
                "starts_at": starts_at,
                "ends_at": starts_at + Duration::hours(1),
                "all_day": all_day,
                "rrule": "FREQ=WEEKLY",
            })),
        )
        .await;
        assert_status(&create, StatusCode::CREATED);
        let event_id: Uuid = json_body(create).await["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        for rule in refused {
            let patch = call(
                &router,
                Method::PATCH,
                &format!("{events_path}/{event_id}"),
                Some(&owner_cookie),
                Some(serde_json::json!({"rrule": rule})),
            )
            .await;
            assert_eq!(
                patch.status(),
                StatusCode::BAD_REQUEST,
                "{rule:?}, all_day = {all_day}"
            );
            assert_eq!(json_body(patch).await["error"], "invalid_rrule");
        }
        let stored: Option<String> = sqlx::query_scalar("SELECT rrule FROM events WHERE id = $1")
            .bind(event_id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(stored.as_deref(), Some("FREQ=WEEKLY"));
    }
}

/// #171: an all-day series is unrolled on civil dates, so a rule stepping by
/// less than a day or naming a time of day unrolled into copies of one day —
/// `FREQ=HOURLY;COUNT=5` listed five times and reminded once. It is a 400
/// `invalid_rrule` on an all-day row now, on create and on update, whether
/// the update brings the rule or the flag; an hour-bound row keeps it. A row
/// stored before the check still reads.
#[sqlx::test]
async fn an_all_day_rule_stepping_by_less_than_a_day_is_refused_on_write(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let events_path = format!("/groups/{group_id}/events");
    let create = |all_day: bool, rule: &str| {
        call(
            &router,
            Method::POST,
            &events_path,
            Some(&owner_cookie),
            Some(serde_json::json!({
                "title": "Anniversaire",
                "starts_at": "2026-09-04T22:00:00Z",
                "ends_at": "2026-09-05T22:00:00Z",
                "all_day": all_day,
                "rrule": rule,
            })),
        )
    };
    let patch = |event_id: Uuid, body: serde_json::Value| {
        let path = format!("{events_path}/{event_id}");
        let (router, owner_cookie) = (&router, &owner_cookie);
        async move { call(router, Method::PATCH, &path, Some(owner_cookie), Some(body)).await }
    };

    let refused = [
        "FREQ=HOURLY;COUNT=5",
        "FREQ=MINUTELY",
        "FREQ=SECONDLY;COUNT=3",
        "FREQ=DAILY;BYHOUR=12;COUNT=3",
        "FREQ=WEEKLY;BYMINUTE=30",
        "FREQ=MONTHLY;BYSECOND=0",
    ];
    for rule in refused {
        let all_day = create(true, rule).await;
        assert_eq!(all_day.status(), StatusCode::BAD_REQUEST, "{rule:?}");
        assert_eq!(json_body(all_day).await["error"], "invalid_rrule");

        let hour_bound = create(false, rule).await;
        assert_eq!(hour_bound.status(), StatusCode::CREATED, "{rule:?}");
    }

    // Update bringing the rule onto an all-day row.
    let daily = create(true, "FREQ=DAILY").await;
    assert_status(&daily, StatusCode::CREATED);
    let daily_id: Uuid = json_body(daily).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    for rule in refused {
        let response = patch(daily_id, serde_json::json!({"rrule": rule})).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{rule:?}");
        assert_eq!(json_body(response).await["error"], "invalid_rrule");
    }
    let stored: Option<String> = sqlx::query_scalar("SELECT rrule FROM events WHERE id = $1")
        .bind(daily_id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(stored.as_deref(), Some("FREQ=DAILY"));

    // Update bringing the flag onto an hour-bound row holding the rule.
    let hourly = create(false, "FREQ=HOURLY;COUNT=5").await;
    assert_status(&hourly, StatusCode::CREATED);
    let hourly_id: Uuid = json_body(hourly).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let response = patch(hourly_id, serde_json::json!({"all_day": true})).await;
    assert_status(&response, StatusCode::BAD_REQUEST);
    assert_eq!(json_body(response).await["error"], "invalid_rrule");
    let still_hour_bound: bool = sqlx::query_scalar("SELECT all_day FROM events WHERE id = $1")
        .bind(hourly_id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(!still_hour_bound);

    // A row stored before the check: no migration, and the window still
    // answers with it, on its own day.
    let mut tx = with_family_scope(&db, &group_id).await;
    let updated = sqlx::query("UPDATE events SET rrule = $1 WHERE id = $2")
        .bind("FREQ=HOURLY;COUNT=5")
        .bind(daily_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(updated.rows_affected(), 1);
    tx.commit().await.unwrap();
    let list = call(
        &router,
        Method::GET,
        &format!(
            "/groups/{group_id}/events?from={}&to={}",
            urlenc("2026-08-31T22:00:00+00:00"),
            urlenc("2026-09-30T21:59:59+00:00")
        ),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&list, StatusCode::OK);
    let body = json_body(list).await;
    let stored_days: Vec<DateTime<Utc>> = body["occurrences"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["id"].as_str() == Some(&daily_id.to_string()))
        .map(|o| instant(o, "occurrence_starts_at"))
        .collect();
    assert!(!stored_days.is_empty(), "{body}");
    assert!(
        stored_days
            .iter()
            .all(|at| *at == "2026-09-04T22:00:00Z".parse::<DateTime<Utc>>().unwrap()),
        "{body}"
    );
}

/// #161: a series stored but impossible to unroll no longer takes the whole
/// window down. Write refuses the all-day case now, but a row can still get
/// there — written before the check, or moved by a calendar re-import (see
/// `google_calendar_flow.rs` for that path) — so the row is put there by
/// hand. The window answers 200, the other events are all there, and the
/// broken series is rendered as its own row.
#[sqlx::test]
async fn a_stored_series_that_does_not_unroll_does_not_take_the_window_down(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let events_path = format!("/groups/{group_id}/events");
    let create = |body: serde_json::Value| {
        call(
            &router,
            Method::POST,
            &events_path,
            Some(&owner_cookie),
            Some(body),
        )
    };
    let one_off = create(serde_json::json!({
        "title": "Dentiste",
        "starts_at": "2026-09-10T08:00:00Z",
        "ends_at": "2026-09-10T09:00:00Z",
    }))
    .await;
    assert_status(&one_off, StatusCode::CREATED);
    let weekly = create(serde_json::json!({
        "title": "Piscine",
        "starts_at": "2026-09-02T16:00:00Z",
        "ends_at": "2026-09-02T17:00:00Z",
        "rrule": "FREQ=WEEKLY;COUNT=2",
    }))
    .await;
    assert_status(&weekly, StatusCode::CREATED);
    let all_day = create(serde_json::json!({
        "title": "Anniversaire",
        "starts_at": "2026-09-04T22:00:00Z",
        "ends_at": "2026-09-04T23:00:00Z",
        "all_day": true,
        "rrule": "FREQ=DAILY;UNTIL=20260905T235959Z",
    }))
    .await;
    assert_status(&all_day, StatusCode::CREATED);
    let all_day_id: Uuid = json_body(all_day).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // The rule the issue reproduced, written past the write-time check.
    let mut tx = with_family_scope(&db, &group_id).await;
    let updated = sqlx::query("UPDATE events SET rrule = $1 WHERE id = $2")
        .bind("FREQ=DAILY;UNTIL=20260904T235959Z")
        .bind(all_day_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(updated.rows_affected(), 1);
    tx.commit().await.unwrap();

    // September as `/agenda` asks for it: Paris midnight to Paris midnight.
    let list = call(
        &router,
        Method::GET,
        &format!(
            "/groups/{group_id}/events?from={}&to={}",
            urlenc("2026-08-31T22:00:00+00:00"),
            urlenc("2026-09-30T21:59:59+00:00")
        ),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&list, StatusCode::OK);
    let body = json_body(list).await;
    let mut seen: Vec<(String, DateTime<Utc>)> = body["occurrences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            (
                o["title"].as_str().unwrap().to_string(),
                instant(o, "occurrence_starts_at"),
            )
        })
        .collect();
    seen.sort_by_key(|(_, at)| *at);
    let at = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
    assert_eq!(
        seen,
        vec![
            ("Piscine".to_string(), at("2026-09-02T16:00:00Z")),
            ("Anniversaire".to_string(), at("2026-09-04T22:00:00Z")),
            ("Piscine".to_string(), at("2026-09-09T16:00:00Z")),
            ("Dentiste".to_string(), at("2026-09-10T08:00:00Z")),
        ],
        "{body}"
    );
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

/// #169: an all-day series is reminded on the occurrences the agenda lists.
/// `UNTIL` at 23:59:59Z on its last day — what the web form writes for
/// « Jusqu'au … » — sits after Paris midnight on the following day (22:00Z
/// or 23:00Z), and the reminders, unrolling on instants, used to schedule
/// that extra day. The window of the reminders opens at `now`, so the series
/// is placed a few days ahead and this test meets whichever offset Paris is
/// on when it runs; both are pinned in `agenda::reminders`' unit tests.
#[sqlx::test]
async fn an_all_day_series_is_reminded_on_exactly_the_days_the_agenda_lists(db: PgPool) {
    use manage_our_home_shared::validation::agenda::{paris_date, paris_start_of_day};

    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let events_path = format!("/groups/{group_id}/events");

    let first = paris_date(Utc::now()) + Duration::days(2);
    let last = first + Duration::days(9);
    let starts_at = paris_start_of_day(first);
    let rrule = format!("FREQ=DAILY;UNTIL={}T235959Z", last.format("%Y%m%d"));
    let create = call(
        &router,
        Method::POST,
        &events_path,
        Some(&owner_cookie),
        Some(serde_json::json!({
            "title": "Stage",
            "starts_at": starts_at,
            "ends_at": starts_at + Duration::hours(1),
            "all_day": true,
            "rrule": rrule,
        })),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let event_id: Uuid = json_body(create).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let reminder = call(
        &router,
        Method::POST,
        &format!("{events_path}/{event_id}/reminders"),
        Some(&owner_cookie),
        Some(serde_json::json!({"offset_minutes": 0})),
    )
    .await;
    assert_status(&reminder, StatusCode::CREATED);

    let (from, to) = (
        starts_at - Duration::days(3),
        starts_at + Duration::days(25),
    );
    let list = call(
        &router,
        Method::GET,
        &format!(
            "{events_path}?from={}&to={}",
            urlenc(&from.to_rfc3339()),
            urlenc(&to.to_rfc3339())
        ),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&list, StatusCode::OK);
    let body = json_body(list).await;
    let listed: Vec<DateTime<Utc>> = body["occurrences"]
        .as_array()
        .expect("occurrences array")
        .iter()
        .map(|o| instant(o, "occurrence_starts_at"))
        .collect();

    let reminded: Vec<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT occurrence_at FROM scheduled_notifications WHERE event_id = $1 ORDER BY occurrence_at",
    )
    .bind(event_id)
    .fetch_all(&db)
    .await
    .unwrap();

    let days: Vec<_> = first.iter_days().take_while(|d| *d <= last).collect();
    assert_eq!(
        listed.iter().map(|s| paris_date(*s)).collect::<Vec<_>>(),
        days,
        "{rrule}: {body}"
    );
    assert_eq!(reminded, listed, "{rrule}");
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

// ---------------------------------------------------------------------------
// Upload body limit (#190)
// ---------------------------------------------------------------------------

/// A payload of `size` bytes that `infer` sniffs as a PNG: the magic header
/// followed by padding. Only the first bytes are ever looked at, so the
/// padding is what makes the size, not the image.
fn png_of_size(size: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; size];
    bytes[..PNG_BYTES.len()].copy_from_slice(PNG_BYTES);
    bytes
}

/// `MAX_ATTACHMENT_SIZE_BYTES` announces 20 MiB, but axum's `DefaultBodyLimit`
/// caps request bodies at 2 MiB unless the route says otherwise — so anything
/// past 2 MiB died while the handler read the multipart fields, and the
/// constant was never reached. Not with a 413 either: `upload_attachment`
/// maps every `MultipartError` to `invalid_multipart` (400), so the answer
/// named neither the size nor the limit. Past the cap the refusal must come
/// from the handler's own check, with `file_too_large`, which is only
/// possible if the whole body got through.
///
/// No storage needed: the size check precedes the MIME sniff and the upload.
#[sqlx::test]
async fn an_upload_over_the_cap_is_refused_by_the_handler_not_the_body_limit(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "upload-cap@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let event_id = create_event(&router, &owner_cookie, &group_id).await;

    let upload = call_upload(
        &router,
        &format!("/groups/{group_id}/events/{event_id}/attachments"),
        &owner_cookie,
        "trop-gros.png",
        &png_of_size(MAX_ATTACHMENT_SIZE_BYTES + 1),
    )
    .await;

    assert_status(&upload, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json_body(upload).await["error"], "file_too_large");
}

/// The body limit is raised, not removed. Paired with the test above, this
/// pins the limit somewhere between `MAX_ATTACHMENT_SIZE_BYTES + 1` — which
/// must reach the handler and get `file_too_large` — and a megabyte past the
/// cap, which must not.
///
/// The refusal surfaces as `invalid_multipart` (400): the limit is enforced
/// while the field is read, and the handler maps every `MultipartError` to
/// that one code — a mapping left unchanged here. It is the backstop nobody
/// reaches: an honest client is stopped by the handler's own 422 first, and
/// only framing far beyond the margin lands here.
#[sqlx::test]
async fn a_body_past_the_framing_margin_never_reaches_the_handler(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "upload-limit@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let event_id = create_event(&router, &owner_cookie, &group_id).await;

    let upload = call_upload(
        &router,
        &format!("/groups/{group_id}/events/{event_id}/attachments"),
        &owner_cookie,
        "bien-trop-gros.png",
        &png_of_size(MAX_ATTACHMENT_SIZE_BYTES + 1024 * 1024),
    )
    .await;

    assert_status(&upload, StatusCode::BAD_REQUEST);
    assert_eq!(json_body(upload).await["error"], "invalid_multipart");
}

/// The other half: an attachment of exactly the announced 20 MiB is stored
/// and committed. The test above only shows where the refusal comes from —
/// it would still pass if every upload past 2 MiB were refused a little
/// later.
///
/// Needs a real MinIO; skipped otherwise (see `real_minio_from_env`).
#[sqlx::test]
async fn an_attachment_of_exactly_the_cap_is_stored(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping an_attachment_of_exactly_the_cap_is_stored: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let router = test_router_with_storage(
        db.clone(),
        manage_our_home::storage::Storage::new(s3.clone(), bucket.clone()),
    );
    let owner_cookie =
        register_verify_login(&router, &db, "upload-20mo@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let event_id = create_event(&router, &owner_cookie, &group_id).await;

    let upload = call_upload(
        &router,
        &format!("/groups/{group_id}/events/{event_id}/attachments"),
        &owner_cookie,
        "scan.png",
        &png_of_size(MAX_ATTACHMENT_SIZE_BYTES),
    )
    .await;

    assert_status(&upload, StatusCode::CREATED);
    assert_eq!(
        json_body(upload).await["size_bytes"],
        MAX_ATTACHMENT_SIZE_BYTES as i64
    );

    let storage_key = attachment_storage_key(&db, &group_id, &event_id).await;
    assert!(
        s3.head_object()
            .bucket(&bucket)
            .key(&storage_key)
            .send()
            .await
            .is_ok(),
        "a 20 MiB attachment must reach the bucket like any other"
    );
}

// ---------------------------------------------------------------------------
// Pool connections during a slow upload (#216)
// ---------------------------------------------------------------------------

/// An upload whose body arrives in two parts: the multipart preamble at
/// once, then nothing until the test says so — a client on a bad link.
/// `waiting_for_body` resolves once the handler has consumed the preamble
/// and is waiting for the rest. Sending on `release` delivers the file and
/// closes the body; dropping it cuts the body short.
struct SlowUpload {
    waiting_for_body: tokio::sync::oneshot::Receiver<()>,
    release: tokio::sync::oneshot::Sender<()>,
    response: tokio::task::JoinHandle<axum::http::Response<axum::body::Body>>,
}

fn start_slow_upload(router: &axum::Router, uri: &str, cookie: &str) -> SlowUpload {
    use axum::body::{Body, Bytes};
    use axum::http::{header, Request};
    use tower::ServiceExt;

    const BOUNDARY: &str = "----manageourhomeslowboundary";
    let preamble = Bytes::from(format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"lent.png\"\r\n\r\n"
    ));
    let mut rest = PNG_BYTES.to_vec();
    rest.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());

    let (waiting_tx, waiting_for_body) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel::<()>();

    type Step = (
        u8,
        Option<tokio::sync::oneshot::Sender<()>>,
        Option<tokio::sync::oneshot::Receiver<()>>,
    );
    let chunks = futures::stream::unfold(
        (0u8, Some(waiting_tx), Some(release_rx)) as Step,
        move |(step, waiting_tx, release_rx)| {
            let preamble = preamble.clone();
            let rest = Bytes::from(rest.clone());
            async move {
                match step {
                    0 => Some((
                        Ok::<_, std::io::Error>(preamble),
                        (1, waiting_tx, release_rx),
                    )),
                    1 => {
                        let _ = waiting_tx.unwrap().send(());
                        match release_rx.unwrap().await {
                            Ok(()) => Some((Ok(rest), (2, None, None))),
                            Err(_) => Some((
                                Err(std::io::Error::other("client went away")),
                                (2, None, None),
                            )),
                        }
                    }
                    _ => None,
                }
            }
        },
    );

    let request = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::COOKIE, cookie)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from_stream(chunks))
        .unwrap();
    let router = router.clone();
    let response = tokio::spawn(async move { router.oneshot(request).await.unwrap() });

    SlowUpload {
        waiting_for_body,
        release,
        response,
    }
}

/// A runtime pool of `size` connections on the test database. An acquire
/// gives up after two seconds rather than the default thirty, so that an
/// exhausted pool shows up as a prompt 500 instead of a slow one.
async fn small_pool(db: &PgPool, size: u32) -> PgPool {
    manage_our_home::db::pool_options()
        .max_connections(size)
        .acquire_timeout(std::time::Duration::from_secs(2))
        .connect_with((*db.connect_options()).clone())
        .await
        .unwrap()
}

/// AC (#216): an upload must not hold a pool connection while its client is
/// still sending the body. As many stalled uploads as the pool has
/// connections used to take every one of them, `idle in transaction`, and
/// any other request — here a plain `GET /groups` from another user —
/// failed on `PoolTimedOut`.
#[sqlx::test]
async fn slow_uploads_filling_the_pool_do_not_starve_other_requests(db: PgPool) {
    const POOL_SIZE: u32 = 3;
    let router = test_router(small_pool(&db, POOL_SIZE).await);

    let owner_cookie =
        register_verify_login(&router, &db, "slow-owner@example.test", "owner-password1").await;
    let reader_cookie =
        register_verify_login(&router, &db, "slow-reader@example.test", "reader-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let event_id = create_event(&router, &owner_cookie, &group_id).await;

    let uploads: Vec<SlowUpload> = (0..POOL_SIZE)
        .map(|_| {
            start_slow_upload(
                &router,
                &format!("/groups/{group_id}/events/{event_id}/attachments"),
                &owner_cookie,
            )
        })
        .collect();
    let mut releases = Vec::new();
    let mut responses = Vec::new();
    for upload in uploads {
        upload
            .waiting_for_body
            .await
            .expect("every upload reaches its body");
        releases.push(upload.release);
        responses.push(upload.response);
    }

    let groups = call(&router, Method::GET, "/groups", Some(&reader_cookie), None).await;
    assert_status(&groups, StatusCode::OK);

    // Cut the stalled bodies short and let the handlers finish.
    drop(releases);
    for response in responses {
        response.await.unwrap();
    }
}

/// AC (#216): the event is checked before the body is read, and the
/// attachment row is written in a second transaction once the body has
/// arrived. An event deleted in between must answer 404, not the 500 of a
/// failed foreign key.
///
/// Driven through the prescribed `NOBYPASSRLS` role: under it the INSERT is
/// refused by the `event_attachments` policy before the foreign key is even
/// checked, and the re-check's `FOR KEY SHARE` needs the role's grants.
#[sqlx::test]
async fn an_event_deleted_while_its_upload_is_in_flight_answers_not_found(db: PgPool) {
    let (role, pool) = prescribed_role_pool(&db).await;
    let router = test_router(pool.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "race-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let event_id = create_event(&router, &owner_cookie, &group_id).await;

    let upload = start_slow_upload(
        &router,
        &format!("/groups/{group_id}/events/{event_id}/attachments"),
        &owner_cookie,
    );
    upload
        .waiting_for_body
        .await
        .expect("the upload reaches its body");

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::NO_CONTENT);

    upload.release.send(()).unwrap();
    let response = upload.response.await.unwrap();
    assert_status(&response, StatusCode::NOT_FOUND);

    drop(router);
    drop_prescribed_role(&db, pool, &role).await;
}
