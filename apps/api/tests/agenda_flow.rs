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
/// cliff (22:00Z in summer, 23:00Z in winter). Unrolled in UTC, every later
/// occurrence would keep September's offset and land at 22:00Z, i.e. 23:00
/// on the *previous* day once the clocks go back. The event then vanishes
/// from a dashboard window that starts at Paris midnight, which is #101's
/// own symptom one level up.
///
/// #101 bought that with a UTC-midnight stand-in for each civil date; #162
/// unrolls the row in Paris from the midnight it stores instead. This test
/// is the invariant, not the path: it is kept verbatim across that move.
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

/// #162: the dashboard's window, end to end. `apps/web`'s `home.rs` asks for
/// `<today>T00:00` to `<today + 2>T23:59:59` in Paris — a three-day window
/// whose far end is one second before the Paris midnight that opens the
/// fourth day. A daily all-day series must be listed three times over it, not
/// four: neither `list_events` nor the dashboard's own `soonest_occurrences`
/// filters again, so a second of slack in the unroll is a whole extra day on
/// the page. Replayed here through `GET /events`, where the unroll actually
/// runs.
#[sqlx::test]
async fn an_all_day_series_fills_the_dashboard_window_and_not_the_day_after(db: PgPool) {
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
            "title": "Sport",
            // Paris is UTC+2 in June: the civil day of the 1st.
            "starts_at": "2026-05-31T22:00:00Z",
            "ends_at": "2026-05-31T23:00:00Z",
            "all_day": true,
            "rrule": "FREQ=DAILY",
        })),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);

    let from = Utc.with_ymd_and_hms(2026, 5, 31, 22, 0, 0).unwrap();
    let to = Utc.with_ymd_and_hms(2026, 6, 3, 21, 59, 59).unwrap();
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
    let starts: Vec<DateTime<Utc>> = occurrences
        .iter()
        .map(|occ| instant(occ, "occurrence_starts_at"))
        .collect();
    assert_eq!(
        starts,
        vec![
            from,
            Utc.with_ymd_and_hms(2026, 6, 1, 22, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 6, 2, 22, 0, 0).unwrap(),
        ],
        "the window of three civil days does not hold exactly its three days: {body}"
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

/// #161: an all-day series' rule has to be validated on the construction
/// that unrolls it. At #161 it was not — the row was unrolled on a
/// UTC-midnight stand-in for its date while the rule was checked on the
/// Paris midnight the row stores, two hours earlier — and the reproduction
/// from the issue was an all-day event on 2026-09-05 « jusqu'au 2026-09-04 »:
/// a 201, then a 500 on the whole month.
///
/// Since #162 that construction is the Paris midnight itself, with `UNTIL`
/// read on the day it names; the answers below are unchanged.
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

/// #707: an occurrence a series excludes (`events.exdates`) is neither
/// listed by `GET …/events` nor reminded, on an hour-bound series (the
/// exact instant) and on an all-day one (the Paris day). No route writes
/// `exdates` yet, so the test sets it in the base.
#[sqlx::test]
async fn an_excluded_occurrence_is_neither_listed_nor_reminded(db: PgPool) {
    use manage_our_home_shared::validation::agenda::{paris_date, paris_start_of_day};

    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let events_path = format!("/groups/{group_id}/events");

    let first = paris_date(Utc::now()) + Duration::days(2);
    let midnight = paris_start_of_day(first);
    // 09:00 on the Paris wall clock `n` days after `first`: what a daily
    // hour-bound series keeps across a change of hour.
    let nine_on = |n: i64| {
        rrule::Tz::Europe__Paris
            .from_local_datetime(&(first + Duration::days(n)).and_hms_opt(9, 0, 0).unwrap())
            .single()
            .unwrap()
            .with_timezone(&Utc)
    };
    let nine = nine_on(0);
    // (all_day, starts_at, the occurrence excluded, what `exdates` stores)
    let cases = [
        (false, nine, nine_on(2), nine_on(2)),
        // Any instant of the Paris day excludes an all-day occurrence.
        (
            true,
            midnight,
            paris_start_of_day(first + Duration::days(2)),
            paris_start_of_day(first + Duration::days(2)) + Duration::hours(15),
        ),
    ];
    for (all_day, starts_at, excluded, stored) in cases {
        let create = call(
            &router,
            Method::POST,
            &events_path,
            Some(&owner_cookie),
            Some(serde_json::json!({
                "title": format!("Série all_day={all_day}"),
                "starts_at": starts_at,
                "ends_at": starts_at + Duration::hours(1),
                "all_day": all_day,
                "rrule": "FREQ=DAILY;COUNT=5",
            })),
        )
        .await;
        assert_status(&create, StatusCode::CREATED);
        let event_id: Uuid = json_body(create).await["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();

        let mut tx = with_family_scope(&db, &group_id).await;
        sqlx::query("UPDATE events SET exdates = ARRAY[$1::timestamptz] WHERE id = $2")
            .bind(stored)
            .bind(event_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();

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
            starts_at - Duration::days(1),
            starts_at + Duration::days(10),
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
            .filter(|o| o["id"] == event_id.to_string())
            .map(|o| instant(o, "occurrence_starts_at"))
            .collect();

        let reminded: Vec<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT occurrence_at FROM scheduled_notifications WHERE event_id = $1 ORDER BY occurrence_at",
        )
        .bind(event_id)
        .fetch_all(&db)
        .await
        .unwrap();

        // COUNT=5 less the excluded one: the exclusion does not give the
        // series a sixth occurrence.
        let expected: Vec<DateTime<Utc>> = (0..5)
            .map(|n| {
                if all_day {
                    paris_start_of_day(first + Duration::days(n))
                } else {
                    nine_on(n)
                }
            })
            .filter(|s| *s != excluded)
            .collect();
        assert_eq!(listed, expected, "all_day = {all_day}: {body}");
        assert_eq!(reminded, expected, "all_day = {all_day}");
    }
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
/// test-side reads and writes have to set `app.group_id` the way
/// `scoped_tx` does. Runtime `sqlx::query` on purpose: test-only SQL that
/// would otherwise need a `.sqlx` offline cache entry.
async fn with_family_scope<'a>(
    db: &PgPool,
    group_id: &str,
) -> sqlx::Transaction<'a, sqlx::Postgres> {
    #[allow(clippy::disallowed_methods)]
    let mut tx = db.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.group_id', $1, true)")
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

/// #773: an attachment's thumbnail has its own object key, `thumbnail_key`.
/// Nullable and without a default: NULL is "no thumbnail" (a PDF, a video,
/// an attachment stored before thumbnails existed), never a made-up key.
#[sqlx::test]
async fn attachment_thumbnail_key_is_a_nullable_text_column_without_default(db: PgPool) {
    let column: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT data_type::text, is_nullable::text, column_default::text
         FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'event_attachments'
           AND column_name = 'thumbnail_key'",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(column, [("text".into(), "YES".into(), None)]);
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

/// A real PNG of `width` × `height`, which the thumbnail decodes (#774);
/// `PNG_BYTES` is a signature only, sniffed as a PNG but unreadable.
fn real_png(width: u32, height: u32) -> Vec<u8> {
    use image::ImageEncoder;
    let picture =
        image::RgbImage::from_fn(width, height, |x, y| image::Rgb([x as u8, y as u8, 120]));
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(
            picture.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    out
}

/// The start of an MP4: an `ftyp` box, major brand `isom`.
const MP4_BYTES: &[u8] = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isomiso2avc1mp41";

/// The storage key and thumbnail key of the attachment `attachment_id`.
async fn attachment_keys(
    db: &PgPool,
    group_id: &str,
    attachment_id: &str,
) -> (String, Option<String>) {
    let mut tx = with_family_scope(db, group_id).await;
    let keys =
        sqlx::query_as("SELECT storage_key, thumbnail_key FROM event_attachments WHERE id = $1")
            .bind(Uuid::parse_str(attachment_id).unwrap())
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    keys
}

async fn object_exists(s3: &aws_sdk_s3::Client, bucket: &str, key: &str) -> bool {
    s3.head_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .is_ok()
}

/// Uploads `bytes` as `filename` and answers the attachment's JSON.
async fn upload(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    event_id: &str,
    filename: &str,
    bytes: &[u8],
) -> serde_json::Value {
    let res = call_upload(
        router,
        &format!("/groups/{group_id}/events/{event_id}/attachments"),
        cookie,
        filename,
        bytes,
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await
}

/// #774: a picture gets a thumbnail, stored as a JPEG of its own, its long
/// side at 400 px; a PDF, a video and an unreadable picture get none, and
/// are stored all the same. The listing says which have one.
///
/// Needs a real MinIO; skipped otherwise (see `real_minio_from_env`).
#[sqlx::test]
async fn a_picture_is_stored_with_a_thumbnail_a_pdf_or_a_video_without(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping a_picture_is_stored_with_a_thumbnail_a_pdf_or_a_video_without: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let storage = manage_our_home::storage::Storage::new(s3.clone(), bucket.clone());
    let router = test_router_with_storage(db.clone(), storage.clone());
    let cookie =
        register_verify_login(&router, &db, "thumb-kinds@example.test", "owner-password1").await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let event_id = create_event(&router, &cookie, &group_id).await;

    let png = upload(
        &router,
        &cookie,
        &group_id,
        &event_id,
        "photo.png",
        &real_png(800, 600),
    )
    .await;
    assert_eq!(png["has_thumbnail"], true, "{png}");
    let (_, thumbnail_key) = attachment_keys(&db, &group_id, png["id"].as_str().unwrap()).await;
    let thumbnail = storage.get_object(&thumbnail_key.unwrap()).await.unwrap();
    let decoded = image::load_from_memory_with_format(&thumbnail, image::ImageFormat::Jpeg)
        .expect("the thumbnail is a JPEG");
    assert_eq!((decoded.width(), decoded.height()), (400, 300));

    for (filename, bytes) in [
        ("ordonnance.pdf", &b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n"[..]),
        ("film.mp4", MP4_BYTES),
        ("illisible.png", PNG_BYTES),
    ] {
        let stored = upload(&router, &cookie, &group_id, &event_id, filename, bytes).await;
        assert_eq!(stored["has_thumbnail"], false, "{filename}: {stored}");
        let (storage_key, thumbnail_key) =
            attachment_keys(&db, &group_id, stored["id"].as_str().unwrap()).await;
        assert_eq!(thumbnail_key, None, "{filename}");
        assert!(
            object_exists(&s3, &bucket, &storage_key).await,
            "{filename}"
        );
    }
    let mp4 = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/events/{event_id}/attachments"),
        Some(&cookie),
        None,
    )
    .await;
    let listed = json_body(mp4).await;
    let mut flags: Vec<(String, bool)> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["filename"].as_str().unwrap().to_string(),
                a["has_thumbnail"].as_bool().unwrap(),
            )
        })
        .collect();
    flags.sort();
    assert_eq!(
        flags,
        [
            ("film.mp4".to_string(), false),
            ("illisible.png".to_string(), false),
            ("ordonnance.pdf".to_string(), false),
            ("photo.png".to_string(), true),
        ]
    );
}

/// #774: the thumbnail follows its file out of the bucket, whichever
/// delete takes it — the attachment's, the event's, the group's.
///
/// Needs a real MinIO; skipped otherwise (see `real_minio_from_env`).
#[sqlx::test]
async fn every_delete_removes_the_thumbnail_with_its_file(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping every_delete_removes_the_thumbnail_with_its_file: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let router = test_router_with_storage(
        db.clone(),
        manage_our_home::storage::Storage::new(s3.clone(), bucket.clone()),
    );
    let cookie =
        register_verify_login(&router, &db, "thumb-delete@example.test", "owner-password1").await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let event_id = create_event(&router, &cookie, &group_id).await;
    let other_event_id = create_event(&router, &cookie, &group_id).await;

    let mut stored = Vec::new();
    for event in [&event_id, &event_id, &other_event_id] {
        let attachment = upload(
            &router,
            &cookie,
            &group_id,
            event,
            "photo.png",
            &real_png(64, 48),
        )
        .await;
        let id = attachment["id"].as_str().unwrap().to_string();
        let (storage_key, thumbnail_key) = attachment_keys(&db, &group_id, &id).await;
        let thumbnail_key = thumbnail_key.expect("a readable PNG gets a thumbnail");
        assert!(object_exists(&s3, &bucket, &storage_key).await);
        assert!(object_exists(&s3, &bucket, &thumbnail_key).await);
        stored.push((id, storage_key, thumbnail_key));
    }
    let gone = |keys: [&String; 2]| {
        let (s3, bucket) = (s3.clone(), bucket.clone());
        let keys = keys.map(String::clone);
        async move {
            for key in keys {
                assert!(
                    !object_exists(&s3, &bucket, &key).await,
                    "{key} still stored"
                );
            }
        }
    };

    // The attachment's own delete.
    let (id, file, thumbnail) = &stored[0];
    let res = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/events/{event_id}/attachments/{id}"),
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    gone([file, thumbnail]).await;
    assert!(object_exists(&s3, &bucket, &stored[1].2).await);

    // The event's.
    let res = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    gone([&stored[1].1, &stored[1].2]).await;
    assert!(object_exists(&s3, &bucket, &stored[2].2).await);

    // The group's, through `group_attachment_storage_keys()` (0054).
    let res = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}"),
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    gone([&stored[2].1, &stored[2].2]).await;
}

/// One file per upload (#326). Each `file` field was read into its own
/// buffer of the declared body, and the previous one stayed alive until
/// the next was read whole: two fields held two bodies, so what an upload
/// cost was no longer bounded by its size. A second `file` field is refused
/// at its headers, before a byte of it is read — here it never ends, so an
/// answer at all shows it was not read — and nothing is stored.
#[sqlx::test]
async fn a_second_file_field_is_refused_before_it_is_read(db: PgPool) {
    use axum::body::{Body, Bytes};
    use axum::http::{header, Request};
    use futures::StreamExt;
    use tower::ServiceExt;

    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "upload-two@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let event_id = create_event(&router, &owner_cookie, &group_id).await;

    const BOUNDARY: &str = "----manageourhometwofilesboundary";
    let field = |name: &str| {
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\r\n"
        )
    };
    let mut first = field("un.png").into_bytes();
    first.extend_from_slice(PNG_BYTES);
    first.extend_from_slice(b"\r\n");
    first.extend_from_slice(field("deux.png").as_bytes());
    let chunks = futures::stream::iter([Ok::<_, std::io::Error>(Bytes::from(first))])
        .chain(futures::stream::pending());

    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("/groups/{group_id}/events/{event_id}/attachments"))
        .header(header::COOKIE, &owner_cookie)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from_stream(chunks))
        .unwrap();
    let upload = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        router.clone().oneshot(request),
    )
    .await
    .expect("the second field was waited for: no answer within 10 s")
    .unwrap();

    assert_status(&upload, StatusCode::BAD_REQUEST);
    assert_eq!(json_body(upload).await["error"], "multiple_files");
    assert_eq!(attachment_count(&db, &group_id, &event_id).await, 0);
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
    // One account starts all the uploads, so its upload limit (#219) is
    // raised to the pool size: what is under test here is the pool, and at
    // the production limit of two the third upload would be turned away
    // before it ever reached its body.
    let mut state = common::test_state(small_pool(&db, POOL_SIZE).await);
    state.upload_gate = manage_our_home_http_guard::UploadGate::new(
        manage_our_home_http_guard::gate::GLOBAL_UPLOADS,
        POOL_SIZE as usize,
    );
    let router = manage_our_home::build_router(state);

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

