//! A member's upcoming reminders (#667), `GET /account/reminders/upcoming`,
//! against a real database: what the mobile application schedules on the
//! device. A reminder goes to the event's creator, in the families they
//! belong to now, on a channel that includes notifications.
//!
//! The fixtures are written on the harness pool; the reminders are read by
//! the handlers on their own pool — under `FLOW_TEST_RUNTIME_ROLE` in CI,
//! and, in the test named for it, under a `NOSUPERUSER NOBYPASSRLS` role
//! here too, where `scheduled_notifications` and `events` hide every row
//! without a family in context.

mod common;

use axum::http::{Method, StatusCode};
use common::{
    assert_status, call, drop_prescribed_role, insert_session, json_body, prescribed_role_pool,
    test_router, test_state,
};
use manage_our_home::build_router;
use sqlx::PgPool;
use uuid::Uuid;

const UPCOMING: &str = "/account/reminders/upcoming";

/// An account able to use the API (age declared, CGU accepted), on
/// `channel`, and the cookie of a session of it.
async fn user(db: &PgPool, email: &str, channel: &str) -> (Uuid, String) {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, display_name, email_verified,
                            age_declared_at, terms_accepted_version, terms_accepted_at,
                            reminder_channel)
         VALUES ($1, 'not-a-real-hash', $1, true, now(), $2, now(), $3)
         RETURNING id",
    )
    .bind(email)
    .bind(manage_our_home::auth::terms_acceptance::terms_in_force_now())
    .bind(channel)
    .fetch_one(db)
    .await
    .unwrap();
    (id, insert_session(db, id).await)
}

/// A family owned by `owner`, with `members` in it as well.
async fn family(db: &PgPool, owner: Uuid, members: &[Uuid]) -> Uuid {
    let group: Uuid = sqlx::query_scalar(
        "INSERT INTO groups (name, created_by) VALUES ('Foyer', $1) RETURNING id",
    )
    .bind(owner)
    .fetch_one(db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'owner'::group_role)",
    )
    .bind(group)
    .bind(owner)
    .execute(db)
    .await
    .unwrap();
    for member in members {
        sqlx::query(
            "INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'standard'::group_role)",
        )
        .bind(group)
        .bind(member)
        .execute(db)
        .await
        .unwrap();
    }
    group
}

/// The ids of what [`reminder`] wrote.
struct Fixture {
    event: Uuid,
    reminder: Uuid,
    notification: Uuid,
}

/// An event of `group` created by `by`, with one reminder whose pending
/// notification falls due `fire_in_minutes` from now (negative: past).
async fn reminder(
    db: &PgPool,
    group: Uuid,
    by: Uuid,
    title: &str,
    fire_in_minutes: i32,
) -> Fixture {
    let event: Uuid = sqlx::query_scalar(
        "INSERT INTO events (group_id, created_by, title, starts_at, ends_at)
         VALUES ($1, $2, $3, now() + make_interval(mins => $4 + 10),
                 now() + make_interval(mins => $4 + 70))
         RETURNING id",
    )
    .bind(group)
    .bind(by)
    .bind(title)
    .bind(fire_in_minutes)
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
    let notification = sqlx::query_scalar(
        "INSERT INTO scheduled_notifications (event_reminder_id, event_id, occurrence_at, fire_at)
         SELECT $1, id, starts_at, starts_at - interval '10 minutes' FROM events WHERE id = $2
         RETURNING id",
    )
    .bind(reminder)
    .bind(event)
    .fetch_one(db)
    .await
    .unwrap();
    Fixture {
        event,
        reminder,
        notification,
    }
}

/// The titles `cookie`'s account is to schedule, in the order listed.
async fn upcoming_titles(router: &axum::Router, cookie: &str) -> Vec<String> {
    let res = call(router, Method::GET, UPCOMING, Some(cookie), None).await;
    assert_status(&res, StatusCode::OK);
    json_body(res)
        .await
        .as_array()
        .expect("an array")
        .iter()
        .map(|r| r["title"].as_str().unwrap().to_owned())
        .collect()
}