/// Waits until some backend of this database is blocked on a lock — the
/// request the test spawned, held up by the transaction the test keeps open.
/// With or without a lock on the handler's read, the request touches the row
/// that transaction holds, so it always gets there.
async fn wait_for_a_lock_wait(db: &PgPool, what: &str) {
    let mut waited = std::time::Duration::ZERO;
    loop {
        let blocked: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity
                            WHERE datname = current_database() AND wait_event_type = 'Lock')",
        )
        .fetch_one(db)
        .await
        .unwrap();
        if blocked {
            return;
        }
        assert!(
            waited < std::time::Duration::from_secs(10),
            "{what} never waited on the open transaction"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        waited += std::time::Duration::from_millis(20);
    }
}

/// Starts `PATCH /events/:id` with `body` while `concurrent` — a write still
/// open, standing in for a Google sync — holds the row, lets it through once
/// it is blocked, and returns its response.
async fn patch_against_an_open_write(
    router: &axum::Router,
    db: &PgPool,
    cookie: &str,
    path: String,
    body: serde_json::Value,
    concurrent: sqlx::Transaction<'static, sqlx::Postgres>,
) -> axum::http::Response<axum::body::Body> {
    let patch = tokio::spawn({
        let (router, cookie) = (router.clone(), cookie.to_string());
        async move { call(&router, Method::PATCH, &path, Some(&cookie), Some(body)).await }
    });
    wait_for_a_lock_wait(db, "the PATCH").await;
    concurrent.commit().await.unwrap();
    patch.await.unwrap()
}

/// #391: a sync turns an hour-bound event all-day while a `PATCH` sets it an
/// hourly rule. Read without a lock, the `PATCH` validated the rule against
/// the hour-bound row it saw, then wrote it on the row the sync had just
/// made all-day — a rule `validate_request` refuses on an all-day event.
/// Locked, the read waits for the sync and validates against what it wrote.
#[sqlx::test]
async fn a_patch_validates_its_rule_on_what_a_concurrent_write_committed(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "patch-race1@example.test", "owner-password1").await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let event_id = Uuid::parse_str(&create_event(&router, &cookie, &group_id).await).unwrap();

    let mut concurrent = manage_our_home::db::begin(&db).await.unwrap();
    sqlx::query("UPDATE events SET all_day = true WHERE id = $1")
        .bind(event_id)
        .execute(&mut *concurrent)
        .await
        .unwrap();

    let patch = patch_against_an_open_write(
        &router,
        &db,
        &cookie,
        format!("/groups/{group_id}/events/{event_id}"),
        serde_json::json!({"rrule": "FREQ=HOURLY;COUNT=5"}),
        concurrent,
    )
    .await;
    assert_status(&patch, StatusCode::BAD_REQUEST);

    let row: (bool, Option<String>) =
        sqlx::query_as("SELECT all_day, rrule FROM events WHERE id = $1")
            .bind(event_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(row, (true, None));
}

/// #391: a sync moves an event and gives it a rule while a `PATCH` renames
/// it. Read without a lock, the `PATCH` rewrote the bounds and the rule it
/// had read before the sync — erasing what the sync had just written.
/// Locked, it carries over what the sync committed.
#[sqlx::test]
async fn a_patch_keeps_the_bounds_and_rule_a_concurrent_write_committed(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "patch-race2@example.test", "owner-password1").await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let event_id = Uuid::parse_str(&create_event(&router, &cookie, &group_id).await).unwrap();

    let mut concurrent = manage_our_home::db::begin(&db).await.unwrap();
    let (moved_start, moved_end): (DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
        "UPDATE events SET starts_at = starts_at + interval '1 day',
                           ends_at = ends_at + interval '1 day',
                           rrule = 'FREQ=DAILY;COUNT=3'
         WHERE id = $1 RETURNING starts_at, ends_at",
    )
    .bind(event_id)
    .fetch_one(&mut *concurrent)
    .await
    .unwrap();

    let patch = patch_against_an_open_write(
        &router,
        &db,
        &cookie,
        format!("/groups/{group_id}/events/{event_id}"),
        serde_json::json!({"title": "Réunion déplacée"}),
        concurrent,
    )
    .await;
    assert_status(&patch, StatusCode::OK);

    let row: (String, DateTime<Utc>, DateTime<Utc>, Option<String>) =
        sqlx::query_as("SELECT title, starts_at, ends_at, rrule FROM events WHERE id = $1")
            .bind(event_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(
        row,
        (
            "Réunion déplacée".to_string(),
            moved_start,
            moved_end,
            Some("FREQ=DAILY;COUNT=3".to_string())
        )
    );
}

/// #391: an attachment committed while its event is being deleted. Read
/// without a lock, `delete_event` collected the storage keys before the
/// attachment row was visible and deleted no object; the row then went with
/// the event's cascade, leaving its bytes in storage with nothing pointing at
/// them. Locked, the read waits for the attachment's transaction (its
/// `FOR KEY SHARE`, `upload_attachment`) and collects its key.
///
/// `test_router`'s storage is unreachable, so a delete that saw the key fails
/// on it (500) and leaves event and attachment for the retry
/// (`event_delete_aborts_when_the_attachment_object_cannot_be_removed`); one
/// that missed it answers 204.
#[sqlx::test]
async fn an_event_delete_sees_an_attachment_a_concurrent_upload_committed(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "delete-race@example.test", "owner-password1").await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let event_id = create_event(&router, &cookie, &group_id).await;
    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind("delete-race@example.test")
        .fetch_one(&db)
        .await
        .unwrap();

    // What `upload_attachment`'s second transaction does, left open.
    let mut upload = with_family_scope(&db, &group_id).await;
    sqlx::query("SELECT id FROM events WHERE id = $1 FOR KEY SHARE")
        .bind(Uuid::parse_str(&event_id).unwrap())
        .execute(&mut *upload)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO event_attachments (event_id, uploaded_by, storage_key, filename, mime_type, size_bytes)
         VALUES ($1, $2, $3, 'ordonnance.png', 'image/png', 42)",
    )
    .bind(Uuid::parse_str(&event_id).unwrap())
    .bind(user_id)
    .bind(format!("{group_id}/{event_id}/{}", Uuid::new_v4()))
    .execute(&mut *upload)
    .await
    .unwrap();

    let delete = tokio::spawn({
        let (router, cookie) = (router.clone(), cookie.clone());
        let path = format!("/groups/{group_id}/events/{event_id}");
        async move { call(&router, Method::DELETE, &path, Some(&cookie), None).await }
    });
    wait_for_a_lock_wait(&db, "the DELETE").await;
    upload.commit().await.unwrap();

    let delete = delete.await.unwrap();
    assert_status(&delete, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        attachment_count(&db, &group_id, &event_id).await,
        1,
        "an attachment committed during the delete must not lose its row \
         while its object stays in storage"
    );
}

/// #391: the `PATCH`'s lock is `FOR NO KEY UPDATE`, which does not conflict
/// with the `FOR KEY SHARE` foreign keys into `events` take. An upload's
/// second transaction holds it through `put_object`; a plain `FOR UPDATE`
/// would make every `PATCH` of that event wait for the upload to finish.
#[sqlx::test]
async fn a_patch_does_not_wait_for_a_transaction_referencing_the_event(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "patch-keyshare@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let event_id = create_event(&router, &cookie, &group_id).await;

    let mut upload = with_family_scope(&db, &group_id).await;
    sqlx::query("SELECT id FROM events WHERE id = $1 FOR KEY SHARE")
        .bind(Uuid::parse_str(&event_id).unwrap())
        .execute(&mut *upload)
        .await
        .unwrap();

    let patch = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        call(
            &router,
            Method::PATCH,
            &format!("/groups/{group_id}/events/{event_id}"),
            Some(&cookie),
            Some(serde_json::json!({"title": "Réunion renommée"})),
        ),
    )
    .await
    .expect("the PATCH waited on a transaction that only references the event");
    assert_status(&patch, StatusCode::OK);
    upload.commit().await.unwrap();
}

// -- #620: private events -----------------------------------------------

/// A member of `group_id`, logged in, with `role` — added directly, as
/// `event_assignment_to_several_members` does, since the invitation flow is
/// not what these tests are about.
async fn private_event_member(
    router: &axum::Router,
    db: &PgPool,
    group_id: &str,
    email: &str,
    role: &str,
) -> (String, Uuid) {
    let cookie = register_verify_login(router, db, email, "member-password1").await;
    let id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(db)
        .await
        .unwrap();
    let mut tx = with_family_scope(db, group_id).await;
    sqlx::query(
        "INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, $3::group_role)",
    )
    .bind(Uuid::parse_str(group_id).unwrap())
    .bind(id)
    .bind(role)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (cookie, id)
}

async fn create_event_with(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    body: serde_json::Value,
) -> serde_json::Value {
    let res = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(cookie),
        Some(body),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await
}