/// A member of two families receives the reminders of both, soonest
/// first, each entry carrying what the device schedules — read here under
/// a role that does not bypass RLS, so each family's must be read in its
/// own scope.
#[sqlx::test]
async fn a_member_of_two_families_gets_the_reminders_of_both(db: PgPool) {
    let (me, cookie) = user(&db, "me@example.test", "push").await;
    let home = family(&db, me, &[]).await;
    let (other_owner, _) = user(&db, "other@example.test", "push").await;
    let elsewhere = family(&db, other_owner, &[me]).await;
    let later = reminder(&db, home, me, "Dentiste", 120).await;
    let sooner = reminder(&db, elsewhere, me, "Piscine", 30).await;

    let (role, pool) = prescribed_role_pool(&db).await;
    let mut state = test_state(db.clone());
    state.db = pool.clone();
    let router = build_router(state);

    let res = call(&router, Method::GET, UPCOMING, Some(&cookie), None).await;
    assert_status(&res, StatusCode::OK);
    let body = json_body(res).await;
    let listed = body.as_array().unwrap();
    assert_eq!(listed.len(), 2, "{body}");
    assert_eq!(listed[0]["title"], "Piscine");
    assert_eq!(listed[0]["id"], sooner.notification.to_string());
    assert_eq!(listed[0]["event_id"], sooner.event.to_string());
    assert_eq!(listed[0]["group_id"], elsewhere.to_string());
    assert_eq!(listed[0]["all_day"], false);
    assert_eq!(listed[1]["title"], "Dentiste");
    assert_eq!(listed[1]["id"], later.notification.to_string());
    assert_eq!(listed[1]["group_id"], home.to_string());
    // The instants are the row's, as written.
    let (occurrence_at, fire_at): (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("SELECT occurrence_at, fire_at FROM scheduled_notifications WHERE id = $1")
            .bind(later.notification)
            .fetch_one(&db)
            .await
            .unwrap();
    let read = |v: &serde_json::Value| {
        chrono::DateTime::parse_from_rfc3339(v.as_str().unwrap())
            .unwrap()
            .with_timezone(&chrono::Utc)
    };
    assert_eq!(read(&listed[1]["occurrence_at"]), occurrence_at);
    assert_eq!(read(&listed[1]["fire_at"]), fire_at);

    drop(router);
    drop_prescribed_role(&db, pool, &role).await;
}

/// Taken out of a family, a member no longer gets its reminders, though
/// the events stay theirs; those of the family they are still in remain.
#[sqlx::test]
async fn a_member_taken_out_of_a_family_no_longer_gets_its_reminders(db: PgPool) {
    let router = test_router(db.clone());
    let (owner, owner_cookie) = user(&db, "owner@example.test", "push").await;
    let (me, cookie) = user(&db, "me@example.test", "push").await;
    let theirs = family(&db, owner, &[me]).await;
    let mine = family(&db, me, &[]).await;
    reminder(&db, theirs, me, "Chez eux", 30).await;
    reminder(&db, mine, me, "Chez moi", 60).await;
    assert_eq!(
        upcoming_titles(&router, &cookie).await,
        ["Chez eux", "Chez moi"]
    );

    let res = call(
        &router,
        Method::DELETE,
        &format!("/groups/{theirs}/members/{me}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert!(res.status().is_success(), "{}", res.status());

    assert_eq!(upcoming_titles(&router, &cookie).await, ["Chez moi"]);
}

/// A reminder goes to the event's creator alone: another member of the
/// family is not listed it.
#[sqlx::test]
async fn another_member_is_not_listed_the_reminders_of_the_creator(db: PgPool) {
    let router = test_router(db.clone());
    let (creator, creator_cookie) = user(&db, "creator@example.test", "push").await;
    let (other, other_cookie) = user(&db, "other@example.test", "both").await;
    let home = family(&db, creator, &[other]).await;
    reminder(&db, home, creator, "Le mien", 30).await;

    assert_eq!(upcoming_titles(&router, &creator_cookie).await, ["Le mien"]);
    assert!(upcoming_titles(&router, &other_cookie).await.is_empty());
}

/// On the email channel, nothing is listed: the server sends the email,
/// and the device would only repeat it. Switched to notifications, the
/// same reminders are listed.
#[sqlx::test]
async fn the_email_channel_lists_nothing(db: PgPool) {
    let router = test_router(db.clone());
    let (me, cookie) = user(&db, "me@example.test", "email").await;
    let home = family(&db, me, &[]).await;
    reminder(&db, home, me, "Dentiste", 30).await;

    let res = call(&router, Method::GET, UPCOMING, Some(&cookie), None).await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(json_body(res).await, serde_json::json!([]));

    sqlx::query("UPDATE users SET reminder_channel = 'push' WHERE id = $1")
        .bind(me)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(upcoming_titles(&router, &cookie).await, ["Dentiste"]);
}

/// Only what is still to be sent: neither a notification already due nor
/// one sent or failed.
#[sqlx::test]
async fn only_pending_notifications_still_to_come_are_listed(db: PgPool) {
    let router = test_router(db.clone());
    let (me, cookie) = user(&db, "me@example.test", "push").await;
    let home = family(&db, me, &[]).await;
    reminder(&db, home, me, "Passé", -5).await;
    let sent = reminder(&db, home, me, "Envoyé", 30).await;
    let failed = reminder(&db, home, me, "Échoué", 40).await;
    reminder(&db, home, me, "À venir", 50).await;
    for (fixture, status) in [(&sent, "sent"), (&failed, "failed")] {
        sqlx::query("UPDATE scheduled_notifications SET status = $2 WHERE id = $1")
            .bind(fixture.notification)
            .bind(status)
            .execute(&db)
            .await
            .unwrap();
    }

    assert_eq!(upcoming_titles(&router, &cookie).await, ["À venir"]);
}

/// A one-off event's reminder is written whatever its date, but one due
/// beyond the 30 days of the horizon is not listed yet.
#[sqlx::test]
async fn a_reminder_beyond_the_horizon_is_not_listed_yet(db: PgPool) {
    let router = test_router(db.clone());
    let (me, cookie) = user(&db, "me@example.test", "push").await;
    let home = family(&db, me, &[]).await;
    let far = reminder(&db, home, me, "Dans deux mois", 60 * 24 * 60).await;
    reminder(&db, home, me, "Demain", 60 * 24).await;

    let (status,): (String,) =
        sqlx::query_as("SELECT status FROM scheduled_notifications WHERE id = $1")
            .bind(far.notification)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(status, "pending");
    assert_eq!(upcoming_titles(&router, &cookie).await, ["Demain"]);
}

/// A reminder deleted, or its event, leaves the list.
#[sqlx::test]
async fn a_deleted_reminder_or_event_leaves_the_list(db: PgPool) {
    let router = test_router(db.clone());
    let (me, cookie) = user(&db, "me@example.test", "push").await;
    let home = family(&db, me, &[]).await;
    let dropped_reminder = reminder(&db, home, me, "Rappel supprimé", 30).await;
    let dropped_event = reminder(&db, home, me, "Événement supprimé", 40).await;
    reminder(&db, home, me, "Gardé", 50).await;
    assert_eq!(
        upcoming_titles(&router, &cookie).await,
        ["Rappel supprimé", "Événement supprimé", "Gardé"]
    );

    let res = call(
        &router,
        Method::DELETE,
        &format!(
            "/groups/{home}/events/{}/reminders/{}",
            dropped_reminder.event, dropped_reminder.reminder
        ),
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    let res = call(
        &router,
        Method::DELETE,
        &format!("/groups/{home}/events/{}", dropped_event.event),
        Some(&cookie),
        None,
    )
    .await;
    assert!(res.status().is_success(), "{}", res.status());

    assert_eq!(upcoming_titles(&router, &cookie).await, ["Gardé"]);
}

/// Without a session, or with one of an account deactivated or deleted
/// since, 401, as every other `/account` route.
#[sqlx::test]
async fn without_an_active_account_the_answer_is_401(db: PgPool) {
    let router = test_router(db.clone());
    let res = call(&router, Method::GET, UPCOMING, None, None).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);

    for column in ["deactivated_at", "deleted_at"] {
        let (me, cookie) = user(&db, &format!("{column}@example.test"), "push").await;
        let home = family(&db, me, &[]).await;
        reminder(&db, home, me, "Dentiste", 30).await;
        assert_eq!(upcoming_titles(&router, &cookie).await, ["Dentiste"]);

        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE users SET {column} = now() WHERE id = $1"
        )))
        .bind(me)
        .execute(&db)
        .await
        .unwrap();
        let res = call(&router, Method::GET, UPCOMING, Some(&cookie), None).await;
        assert_status(&res, StatusCode::UNAUTHORIZED);
    }
}