/// The ids of the events listed to `cookie` over `[from, to]`, one per
/// occurrence.
async fn listed_event_ids(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<String> {
    let res = call(
        router,
        Method::GET,
        &format!(
            "/groups/{group_id}/events?from={}&to={}",
            urlenc(&from.to_rfc3339()),
            urlenc(&to.to_rfc3339())
        ),
        Some(cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    json_body(res).await["occurrences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap().to_string())
        .collect()
}

/// AC (#620): a private event is invisible to a member who is not one of
/// its participants — absent from the list, 404 on GET, PATCH and DELETE —
/// whatever their role: the owner and an admin are refused like anyone.
/// An assignee who did not create it sees it and may change and delete it.
///
/// Driven through a role that does not bypass RLS: the harness role would
/// show every row and prove nothing.
#[sqlx::test]
async fn a_private_event_is_visible_to_its_participants_alone(db: PgPool) {
    let (role, pool) = prescribed_role_pool(&db).await;
    let router = test_router(pool.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "priv-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let (creator_cookie, _) = private_event_member(
        &router,
        &db,
        &group_id,
        "priv-creator@example.test",
        "standard",
    )
    .await;
    let (assignee_cookie, assignee_id) = private_event_member(
        &router,
        &db,
        &group_id,
        "priv-assignee@example.test",
        "standard",
    )
    .await;
    let (outsider_cookie, _) = private_event_member(
        &router,
        &db,
        &group_id,
        "priv-outsider@example.test",
        "standard",
    )
    .await;
    let (admin_cookie, _) =
        private_event_member(&router, &db, &group_id, "priv-admin@example.test", "admin").await;

    let starts_at = Utc::now() + Duration::days(1);
    let shared = create_event_with(
        &router,
        &creator_cookie,
        &group_id,
        serde_json::json!({
            "title": "Dîner",
            "starts_at": starts_at,
            "ends_at": starts_at + Duration::hours(1),
        }),
    )
    .await;
    assert_eq!(shared["is_private"], false);
    let shared_id = shared["id"].as_str().unwrap().to_string();
    // The creator is not among the assignees: still a participant.
    let private = create_event_with(
        &router,
        &creator_cookie,
        &group_id,
        serde_json::json!({
            "title": "Surprise",
            "starts_at": starts_at,
            "ends_at": starts_at + Duration::hours(1),
            "assignee_ids": [assignee_id],
            "is_private": true,
        }),
    )
    .await;
    assert_eq!(private["is_private"], true);
    let private_id = private["id"].as_str().unwrap().to_string();
    let event_uri = format!("/groups/{group_id}/events/{private_id}");
    let (from, to) = (
        starts_at - Duration::hours(1),
        starts_at + Duration::hours(2),
    );

    for (who, cookie) in [
        ("outsider", &outsider_cookie),
        ("admin", &admin_cookie),
        ("owner", &owner_cookie),
    ] {
        assert_eq!(
            listed_event_ids(&router, cookie, &group_id, from, to).await,
            vec![shared_id.clone()],
            "{who}"
        );
        for (method, body) in [
            (Method::GET, None),
            (Method::PATCH, Some(serde_json::json!({"title": "Vu"}))),
            (Method::DELETE, None),
        ] {
            let res = call(&router, method.clone(), &event_uri, Some(cookie), body).await;
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{who} {method}");
        }
    }

    for cookie in [&creator_cookie, &assignee_cookie] {
        let mut listed = listed_event_ids(&router, cookie, &group_id, from, to).await;
        listed.sort();
        let mut expected = vec![shared_id.clone(), private_id.clone()];
        expected.sort();
        assert_eq!(listed, expected);
    }
    let get = call(
        &router,
        Method::GET,
        &event_uri,
        Some(&assignee_cookie),
        None,
    )
    .await;
    assert_status(&get, StatusCode::OK);
    assert_eq!(json_body(get).await["is_private"], true);

    let patch = call(
        &router,
        Method::PATCH,
        &event_uri,
        Some(&assignee_cookie),
        Some(serde_json::json!({"title": "Surprise avancée"})),
    )
    .await;
    assert_status(&patch, StatusCode::OK);
    let patched = json_body(patch).await;
    assert_eq!(patched["title"], "Surprise avancée");
    assert_eq!(patched["is_private"], true);

    let delete = call(
        &router,
        Method::DELETE,
        &event_uri,
        Some(&assignee_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::NO_CONTENT);

    drop_prescribed_role(&db, pool, &role).await;
}

/// AC (#620): a private series has no occurrence at all for a member who
/// is not one of its participants, while its creator gets every one.
#[sqlx::test]
async fn a_private_recurring_event_has_no_occurrence_for_a_non_participant(db: PgPool) {
    let (role, pool) = prescribed_role_pool(&db).await;
    let router = test_router(pool.clone());
    let creator_cookie = register_verify_login(
        &router,
        &db,
        "priv-series-creator@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &creator_cookie, "Foyer").await;
    let (outsider_cookie, _) = private_event_member(
        &router,
        &db,
        &group_id,
        "priv-series-outsider@example.test",
        "admin",
    )
    .await;

    let starts_at = Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap();
    let series = create_event_with(
        &router,
        &creator_cookie,
        &group_id,
        serde_json::json!({
            "title": "Kiné",
            "starts_at": starts_at,
            "ends_at": starts_at + Duration::hours(1),
            "rrule": "FREQ=WEEKLY;COUNT=4",
            "is_private": true,
        }),
    )
    .await;
    let series_id = series["id"].as_str().unwrap().to_string();
    let (from, to) = (
        starts_at - Duration::days(1),
        starts_at + Duration::days(30),
    );

    assert_eq!(
        listed_event_ids(&router, &creator_cookie, &group_id, from, to).await,
        vec![series_id; 4]
    );
    assert_eq!(
        listed_event_ids(&router, &outsider_cookie, &group_id, from, to).await,
        Vec::<String>::new()
    );

    drop_prescribed_role(&db, pool, &role).await;
}

/// AC (#620): an assignee who takes themselves off a private event they did
/// not create loses it — 404 from then on — and the creator keeps it. The
/// write that does it is answered, with the assignees it left.
#[sqlx::test]
async fn an_assignee_who_leaves_a_private_event_loses_it(db: PgPool) {
    let (role, pool) = prescribed_role_pool(&db).await;
    let router = test_router(pool.clone());
    let creator_cookie = register_verify_login(
        &router,
        &db,
        "priv-leave-creator@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &creator_cookie, "Foyer").await;
    let creator_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE email = 'priv-leave-creator@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();
    let (assignee_cookie, assignee_id) = private_event_member(
        &router,
        &db,
        &group_id,
        "priv-leave-assignee@example.test",
        "standard",
    )
    .await;

    let starts_at = Utc::now() + Duration::days(1);
    let event = create_event_with(
        &router,
        &creator_cookie,
        &group_id,
        serde_json::json!({
            "title": "Cadeau",
            "starts_at": starts_at,
            "ends_at": starts_at + Duration::hours(1),
            "assignee_ids": [creator_id, assignee_id],
            "is_private": true,
        }),
    )
    .await;
    let event_uri = format!(
        "/groups/{group_id}/events/{}",
        event["id"].as_str().unwrap()
    );

    let leave = call(
        &router,
        Method::PATCH,
        &event_uri,
        Some(&assignee_cookie),
        Some(serde_json::json!({"assignee_ids": [creator_id]})),
    )
    .await;
    assert_status(&leave, StatusCode::OK);
    assert_eq!(
        json_body(leave).await["assignee_ids"],
        serde_json::json!([creator_id])
    );

    let gone = call(
        &router,
        Method::GET,
        &event_uri,
        Some(&assignee_cookie),
        None,
    )
    .await;
    assert_status(&gone, StatusCode::NOT_FOUND);
    let kept = call(
        &router,
        Method::GET,
        &event_uri,
        Some(&creator_cookie),
        None,
    )
    .await;
    assert_status(&kept, StatusCode::OK);
    assert_eq!(
        json_body(kept).await["assignee_ids"],
        serde_json::json!([creator_id])
    );

    drop_prescribed_role(&db, pool, &role).await;
}

/// #620: an event is made private only by someone who will be one of its
/// participants. An admin who may edit a member's shared event but is not
/// on it is refused — the write would hide the event from its own author —
/// and allowed once they put themselves on it in the same request. Either
/// way the creator keeps it.
#[sqlx::test]
async fn making_an_event_private_needs_its_author_among_its_participants(db: PgPool) {
    let (role, pool) = prescribed_role_pool(&db).await;
    let router = test_router(pool.clone());
    let admin_cookie = register_verify_login(
        &router,
        &db,
        "priv-make-admin@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &admin_cookie, "Foyer").await;
    let admin_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE email = 'priv-make-admin@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();
    let (creator_cookie, _) = private_event_member(
        &router,
        &db,
        &group_id,
        "priv-make-creator@example.test",
        "standard",
    )
    .await;
    let event_id = create_event(&router, &creator_cookie, &group_id).await;
    let event_uri = format!("/groups/{group_id}/events/{event_id}");

    let refused = call(
        &router,
        Method::PATCH,
        &event_uri,
        Some(&admin_cookie),
        Some(serde_json::json!({"is_private": true})),
    )
    .await;
    assert_status(&refused, StatusCode::FORBIDDEN);
    let still_shared = call(&router, Method::GET, &event_uri, Some(&admin_cookie), None).await;
    assert_status(&still_shared, StatusCode::OK);
    assert_eq!(json_body(still_shared).await["is_private"], false);

    let allowed = call(
        &router,
        Method::PATCH,
        &event_uri,
        Some(&admin_cookie),
        Some(serde_json::json!({"is_private": true, "assignee_ids": [admin_id]})),
    )
    .await;
    assert_status(&allowed, StatusCode::OK);
    assert_eq!(json_body(allowed).await["is_private"], true);
    for cookie in [&admin_cookie, &creator_cookie] {
        let get = call(&router, Method::GET, &event_uri, Some(cookie), None).await;
        assert_status(&get, StatusCode::OK);
    }

    drop_prescribed_role(&db, pool, &role).await;
}

// -- #562: national days ----------------------------------------------------

/// The `(date, label)` pairs of a `GET /groups/:id/national-days` body.
fn national_day_labels(body: &serde_json::Value) -> Vec<(String, String)> {
    body["days"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["date"].as_str().unwrap().to_string(),
                d["label"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// AC (#562): the national days of a range, for the family's members only,
/// with its Alsace-Moselle setting adding Good Friday and 26 December, and
/// the range bounded to a leap year.
///
/// Driven through a role that does not bypass RLS, the one production runs
/// under: the setting is read from `groups` under its policies.
#[sqlx::test]
async fn national_days_follow_the_family_setting_and_stay_with_its_members(db: PgPool) {
    let (role, pool) = prescribed_role_pool(&db).await;
    let router = test_router(pool.clone());
    let owner =
        register_verify_login(&router, &db, "nd-owner@example.test", "owner-password1").await;
    let outsider = register_verify_login(
        &router,
        &db,
        "nd-outsider@example.test",
        "outsider-password1",
    )
    .await;
    let group_id = create_group(&router, &owner, "Foyer").await;
    let year_2026 = format!("/groups/{group_id}/national-days?from=2026-01-01&to=2026-12-31");

    let res = call(&router, Method::GET, &year_2026, Some(&owner), None).await;
    assert_status(&res, StatusCode::OK);
    let body = json_body(res).await;
    let days = national_day_labels(&body);
    assert_eq!(days.len(), 15, "{days:?}");
    for (date, label) in [
        ("2026-05-14", "Ascension"),
        ("2026-03-29", "Passage à l'heure d'été"),
        ("2026-10-25", "Passage à l'heure d'hiver"),
        ("2026-05-31", "Fête des mères"),
        ("2026-06-21", "Fête des pères"),
    ] {
        assert!(days.contains(&(date.into(), label.into())), "{label}");
    }
    assert!(!days
        .iter()
        .any(|(_, l)| l == "Vendredi saint" || l == "Saint-Étienne"));
    assert_eq!(
        body["days"][0],
        serde_json::json!({"date": "2026-01-01", "kind": "public_holiday", "label": "Jour de l'an"})
    );
    let count = |kind: &str| {
        body["days"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["kind"] == kind)
            .count()
    };
    assert_eq!(
        (
            count("public_holiday"),
            count("clock_change"),
            count("observance")
        ),
        (11, 2, 2)
    );

    // Both bounds are included, nothing outside them is.
    let spring_2027 = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/national-days?from=2027-03-28&to=2027-05-17"),
        Some(&owner),
        None,
    )
    .await;
    assert_status(&spring_2027, StatusCode::OK);
    assert_eq!(
        national_day_labels(&json_body(spring_2027).await),
        [
            ("2027-03-28", "Passage à l'heure d'été"),
            ("2027-03-29", "Lundi de Pâques"),
            ("2027-05-01", "Fête du Travail"),
            ("2027-05-06", "Ascension"),
            ("2027-05-08", "Victoire 1945"),
            ("2027-05-17", "Lundi de Pentecôte"),
        ]
        .map(|(d, l)| (d.to_string(), l.to_string()))
    );

    let set = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}"),
        Some(&owner),
        Some(serde_json::json!({"alsace_moselle": true})),
    )
    .await;
    assert_status(&set, StatusCode::OK);
    let res = call(&router, Method::GET, &year_2026, Some(&owner), None).await;
    assert_status(&res, StatusCode::OK);
    let days = national_day_labels(&json_body(res).await);
    assert_eq!(days.len(), 17, "{days:?}");
    assert!(days.contains(&("2026-04-03".into(), "Vendredi saint".into())));
    assert!(days.contains(&("2026-12-26".into(), "Saint-Étienne".into())));

    let res = call(&router, Method::GET, &year_2026, Some(&outsider), None).await;
    assert_status(&res, StatusCode::NOT_FOUND);
    let res = call(
        &router,
        Method::GET,
        &format!(
            "/groups/{}/national-days?from=2026-01-01&to=2026-12-31",
            Uuid::new_v4()
        ),
        Some(&owner),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);
    let res = call(&router, Method::GET, &year_2026, None, None).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);

    for (from, to, code) in [
        // 367 days, both ends counted.
        ("2026-01-01", "2027-01-02", "range_too_wide"),
        ("2028-01-01", "2029-01-01", "range_too_wide"),
        ("2020-01-01", "2030-12-31", "range_too_wide"),
        ("2026-07-15", "2026-07-14", "to_before_from"),
    ] {
        let res = call(
            &router,
            Method::GET,
            &format!("/groups/{group_id}/national-days?from={from}&to={to}"),
            Some(&owner),
            None,
        )
        .await;
        assert_status(&res, StatusCode::BAD_REQUEST);
        assert_eq!(json_body(res).await["error"], code, "{from}..{to}");
    }
    // A whole leap year is the widest range served.
    let res = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/national-days?from=2028-01-01&to=2028-12-31"),
        Some(&owner),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);

    drop(router);
    drop_prescribed_role(&db, pool, &role).await;
}

// -- #563: school holidays ----------------------------------------------------

/// The `(date, end, label)` of the `school_holiday` entries of a
/// `GET /groups/:id/national-days` body.
fn school_holidays_of(body: &serde_json::Value) -> Vec<(String, String, String)> {
    body["days"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["kind"] == "school_holiday")
        .map(|d| {
            (
                d["date"].as_str().unwrap().to_string(),
                d["end"].as_str().unwrap().to_string(),
                d["label"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn period(date: &str, end: &str, label: &str) -> (String, String, String) {
    (date.to_string(), end.to_string(), label.to_string())
}

/// AC (#563): with the cache filled from the recorded answer of the
/// Éducation nationale's open data — no network — a family sees the school
/// holidays of its own zone only, each a whole period, and none without a
/// zone.
///
/// Driven through a role that does not bypass RLS, the one production runs
/// under: `school_holidays` has no policy and is read under the
/// application's grants.
#[sqlx::test]
async fn school_holidays_follow_the_family_zone(db: PgPool) {
    use manage_our_home::agenda::school_holidays::{parse_calendar, replace_cache, school_years};

    let recorded: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/school_calendar/2026-2027_2027-2028.json"
    ))
    .unwrap();
    let years = school_years(chrono::NaiveDate::from_ymd_opt(2026, 11, 5).unwrap());
    let periods = parse_calendar(&recorded, &years).unwrap();
    replace_cache(&db, &years, &periods).await.unwrap();

    let (role, pool) = prescribed_role_pool(&db).await;
    let router = test_router(pool.clone());
    let owner =
        register_verify_login(&router, &db, "sh-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner, "Foyer").await;
    let get = async |from: &str, to: &str| {
        let path = format!("/groups/{group_id}/national-days?from={from}&to={to}");
        let res = call(&router, Method::GET, &path, Some(&owner), None).await;
        assert_status(&res, StatusCode::OK);
        json_body(res).await
    };
    let set_zone = async |zone: serde_json::Value| {
        let res = call(
            &router,
            Method::PATCH,
            &format!("/groups/{group_id}"),
            Some(&owner),
            Some(serde_json::json!({ "school_zone": zone })),
        )
        .await;
        assert_status(&res, StatusCode::OK);
    };

    // No zone: the national days only.
    let body = get("2026-09-01", "2027-08-31").await;
    assert_eq!(school_holidays_of(&body), []);
    assert_eq!(national_day_labels(&body).len(), 15);

    set_zone(serde_json::json!("B")).await;
    let body = get("2026-09-01", "2027-08-31").await;
    assert_eq!(
        school_holidays_of(&body),
        [
            period("2026-10-17", "2026-11-01", "Vacances de la Toussaint"),
            period("2026-12-19", "2027-01-03", "Vacances de Noël"),
            period("2027-02-20", "2027-03-07", "Vacances d'Hiver"),
            period("2027-04-17", "2027-05-02", "Vacances de Printemps"),
            period("2027-07-03", "2027-09-01", "Vacances d'Été"),
        ]
    );
    // Merged by date with the national days.
    let dates: Vec<&str> = body["days"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["date"].as_str().unwrap())
        .collect();
    let mut sorted = dates.clone();
    sorted.sort();
    assert_eq!(dates, sorted);
    // The JSON of a period; a one-day entry still has no `end`.
    let days = body["days"].as_array().unwrap();
    assert!(days.contains(&serde_json::json!({
        "date": "2026-12-19",
        "kind": "school_holiday",
        "label": "Vacances de Noël",
        "end": "2027-01-03",
    })));
    assert!(days.contains(&serde_json::json!({
        "date": "2026-12-25",
        "kind": "public_holiday",
        "label": "Noël",
    })));

    // A period sharing one day with the range comes whole, at either end;
    // one sharing none is left out.
    assert_eq!(
        school_holidays_of(&get("2027-01-03", "2027-02-20").await),
        [
            period("2026-12-19", "2027-01-03", "Vacances de Noël"),
            period("2027-02-20", "2027-03-07", "Vacances d'Hiver"),
        ]
    );
    assert_eq!(
        school_holidays_of(&get("2027-01-04", "2027-02-19").await),
        []
    );

    // Another zone, other winter holidays.
    set_zone(serde_json::json!("A")).await;
    assert_eq!(
        school_holidays_of(&get("2027-02-01", "2027-03-31").await),
        [period("2027-02-13", "2027-02-28", "Vacances d'Hiver")]
    );
    set_zone(serde_json::json!("C")).await;
    assert_eq!(
        school_holidays_of(&get("2027-02-01", "2027-03-31").await),
        [period("2027-02-06", "2027-02-21", "Vacances d'Hiver")]
    );

    // The zone cleared: none again.
    set_zone(serde_json::Value::Null).await;
    assert_eq!(
        school_holidays_of(&get("2026-09-01", "2027-08-31").await),
        []
    );

    drop(router);
    drop_prescribed_role(&db, pool, &role).await;
}

// -- #708: PATCH of a series with a scope ----------------------------------

/// 09:00 on the Paris wall clock `n` days after the day after tomorrow —
/// in the future, so the reminders of these series get scheduled.
fn nine_paris(n: i64) -> DateTime<Utc> {
    use manage_our_home_shared::validation::agenda::paris_date;
    let day = paris_date(Utc::now()) + Duration::days(2 + n);
    rrule::Tz::Europe__Paris
        .from_local_datetime(&day.and_hms_opt(9, 0, 0).unwrap())
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

/// The occurrences listed to `cookie` over `[from, to]`, as
/// `(id, title, occurrence_starts_at, completed)`.
async fn listed_occurrences(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<(String, String, DateTime<Utc>, bool)> {
    let res = call(
        router,
        Method::GET,
        &format!(
            "/groups/{group_id}/events?from={}&to={}",
            urlenc(&from.to_rfc3339()),
            urlenc(&to.to_rfc3339())
        ),
        Some(cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let mut listed: Vec<_> = json_body(res).await["occurrences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            (
                o["id"].as_str().unwrap().to_string(),
                o["title"].as_str().unwrap().to_string(),
                instant(o, "occurrence_starts_at"),
                o["completed_at"].is_string(),
            )
        })
        .collect();
    listed.sort_by_key(|o| o.2);
    listed
}

/// What `scheduled_notifications` holds for `event_id`, by occurrence.
async fn scheduled_occurrences(db: &PgPool, event_id: &str) -> Vec<DateTime<Utc>> {
    sqlx::query_scalar(
        "SELECT occurrence_at FROM scheduled_notifications WHERE event_id = $1::uuid ORDER BY occurrence_at",
    )
    .bind(event_id)
    .fetch_all(db)
    .await
    .unwrap()
}

async fn reminder_count(db: &PgPool, event_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM event_reminders WHERE event_id = $1::uuid")
        .bind(event_id)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn patch_event(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    event_id: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    call(
        router,
        Method::PATCH,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(cookie),
        Some(body),
    )
    .await
}

/// A daily series of five at 09:00 Paris, assigned to `assignee_ids`, with a
/// reminder at the start of each occurrence. Returns its id.
async fn daily_series_with_a_reminder(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    extra: serde_json::Value,
) -> String {
    let mut body = serde_json::json!({
        "title": "Piscine",
        "starts_at": nine_paris(0),
        "ends_at": nine_paris(0) + Duration::hours(1),
        "rrule": "FREQ=DAILY;COUNT=5",
    });
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let series = create_event_with(router, cookie, group_id, body).await;
    let id = series["id"].as_str().unwrap().to_string();
    let reminder = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/events/{id}/reminders"),
        Some(cookie),
        Some(serde_json::json!({"offset_minutes": 0})),
    )
    .await;
    assert_status(&reminder, StatusCode::CREATED);
    id
}

/// AC (#708): `this` takes one occurrence out of its series — the series
/// excludes it, and a one-off event with the change takes its place, with
/// the series' assignees and reminders, while the attachments stay on the
/// series. Every other occurrence is listed as it was.
#[sqlx::test]
async fn editing_one_occurrence_detaches_it_from_its_series(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "scope-this@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let (_, member_id) = private_event_member(
        &router,
        &db,
        &group_id,
        "scope-this-member@example.test",
        "standard",
    )
    .await;
    let owner_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE email = 'scope-this@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();
    let series_id = daily_series_with_a_reminder(
        &router,
        &owner_cookie,
        &group_id,
        serde_json::json!({"assignee_ids": [owner_id, member_id]}),
    )
    .await;
    let mut tx = with_family_scope(&db, &group_id).await;
    sqlx::query(
        "INSERT INTO event_attachments (event_id, uploaded_by, storage_key, filename, mime_type, size_bytes)
         VALUES ($1::uuid, $2, 'k-708', 'plan.png', 'image/png', 10)",
    )
    .bind(&series_id)
    .bind(owner_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let (from, to) = (nine_paris(-1), nine_paris(10));
    let before = listed_occurrences(&router, &owner_cookie, &group_id, from, to).await;
    assert_eq!(before.len(), 5);

    let moved = nine_paris(2) + Duration::hours(2);
    let res = patch_event(
        &router,
        &owner_cookie,
        &group_id,
        &series_id,
        serde_json::json!({
            "scope": "this",
            "occurrence_at": nine_paris(2),
            "title": "Piscine (décalée)",
            "starts_at": moved,
            "ends_at": moved + Duration::hours(1),
        }),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let created = json_body(res).await;
    let created_id = created["id"].as_str().unwrap().to_string();
    assert_ne!(created_id, series_id);
    assert_eq!(created["rrule"], serde_json::Value::Null);
    assert_eq!(created["title"], "Piscine (décalée)");
    assert_eq!(instant(&created, "starts_at"), moved);
    let mut both = [owner_id, member_id];
    both.sort();
    assert_eq!(created["assignee_ids"], serde_json::json!(both));

    let after = listed_occurrences(&router, &owner_cookie, &group_id, from, to).await;
    let mut expected: Vec<_> = before
        .iter()
        .filter(|o| o.2 != nine_paris(2))
        .cloned()
        .collect();
    expected.push((created_id.clone(), "Piscine (décalée)".into(), moved, false));
    expected.sort_by_key(|o| o.2);
    assert_eq!(after, expected);

    assert_eq!(reminder_count(&db, &created_id).await, 1);
    assert_eq!(scheduled_occurrences(&db, &created_id).await, vec![moved]);
    assert_eq!(
        scheduled_occurrences(&db, &series_id).await,
        [0, 1, 3, 4].map(nine_paris).to_vec()
    );
    assert_eq!(attachment_count(&db, &group_id, &series_id).await, 1);
    assert_eq!(attachment_count(&db, &group_id, &created_id).await, 0);
}

/// AC (#708): `this_and_following` cuts the series right before the
/// occurrence and carries the rest on in a new series with the change: the
/// occurrences before it are listed exactly as they were, the series' count
/// is shared between the two, and reminders follow each.
#[sqlx::test]
async fn editing_an_occurrence_and_the_following_ones_splits_the_series(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "scope-follow@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let series_id =
        daily_series_with_a_reminder(&router, &owner_cookie, &group_id, serde_json::json!({}))
            .await;
    let (from, to) = (nine_paris(-1), nine_paris(10));
    let before = listed_occurrences(&router, &owner_cookie, &group_id, from, to).await;

    let res = patch_event(
        &router,
        &owner_cookie,
        &group_id,
        &series_id,
        serde_json::json!({
            "scope": "this_and_following",
            "occurrence_at": nine_paris(3),
            "title": "Aquagym",
            "starts_at": nine_paris(3) + Duration::hours(1),
            "ends_at": nine_paris(3) + Duration::hours(2),
        }),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let created = json_body(res).await;
    let created_id = created["id"].as_str().unwrap().to_string();
    assert_ne!(created_id, series_id);
    assert_eq!(created["rrule"], "FREQ=DAILY;COUNT=2");

    let after = listed_occurrences(&router, &owner_cookie, &group_id, from, to).await;
    assert_eq!(
        after[..3],
        before[..3],
        "the occurrences before stay as they were"
    );
    assert_eq!(
        after[3..],
        [3, 4]
            .map(|n| (
                created_id.clone(),
                "Aquagym".to_string(),
                nine_paris(n) + Duration::hours(1),
                false
            ))
            .to_vec()
    );

    assert_eq!(
        scheduled_occurrences(&db, &series_id).await,
        [0, 1, 2].map(nine_paris).to_vec()
    );
    assert_eq!(
        scheduled_occurrences(&db, &created_id).await,
        [3, 4].map(|n| nine_paris(n) + Duration::hours(1)).to_vec()
    );
}

/// AC (#708): from the series' first occurrence, `this_and_following` is
/// the whole series — the row is updated in place.
#[sqlx::test]
async fn this_and_following_from_the_first_occurrence_updates_the_series(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "scope-first@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let series_id =
        daily_series_with_a_reminder(&router, &owner_cookie, &group_id, serde_json::json!({}))
            .await;

    let res = patch_event(
        &router,
        &owner_cookie,
        &group_id,
        &series_id,
        serde_json::json!({
            "scope": "this_and_following",
            "occurrence_at": nine_paris(0),
            "title": "Aquagym",
        }),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["id"], series_id);
    assert_eq!(body["rrule"], "FREQ=DAILY;COUNT=5");
    let listed = listed_occurrences(
        &router,
        &owner_cookie,
        &group_id,
        nine_paris(-1),
        nine_paris(10),
    )
    .await;
    assert_eq!(listed.len(), 5);
    assert!(listed.iter().all(|o| o.0 == series_id && o.1 == "Aquagym"));
}

/// AC (#708): the first occurrence the series *lists* is not its start when
/// earlier ones are excluded. `this_and_following` from there still cuts
/// the series: a move of that occurrence moves the following ones, and the
/// count left is shared — not five more occurrences from the moved one.
#[sqlx::test]
async fn this_and_following_after_an_excluded_head_still_splits_the_series(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "scope-excluded-head@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let series_id =
        daily_series_with_a_reminder(&router, &owner_cookie, &group_id, serde_json::json!({}))
            .await;
    let detached = patch_event(
        &router,
        &owner_cookie,
        &group_id,
        &series_id,
        serde_json::json!({"scope": "this", "occurrence_at": nine_paris(0), "title": "Seule"}),
    )
    .await;
    assert_status(&detached, StatusCode::OK);

    let res = patch_event(
        &router,
        &owner_cookie,
        &group_id,
        &series_id,
        serde_json::json!({
            "scope": "this_and_following",
            "occurrence_at": nine_paris(1),
            "starts_at": nine_paris(1) + Duration::hours(1),
            "ends_at": nine_paris(1) + Duration::hours(2),
        }),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let created = json_body(res).await;
    assert_eq!(created["rrule"], "FREQ=DAILY;COUNT=4");

    let listed = listed_occurrences(
        &router,
        &owner_cookie,
        &group_id,
        nine_paris(-1),
        nine_paris(10),
    )
    .await;
    let starts: Vec<_> = listed.iter().map(|o| o.2).collect();
    let mut expected = vec![nine_paris(0)];
    expected.extend([1, 2, 3, 4].map(|n| nine_paris(n) + Duration::hours(1)));
    assert_eq!(starts, expected);
}

/// AC (#708): a recurring task keeps its completions where they belong — the
/// ones from the occurrence on move to the new series, a detached
/// occurrence carries its own onto the one-off.
#[sqlx::test]
async fn a_recurring_task_split_keeps_each_completion_on_its_occurrence(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "scope-task@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let series_id = daily_series_with_a_reminder(
        &router,
        &owner_cookie,
        &group_id,
        serde_json::json!({"is_task": true}),
    )
    .await;
    for n in [0, 1, 3] {
        let res = patch_event(
            &router,
            &owner_cookie,
            &group_id,
            &series_id,
            serde_json::json!({"completed": true, "occurrence_at": nine_paris(n)}),
        )
        .await;
        assert_status(&res, StatusCode::OK);
    }

    // The second occurrence alone, then the fourth on.
    let res = patch_event(
        &router,
        &owner_cookie,
        &group_id,
        &series_id,
        serde_json::json!({"scope": "this", "occurrence_at": nine_paris(1)}),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let detached = json_body(res).await;
    assert!(detached["completed_at"].is_string());
    let detached_id = detached["id"].as_str().unwrap().to_string();

    let moved = nine_paris(3) + Duration::minutes(30);
    let res = patch_event(
        &router,
        &owner_cookie,
        &group_id,
        &series_id,
        serde_json::json!({
            "scope": "this_and_following",
            "occurrence_at": nine_paris(3),
            "starts_at": moved,
            "ends_at": moved + Duration::hours(1),
        }),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let following_id = json_body(res).await["id"].as_str().unwrap().to_string();

    let listed = listed_occurrences(
        &router,
        &owner_cookie,
        &group_id,
        nine_paris(-1),
        nine_paris(10),
    )
    .await;
    let summary: Vec<_> = listed
        .iter()
        .map(|(id, _, at, done)| (id.clone(), *at, *done))
        .collect();
    assert_eq!(
        summary,
        vec![
            (series_id.clone(), nine_paris(0), true),
            (detached_id, nine_paris(1), true),
            (series_id.clone(), nine_paris(2), false),
            (following_id.clone(), moved, true),
            (following_id, nine_paris(4) + Duration::minutes(30), false),
        ]
    );
}

/// AC (#708): what a scope other than `all` refuses, and who may send it.
#[sqlx::test]
async fn a_scope_is_refused_where_it_has_no_meaning(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "scope-refused@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let (member_cookie, _) = private_event_member(
        &router,
        &db,
        &group_id,
        "scope-refused-member@example.test",
        "standard",
    )
    .await;
    let series_id =
        daily_series_with_a_reminder(&router, &owner_cookie, &group_id, serde_json::json!({}))
            .await;
    let one_off = create_event_with(
        &router,
        &owner_cookie,
        &group_id,
        serde_json::json!({
            "title": "Dentiste",
            "starts_at": nine_paris(0),
            "ends_at": nine_paris(0) + Duration::hours(1),
        }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let refused = |id: &str, body: serde_json::Value| {
        let (router, cookie, group_id, id) = (
            router.clone(),
            owner_cookie.clone(),
            group_id.clone(),
            id.to_string(),
        );
        async move {
            let res = patch_event(&router, &cookie, &group_id, &id, body).await;
            assert_status(&res, StatusCode::BAD_REQUEST);
            json_body(res).await["error"].as_str().unwrap().to_string()
        }
    };
    for scope in ["this", "this_and_following"] {
        assert_eq!(
            refused(
                &series_id,
                serde_json::json!({"scope": scope, "title": "X"})
            )
            .await,
            "scope_requires_occurrence"
        );
        assert_eq!(
            refused(
                &one_off,
                serde_json::json!({"scope": scope, "occurrence_at": nine_paris(0)})
            )
            .await,
            "scope_requires_recurring_event"
        );
        for not_one in [nine_paris(1) + Duration::minutes(5), nine_paris(5)] {
            assert_eq!(
                refused(
                    &series_id,
                    serde_json::json!({"scope": scope, "occurrence_at": not_one})
                )
                .await,
                "occurrence_not_in_series"
            );
        }
    }

    // Same permission as any PATCH: a plain member may not change someone
    // else's shared series, one occurrence or all.
    let res = patch_event(
        &router,
        &member_cookie,
        &group_id,
        &series_id,
        serde_json::json!({"scope": "this", "occurrence_at": nine_paris(1), "title": "X"}),
    )
    .await;
    assert_status(&res, StatusCode::FORBIDDEN);

    // A series mirrored from an imported calendar is changed whole or not
    // at all.
    let owner_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE email = 'scope-refused@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();
    let mut tx = with_family_scope(&db, &group_id).await;
    sqlx::query(
        "WITH import AS (
             INSERT INTO calendar_imports (group_id, created_by, label, feed_url)
             VALUES ($1::uuid, $2, 'Agenda', '\\x00'::bytea) RETURNING id
         )
         INSERT INTO calendar_import_events (calendar_import_id, event_id, external_uid)
         SELECT id, $3::uuid, 'uid-708' FROM import",
    )
    .bind(&group_id)
    .bind(owner_id)
    .bind(&series_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    for scope in ["this", "this_and_following"] {
        assert_eq!(
            refused(
                &series_id,
                serde_json::json!({"scope": scope, "occurrence_at": nine_paris(1)})
            )
            .await,
            "scope_not_allowed_for_imported_event"
        );
    }
    let whole = patch_event(
        &router,
        &owner_cookie,
        &group_id,
        &series_id,
        serde_json::json!({"scope": "all", "title": "Piscine municipale"}),
    )
    .await;
    assert_status(&whole, StatusCode::OK);

    // Nothing above changed the series.
    let listed = listed_occurrences(
        &router,
        &owner_cookie,
        &group_id,
        nine_paris(-1),
        nine_paris(10),
    )
    .await;
    assert_eq!(
        listed
            .iter()
            .filter(|o| o.0 == series_id)
            .map(|o| o.2)
            .collect::<Vec<_>>(),
        (0..5).map(nine_paris).collect::<Vec<_>>()
    );
}

/// AC (#708): on a private series, an assignee who did not create it may
/// split it like any participant. What it creates is private too, keeps the
/// series' creator, and stays out of a non-participant's agenda.
///
/// Driven through a role that does not bypass RLS: the new row is written
/// under the policy of `events`.
#[sqlx::test]
async fn an_assignee_splits_a_private_series_into_private_events(db: PgPool) {
    let (role, pool) = prescribed_role_pool(&db).await;
    let router = test_router(pool.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "scope-priv-owner@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let (creator_cookie, creator_id) = private_event_member(
        &router,
        &db,
        &group_id,
        "scope-priv-creator@example.test",
        "standard",
    )
    .await;
    let (assignee_cookie, assignee_id) = private_event_member(
        &router,
        &db,
        &group_id,
        "scope-priv-assignee@example.test",
        "standard",
    )
    .await;
    let series_id = daily_series_with_a_reminder(
        &router,
        &creator_cookie,
        &group_id,
        serde_json::json!({"assignee_ids": [creator_id, assignee_id], "is_private": true}),
    )
    .await;

    let mut created_ids = Vec::new();
    for (scope, n) in [("this", 1), ("this_and_following", 3)] {
        let res = patch_event(
            &router,
            &assignee_cookie,
            &group_id,
            &series_id,
            serde_json::json!({"scope": scope, "occurrence_at": nine_paris(n), "title": "Cadeau"}),
        )
        .await;
        assert_status(&res, StatusCode::OK);
        let created = json_body(res).await;
        assert_eq!(created["is_private"], true, "{scope}");
        assert_eq!(created["created_by"], creator_id.to_string(), "{scope}");
        created_ids.push(created["id"].as_str().unwrap().to_string());
    }

    let (from, to) = (nine_paris(-1), nine_paris(10));
    for cookie in [&creator_cookie, &assignee_cookie] {
        let ids: Vec<_> = listed_occurrences(&router, cookie, &group_id, from, to)
            .await
            .into_iter()
            .map(|o| o.0)
            .collect();
        assert_eq!(
            ids,
            [
                &series_id,
                &created_ids[0],
                &series_id,
                &created_ids[1],
                &created_ids[1]
            ]
            .map(|s| s.clone())
            .to_vec()
        );
    }
    assert!(
        listed_occurrences(&router, &owner_cookie, &group_id, from, to)
            .await
            .is_empty()
    );

    drop(router);
    drop_prescribed_role(&db, pool, &role).await;
}

// -- #710: DELETE of a series with a scope ---------------------------------

async fn delete_scoped(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    event_id: &str,
    query: &str,
) -> axum::response::Response {
    call(
        router,
        Method::DELETE,
        &format!("/groups/{group_id}/events/{event_id}{query}"),
        Some(cookie),
        None,
    )
    .await
}

/// `?scope=…&occurrence_at=…` for the occurrence at `at`.
fn scoped(scope: &str, at: DateTime<Utc>) -> String {
    format!("?scope={scope}&occurrence_at={}", urlenc(&at.to_rfc3339()))
}

/// The occurrences of `event_id` marked done, in order.
async fn completion_instants(db: &PgPool, group_id: &str, event_id: &str) -> Vec<DateTime<Utc>> {
    let mut tx = with_family_scope(db, group_id).await;
    let at = sqlx::query_scalar(
        "SELECT occurrence_at FROM event_occurrence_completions WHERE event_id = $1 ORDER BY occurrence_at",
    )
    .bind(Uuid::parse_str(event_id).unwrap())
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    at
}

/// A recurring task from `daily_series_with_a_reminder`, its occurrences
/// `done` marked done.
async fn daily_task_done_on(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    done: &[i64],
) -> String {
    let series_id = daily_series_with_a_reminder(
        router,
        cookie,
        group_id,
        serde_json::json!({"is_task": true}),
    )
    .await;
    for n in done {
        let res = patch_event(
            router,
            cookie,
            group_id,
            &series_id,
            serde_json::json!({"completed": true, "occurrence_at": nine_paris(*n)}),
        )
        .await;
        assert_status(&res, StatusCode::OK);
    }
    series_id
}

/// AC (#710): `this` takes one occurrence out of its series — and nothing
/// else: the series keeps its other occurrences, their completions and
/// notifications, its reminder and its attachments. The occurrence's own
/// completion and pending notification go with it.
#[sqlx::test]
async fn deleting_one_occurrence_excludes_it_from_its_series(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "del-this@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let owner_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE email = 'del-this@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();
    let series_id = daily_task_done_on(&router, &owner_cookie, &group_id, &[2, 3]).await;
    let mut tx = with_family_scope(&db, &group_id).await;
    sqlx::query(
        "INSERT INTO event_attachments (event_id, uploaded_by, storage_key, filename, mime_type, size_bytes)
         VALUES ($1::uuid, $2, 'k-710', 'plan.png', 'image/png', 10)",
    )
    .bind(&series_id)
    .bind(owner_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (from, to) = (nine_paris(-1), nine_paris(10));
    let before = listed_occurrences(&router, &owner_cookie, &group_id, from, to).await;
    assert_eq!(before.len(), 5);

    let res = delete_scoped(
        &router,
        &owner_cookie,
        &group_id,
        &series_id,
        &scoped("this", nine_paris(2)),
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);

    let after = listed_occurrences(&router, &owner_cookie, &group_id, from, to).await;
    let expected: Vec<_> = before
        .iter()
        .filter(|o| o.2 != nine_paris(2))
        .cloned()
        .collect();
    assert_eq!(after, expected);
    assert_eq!(
        completion_instants(&db, &group_id, &series_id).await,
        vec![nine_paris(3)]
    );
    assert_eq!(
        scheduled_occurrences(&db, &series_id).await,
        [0, 1, 3, 4].map(nine_paris).to_vec()
    );
    assert_eq!(attachment_count(&db, &group_id, &series_id).await, 1);
    assert_eq!(reminder_count(&db, &series_id).await, 1);

    // Gone from the series, the occurrence is no longer one of it.
    let again = delete_scoped(
        &router,
        &owner_cookie,
        &group_id,
        &series_id,
        &scoped("this", nine_paris(2)),
    )
    .await;
    assert_status(&again, StatusCode::BAD_REQUEST);
    assert_eq!(json_body(again).await["error"], "occurrence_not_in_series");
}

/// AC (#710): `this_and_following` cuts the series right before the
/// occurrence, with the rule #708's split writes: the occurrences before it
/// are listed as they were, none after, and the completions and pending
/// notifications from the occurrence on are gone.
#[sqlx::test]
async fn deleting_an_occurrence_and_the_following_ones_cuts_the_series(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "del-follow@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let series_id = daily_task_done_on(&router, &owner_cookie, &group_id, &[1, 3]).await;
    let (from, to) = (nine_paris(-1), nine_paris(30));
    let before = listed_occurrences(&router, &owner_cookie, &group_id, from, to).await;

    let res = delete_scoped(
        &router,
        &owner_cookie,
        &group_id,
        &series_id,
        &scoped("this_and_following", nine_paris(3)),
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);

    let after = listed_occurrences(&router, &owner_cookie, &group_id, from, to).await;
    assert_eq!(after, before[..3]);
    assert_eq!(
        completion_instants(&db, &group_id, &series_id).await,
        vec![nine_paris(1)]
    );
    assert_eq!(
        scheduled_occurrences(&db, &series_id).await,
        [0, 1, 2].map(nine_paris).to_vec()
    );

    // The same cut as a split of the same series from the same occurrence.
    let twin_id = daily_task_done_on(&router, &owner_cookie, &group_id, &[]).await;
    let split = patch_event(
        &router,
        &owner_cookie,
        &group_id,
        &twin_id,
        serde_json::json!({"scope": "this_and_following", "occurrence_at": nine_paris(3), "title": "X"}),
    )
    .await;
    assert_status(&split, StatusCode::OK);
    let rule_of = |id: String| {
        let (router, cookie, group_id) = (router.clone(), owner_cookie.clone(), group_id.clone());
        async move {
            let res = call(
                &router,
                Method::GET,
                &format!("/groups/{group_id}/events/{id}"),
                Some(&cookie),
                None,
            )
            .await;
            assert_status(&res, StatusCode::OK);
            json_body(res).await["rrule"].as_str().unwrap().to_string()
        }
    };
    let cut = rule_of(series_id.clone()).await;
    assert!(cut.contains("UNTIL="), "{cut}");
    assert_eq!(cut, rule_of(twin_id).await);
}

/// AC (#710): from the series' first occurrence, `this_and_following` is the
/// whole series — the row goes, as with `all` or no scope at all.
#[sqlx::test]
async fn deleting_from_the_first_occurrence_on_deletes_the_series(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "del-first@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let mut series = Vec::new();
    for query in [
        scoped("this_and_following", nine_paris(0)),
        scoped("all", nine_paris(2)),
        String::new(),
    ] {
        let id =
            daily_series_with_a_reminder(&router, &owner_cookie, &group_id, serde_json::json!({}))
                .await;
        let res = delete_scoped(&router, &owner_cookie, &group_id, &id, &query).await;
        assert_status(&res, StatusCode::NO_CONTENT);
        series.push(id);
    }
    for id in &series {
        let res = call(
            &router,
            Method::GET,
            &format!("/groups/{group_id}/events/{id}"),
            Some(&owner_cookie),
            None,
        )
        .await;
        assert_status(&res, StatusCode::NOT_FOUND);
        assert_eq!(reminder_count(&db, id).await, 0);
    }
    assert!(listed_occurrences(
        &router,
        &owner_cookie,
        &group_id,
        nine_paris(-1),
        nine_paris(10)
    )
    .await
    .is_empty());
}

/// AC (#710): what a scoped delete refuses, and who may send it — the same
/// as a scoped `PATCH` (#708). None of it touches the series.
#[sqlx::test]
async fn a_scoped_delete_is_refused_where_it_has_no_meaning(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "del-refused@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let (member_cookie, _) = private_event_member(
        &router,
        &db,
        &group_id,
        "del-refused-member@example.test",
        "standard",
    )
    .await;
    let series_id =
        daily_series_with_a_reminder(&router, &owner_cookie, &group_id, serde_json::json!({}))
            .await;
    let one_off = create_event_with(
        &router,
        &owner_cookie,
        &group_id,
        serde_json::json!({
            "title": "Dentiste",
            "starts_at": nine_paris(0),
            "ends_at": nine_paris(0) + Duration::hours(1),
        }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let refused = |id: &str, query: String| {
        let (router, cookie, group_id, id) = (
            router.clone(),
            owner_cookie.clone(),
            group_id.clone(),
            id.to_string(),
        );
        async move {
            let res = delete_scoped(&router, &cookie, &group_id, &id, &query).await;
            assert_status(&res, StatusCode::BAD_REQUEST);
            json_body(res).await["error"].as_str().unwrap().to_string()
        }
    };
    for scope in ["this", "this_and_following"] {
        assert_eq!(
            refused(&series_id, format!("?scope={scope}")).await,
            "scope_requires_occurrence"
        );
        assert_eq!(
            refused(&one_off, scoped(scope, nine_paris(0))).await,
            "scope_requires_recurring_event"
        );
        for not_one in [nine_paris(1) + Duration::minutes(5), nine_paris(5)] {
            assert_eq!(
                refused(&series_id, scoped(scope, not_one)).await,
                "occurrence_not_in_series"
            );
        }
    }

    // Same permission as any delete: a plain member may not delete someone
    // else's shared series, one occurrence or more.
    for scope in ["this", "this_and_following"] {
        let res = delete_scoped(
            &router,
            &member_cookie,
            &group_id,
            &series_id,
            &scoped(scope, nine_paris(1)),
        )
        .await;
        assert_status(&res, StatusCode::FORBIDDEN);
    }

    // A series mirrored from an imported calendar is deleted whole or not
    // at all.
    let owner_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE email = 'del-refused@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();
    let mut tx = with_family_scope(&db, &group_id).await;
    sqlx::query(
        "WITH import AS (
             INSERT INTO calendar_imports (group_id, created_by, label, feed_url)
             VALUES ($1::uuid, $2, 'Agenda', '\\x00'::bytea) RETURNING id
         )
         INSERT INTO calendar_import_events (calendar_import_id, event_id, external_uid)
         SELECT id, $3::uuid, 'uid-710' FROM import",
    )
    .bind(&group_id)
    .bind(owner_id)
    .bind(&series_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    for scope in ["this", "this_and_following"] {
        assert_eq!(
            refused(&series_id, scoped(scope, nine_paris(1))).await,
            "scope_not_allowed_for_imported_event"
        );
    }

    // Nothing above changed the series.
    let listed = listed_occurrences(
        &router,
        &owner_cookie,
        &group_id,
        nine_paris(-1),
        nine_paris(10),
    )
    .await;
    assert_eq!(
        listed
            .iter()
            .filter(|o| o.0 == series_id)
            .map(|o| o.2)
            .collect::<Vec<_>>(),
        (0..5).map(nine_paris).collect::<Vec<_>>()
    );
    assert_eq!(
        scheduled_occurrences(&db, &series_id).await,
        (0..5).map(nine_paris).collect::<Vec<_>>()
    );
}
