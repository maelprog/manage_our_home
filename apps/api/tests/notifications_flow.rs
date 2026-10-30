//! The notification centre (#534), against a real database: the events
//! that write a notification (a reminder coming due, a stock item nearing
//! its expiry, a message, an invitation, a grocery item), the member's API
//! (`/notifications`), its isolation per recipient, the 30-day purge and
//! the export.
//!
//! The reminder pass is driven with recording senders, as in
//! `push_reminders_flow`: no email leaves and no push service is called.

mod common;

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use common::{assert_status, call, json_body, set_cookie, test_router};
use manage_our_home::jobs::retention_purge::{purge, RetentionCutoffs};
use manage_our_home::jobs::scheduled_notifications::send_due_notifications;
use manage_our_home::jobs::stock_expiry::notify_expiring;
use manage_our_home::notifications::push::PushOutcome;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

async fn register_verify_login(router: &axum::Router, db: &PgPool, email: &str) -> String {
    let password = "notif-flow-password-1";
    call(
        router,
        Method::POST,
        "/auth/register",
        None,
        Some(json!({"email": email, "password": password, "display_name": email, "declares_minimum_age": true, "accepts_terms": true})),
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
        Some(json!({"email": email, "password": password})),
    )
    .await;
    set_cookie(&login).unwrap()
}

async fn user_id(db: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn create_group(router: &axum::Router, cookie: &str, name: &str) -> String {
    let res = call(
        router,
        Method::POST,
        "/groups",
        Some(cookie),
        Some(json!({"name": name})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

/// Invites without an address (no notification) and accepts.
async fn invite_and_join(router: &axum::Router, group: &str, owner: &str, member: &str) {
    let invite = call(
        router,
        Method::POST,
        &format!("/groups/{group}/invitations"),
        Some(owner),
        Some(json!({})),
    )
    .await;
    assert_status(&invite, StatusCode::CREATED);
    let token = json_body(invite).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let accept = call(
        router,
        Method::POST,
        &format!("/groups/invitations/{token}/accept"),
        Some(member),
        None,
    )
    .await;
    assert_status(&accept, StatusCode::OK);
}

/// An owner and a member of one family, both signed in.
struct Family {
    router: axum::Router,
    group: String,
    owner: String,
    member: String,
}

async fn family(db: &PgPool) -> Family {
    let router = test_router(db.clone());
    let owner = register_verify_login(&router, db, "owner@example.test").await;
    let member = register_verify_login(&router, db, "member@example.test").await;
    let group = create_group(&router, &owner, "Maison").await;
    invite_and_join(&router, &group, &owner, &member).await;
    Family {
        router,
        group,
        owner,
        member,
    }
}

async fn list(router: &axum::Router, cookie: &str, query: &str) -> Value {
    let res = call(
        router,
        Method::GET,
        &format!("/notifications{query}"),
        Some(cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    json_body(res).await
}

async fn unread(router: &axum::Router, cookie: &str) -> i64 {
    let res = call(
        router,
        Method::GET,
        "/notifications/unread_count",
        Some(cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    json_body(res).await["unread_count"].as_i64().unwrap()
}

fn kinds_and_bodies(page: &Value) -> Vec<(String, String)> {
    page["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            (
                n["kind"].as_str().unwrap().to_string(),
                n["body"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

// -- the reminder ---------------------------------------------------------------

/// A group owned by `by`, one event of theirs at `starts_at`, and its
/// reminder's notification due a minute ago. Returns the group, the event
/// and the scheduled notification.
async fn due_reminder(db: &PgPool, by: Uuid, starts_at: DateTime<Utc>) -> (Uuid, Uuid, Uuid) {
    let group: Uuid = sqlx::query_scalar(
        "INSERT INTO groups (name, created_by) VALUES ('Maison', $1) RETURNING id",
    )
    .bind(by)
    .fetch_one(db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'owner')")
        .bind(group)
        .bind(by)
        .execute(db)
        .await
        .unwrap();
    let event: Uuid = sqlx::query_scalar(
        "INSERT INTO events (group_id, created_by, title, starts_at, ends_at)
         VALUES ($1, $2, 'IRM', $3, $3 + interval '1 hour')
         RETURNING id",
    )
    .bind(group)
    .bind(by)
    .bind(starts_at)
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
    let scheduled = sqlx::query_scalar(
        "INSERT INTO scheduled_notifications (event_reminder_id, event_id, occurrence_at, fire_at)
         VALUES ($1, $2, $3, now() - interval '1 minute')
         RETURNING id",
    )
    .bind(reminder)
    .bind(event)
    .bind(starts_at)
    .fetch_one(db)
    .await
    .unwrap();
    (group, event, scheduled)
}

/// One reminder pass; every email answers `email_ok`, every push is
/// delivered.
async fn pass(db: &PgPool, email_ok: bool) {
    let send = move |_to: String, _subject: String, _body: String| async move {
        if email_ok {
            Ok(())
        } else {
            Err(anyhow::anyhow!("smtp down"))
        }
    };
    let send_push = |_endpoint: String, _ttl: i64| async { PushOutcome::Delivered };
    send_due_notifications(db, send, send_push).await.unwrap();
}

/// The acceptance criterion: a member on notifications with no device
/// subscribed gets nothing pushed (#306), and their centre still holds the
/// reminder, unread, naming the event and its Paris hour.
#[sqlx::test]
async fn a_due_reminder_notifies_its_recipient_even_without_any_device(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "rappel@example.test").await;
    let me = user_id(&db, "rappel@example.test").await;
    // 14:30 UTC on 3 October 2030 is 16:30 in Paris.
    let starts_at = "2030-10-03T14:30:00Z".parse().unwrap();
    let (group, event, scheduled) = due_reminder(&db, me, starts_at).await;

    pass(&db, true).await;

    let status: String =
        sqlx::query_scalar("SELECT status FROM scheduled_notifications WHERE id = $1")
            .bind(scheduled)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(status, "failed", "no device: retired, nothing pushed");
    let page = list(&router, &cookie, "").await;
    assert_eq!(
        kinds_and_bodies(&page),
        [(
            "agenda_reminder".to_string(),
            "Rappel : « IRM » le 03/10/2030 à 16:30.".to_string()
        )]
    );
    let entry = &page["notifications"][0];
    assert_eq!(entry["group_id"], group.to_string());
    assert_eq!(entry["object_id"], event.to_string());
    assert!(entry["read_at"].is_null());
    assert_eq!(unread(&router, &cookie).await, 1);
}

/// A reminder whose email failed is taken again on the next pass: its
/// notification is written once, not once per attempt.
#[sqlx::test]
async fn a_retried_reminder_is_notified_once(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "retry@example.test").await;
    let me = user_id(&db, "retry@example.test").await;
    sqlx::query("UPDATE users SET reminder_channel = 'email' WHERE id = $1")
        .bind(me)
        .execute(&db)
        .await
        .unwrap();
    let (_, _, scheduled) = due_reminder(&db, me, Utc::now() + Duration::minutes(9)).await;

    pass(&db, false).await;
    pass(&db, false).await;

    let (status, attempts): (String, i32) =
        sqlx::query_as("SELECT status, attempts FROM scheduled_notifications WHERE id = $1")
            .bind(scheduled)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!((status.as_str(), attempts), ("pending", 2));
    assert_eq!(unread(&router, &cookie).await, 1);
}

// -- the events of a family -----------------------------------------------------

/// The other members are told who wrote, never what: the content is
/// encrypted at rest and the notification is not. The author is not told.
#[sqlx::test]
async fn a_message_notifies_the_other_members_without_its_content(db: PgPool) {
    let f = family(&db).await;
    let res = call(
        &f.router,
        Method::POST,
        &format!("/groups/{}/messages", f.group),
        Some(&f.member),
        Some(json!({"content": "Le code de la porte est 4521"})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let message = json_body(res).await["id"].as_str().unwrap().to_string();

    let page = list(&f.router, &f.owner, "").await;
    assert_eq!(
        kinds_and_bodies(&page),
        [(
            "message".to_string(),
            "Nouveau message de member@example.test.".to_string()
        )]
    );
    assert_eq!(page["notifications"][0]["object_id"], message);
    assert_eq!(page["notifications"][0]["group_id"], f.group);
    assert_eq!(unread(&f.router, &f.member).await, 0);
}

#[sqlx::test]
async fn a_grocery_item_notifies_the_other_members(db: PgPool) {
    let f = family(&db).await;
    let res = call(
        &f.router,
        Method::POST,
        &format!("/groups/{}/grocery-items", f.group),
        Some(&f.owner),
        Some(json!({"name": "Lait"})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);

    assert_eq!(
        kinds_and_bodies(&list(&f.router, &f.member, "").await),
        [(
            "grocery_item".to_string(),
            "owner@example.test a ajouté « Lait » à la liste de courses.".to_string()
        )]
    );
    assert_eq!(unread(&f.router, &f.owner).await, 0);
}

/// An invitation to an address that holds an account shows in that
/// account's centre, from a family it is not a member of; one to an
/// address without an account writes nothing.
#[sqlx::test]
async fn an_invitation_notifies_an_existing_account(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(&router, &db, "inviter@example.test").await;
    let invitee = register_verify_login(&router, &db, "invitee@example.test").await;
    let group = create_group(&router, &owner, "Famille Martin").await;

    for email in ["Invitee@Example.test", "nobody@example.test"] {
        let res = call(
            &router,
            Method::POST,
            &format!("/groups/{group}/invitations"),
            Some(&owner),
            Some(json!({"invited_email": email})),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
    }

    let page = list(&router, &invitee, "").await;
    assert_eq!(
        kinds_and_bodies(&page),
        [(
            "invitation".to_string(),
            "inviter@example.test vous invite à rejoindre « Famille Martin ».".to_string()
        )]
    );
    assert_eq!(page["notifications"][0]["group_id"], group);
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM notifications")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(total, 1);
}

async fn stock_item(db: &PgPool, group: &str, name: &str, qty: f64, expires: NaiveDate) -> Uuid {
    let owner = user_id(db, "owner@example.test").await;
    sqlx::query_scalar(
        "INSERT INTO stock_items (group_id, created_by, name, quantity, expires_on)
         VALUES ($1::uuid, $2, $3, $4, $5) RETURNING id",
    )
    .bind(group)
    .bind(owner)
    .bind(name)
    .bind(qty)
    .bind(expires)
    .fetch_one(db)
    .await
    .unwrap()
}

/// Every member is told once per item and date, within the « bientôt »
/// window only; a date moved is told again.
#[sqlx::test]
async fn an_expiring_item_notifies_every_member_once(db: PgPool) {
    let f = family(&db).await;
    let today = NaiveDate::from_ymd_opt(2030, 10, 30).unwrap();
    let day = |n: i64| today + Duration::days(n);
    let lait = stock_item(&db, &f.group, "Lait", 1.0, day(3)).await;
    stock_item(&db, &f.group, "Riz", 1.0, day(4)).await;
    stock_item(&db, &f.group, "Yaourt", 1.0, day(-1)).await;
    stock_item(&db, &f.group, "Beurre", 0.0, day(1)).await;

    assert_eq!(notify_expiring(&db, today).await.unwrap(), 2);
    assert_eq!(notify_expiring(&db, today).await.unwrap(), 0);
    for cookie in [&f.owner, &f.member] {
        assert_eq!(
            kinds_and_bodies(&list(&f.router, cookie, "").await),
            [(
                "stock_expiry".to_string(),
                "« Lait » est à consommer jusqu'au 02/11/2030.".to_string()
            )]
        );
    }

    sqlx::query("UPDATE stock_items SET expires_on = $2 WHERE id = $1")
        .bind(lait)
        .bind(day(2))
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(notify_expiring(&db, today).await.unwrap(), 2);
}

/// The window's first day is today: an item to eat today is told.
#[sqlx::test]
async fn an_item_expiring_today_is_notified(db: PgPool) {
    let f = family(&db).await;
    let today = NaiveDate::from_ymd_opt(2030, 10, 30).unwrap();
    stock_item(&db, &f.group, "Crème", 1.0, today).await;

    assert_eq!(notify_expiring(&db, today).await.unwrap(), 2);
    assert_eq!(
        kinds_and_bodies(&list(&f.router, &f.member, "").await),
        [(
            "stock_expiry".to_string(),
            "« Crème » est à consommer jusqu'au 30/10/2030.".to_string()
        )]
    );
}

/// A deactivated account and a purged one are members still, and told
/// nothing: neither can read its centre.
#[sqlx::test]
async fn a_deactivated_or_purged_member_is_not_notified(db: PgPool) {
    let f = family(&db).await;
    let third = register_verify_login(&f.router, &db, "third@example.test").await;
    invite_and_join(&f.router, &f.group, &f.owner, &third).await;
    let member = user_id(&db, "member@example.test").await;
    let third = user_id(&db, "third@example.test").await;
    sqlx::query("UPDATE users SET deactivated_at = now() WHERE id = $1")
        .bind(member)
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET deleted_at = now() WHERE id = $1")
        .bind(third)
        .execute(&db)
        .await
        .unwrap();

    let res = call(
        &f.router,
        Method::POST,
        &format!("/groups/{}/grocery-items", f.group),
        Some(&f.owner),
        Some(json!({"name": "Lait"})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);

    let notified: Vec<Uuid> = sqlx::query_scalar("SELECT user_id FROM notifications")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(notified, Vec::<Uuid>::new());
}

/// An inviter typing their own address is not told of their own
/// invitation.
#[sqlx::test]
async fn inviting_ones_own_address_notifies_no_one(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(&router, &db, "self@example.test").await;
    let group = create_group(&router, &owner, "Maison").await;

    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{group}/invitations"),
        Some(&owner),
        Some(json!({"invited_email": "self@example.test"})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);

    assert_eq!(list(&router, &owner, "").await["notifications"], json!([]));
}

// -- the API ------------------------------------------------------------------

async fn insert(db: &PgPool, user: Uuid, group: &str, kind: &str, minutes_ago: i64) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO notifications (user_id, group_id, kind, body, created_at)
         VALUES ($1, $2::uuid, $3, 'Texte.', now() - make_interval(mins => $4::int))
         RETURNING id",
    )
    .bind(user)
    .bind(group)
    .bind(kind)
    .bind(minutes_ago as i32)
    .fetch_one(db)
    .await
    .unwrap()
}

/// The recipient policy, through the API: another member's notification
/// is neither listed nor counted, and marking it read answers 404 as for
/// an id that does not exist, leaving it unread.
#[sqlx::test]
async fn a_member_neither_sees_nor_marks_another_ones(db: PgPool) {
    let f = family(&db).await;
    let owner = user_id(&db, "owner@example.test").await;
    let theirs = insert(&db, owner, &f.group, "message", 1).await;

    assert_eq!(
        list(&f.router, &f.member, "").await["notifications"],
        json!([])
    );
    assert_eq!(unread(&f.router, &f.member).await, 0);
    for id in [theirs, Uuid::new_v4()] {
        let res = call(
            &f.router,
            Method::POST,
            &format!("/notifications/{id}/read"),
            Some(&f.member),
            None,
        )
        .await;
        assert_status(&res, StatusCode::NOT_FOUND);
    }
    let res = call(
        &f.router,
        Method::POST,
        "/notifications/read",
        Some(&f.member),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(unread(&f.router, &f.owner).await, 1);
}

#[sqlx::test]
async fn one_or_all_can_be_marked_read(db: PgPool) {
    let f = family(&db).await;
    let owner = user_id(&db, "owner@example.test").await;
    let first = insert(&db, owner, &f.group, "message", 3).await;
    insert(&db, owner, &f.group, "message", 2).await;
    insert(&db, owner, &f.group, "grocery_item", 1).await;
    assert_eq!(unread(&f.router, &f.owner).await, 3);

    let read_at_of = |id: Uuid| {
        let db = db.clone();
        async move {
            sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
                "SELECT read_at FROM notifications WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };
    // Marking it read again keeps the date of the first time.
    let mut dates = Vec::new();
    for _ in 0..2 {
        let res = call(
            &f.router,
            Method::POST,
            &format!("/notifications/{first}/read"),
            Some(&f.owner),
            None,
        )
        .await;
        assert_status(&res, StatusCode::NO_CONTENT);
        dates.push(read_at_of(first).await);
    }
    assert_eq!(unread(&f.router, &f.owner).await, 2);
    let read_at = dates[0];
    assert!(read_at.is_some());
    assert_eq!(dates[1], read_at);

    let res = call(
        &f.router,
        Method::POST,
        "/notifications/read",
        Some(&f.owner),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(unread(&f.router, &f.owner).await, 0);
    // Marking all read again keeps the first date of each.
    assert_eq!(read_at_of(first).await, read_at);
}

fn ids(page: &Value) -> Vec<Uuid> {
    page["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap().parse().unwrap())
        .collect()
}

/// Newest first, by pages on the `(created_at, id)` cursor, one kind or all.
#[sqlx::test]
async fn the_list_pages_newest_first_and_filters_by_kind(db: PgPool) {
    let f = family(&db).await;
    let owner = user_id(&db, "owner@example.test").await;
    let mut made = Vec::new();
    for (minutes_ago, kind) in [
        (5, "message"),
        (4, "grocery_item"),
        (3, "message"),
        (2, "stock_expiry"),
        (1, "message"),
    ] {
        made.push(insert(&db, owner, &f.group, kind, minutes_ago).await);
    }
    made.reverse();

    let first = list(&f.router, &f.owner, "?limit=2").await;
    assert_eq!(ids(&first), made[..2]);
    assert_eq!(first["has_more"], true);
    let last = &first["notifications"][1];
    let cursor = format!(
        "?limit=2&before_created_at={}&before_id={}",
        urlencoding(last["created_at"].as_str().unwrap()),
        last["id"].as_str().unwrap()
    );
    let second = list(&f.router, &f.owner, &cursor).await;
    assert_eq!(ids(&second), made[2..4]);
    let all = list(&f.router, &f.owner, "?limit=100").await;
    assert_eq!(ids(&all), made);
    assert_eq!(all["has_more"], false);

    let messages = list(&f.router, &f.owner, "?kind=message").await;
    assert_eq!(ids(&messages), [made[0], made[2], made[4]]);

    for bad in ["?kind=budget", "?kind=", &format!("?before_id={}", made[0])] {
        let res = call(
            &f.router,
            Method::GET,
            &format!("/notifications{bad}"),
            Some(&f.owner),
            None,
        )
        .await;
        assert_status(&res, StatusCode::BAD_REQUEST);
    }
}

/// `+` in an RFC 3339 offset would read as a space in a query string.
fn urlencoding(value: &str) -> String {
    value.replace('+', "%2B").replace(':', "%3A")
}

#[sqlx::test]
async fn the_centre_needs_a_session(db: PgPool) {
    let router = test_router(db);
    for (method, uri) in [
        (Method::GET, "/notifications"),
        (Method::GET, "/notifications/unread_count"),
        (Method::POST, "/notifications/read"),
        (
            Method::POST,
            &format!("/notifications/{}/read", Uuid::new_v4()),
        ),
    ] {
        let res = call(&router, method, uri, None, None).await;
        assert_status(&res, StatusCode::UNAUTHORIZED);
    }
}

// -- retention and export -------------------------------------------------------

/// The 30 days of the controller's decision of 2026-10-21, against an
/// injected clock: a minute past them goes, read or not; a minute short
/// stays, read or not.
#[sqlx::test]
async fn the_purge_drops_notifications_past_30_days_on_an_injected_date(db: PgPool) {
    let f = family(&db).await;
    let owner = user_id(&db, "owner@example.test").await;
    let injected: DateTime<Utc> = "2031-03-15T08:00:00Z".parse().unwrap();
    let edge = injected - Duration::days(30);
    let mut kept = Vec::new();
    for (created, read, keep) in [
        (edge - Duration::minutes(1), false, false),
        (edge - Duration::minutes(1), true, false),
        (edge + Duration::minutes(1), false, true),
        (edge + Duration::minutes(1), true, true),
    ] {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO notifications (user_id, group_id, kind, body, created_at, read_at)
             VALUES ($1, $2::uuid, 'message', 'Texte.', $3, CASE WHEN $4 THEN $3 END)
             RETURNING id",
        )
        .bind(owner)
        .bind(&f.group)
        .bind(created)
        .bind(read)
        .fetch_one(&db)
        .await
        .unwrap();
        if keep {
            kept.push(id);
        }
    }

    let counts = purge(&db, RetentionCutoffs::at(injected)).await.unwrap();

    assert_eq!(counts.notifications, 2);
    let mut left: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM notifications")
        .fetch_all(&db)
        .await
        .unwrap();
    left.sort();
    kept.sort();
    assert_eq!(left, kept);
}

/// The export holds the caller's notifications, from every family — an
/// invitation's included — and no one else's.
#[sqlx::test]
async fn the_export_holds_the_callers_notifications_only(db: PgPool) {
    let f = family(&db).await;
    let owner = user_id(&db, "owner@example.test").await;
    let member = user_id(&db, "member@example.test").await;
    let mine = insert(&db, member, &f.group, "message", 2).await;
    insert(&db, owner, &f.group, "message", 1).await;

    let res = call(
        &f.router,
        Method::GET,
        "/account/export",
        Some(&f.member),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let doc = json_body(res).await;
    let exported = doc["notifications"].as_array().unwrap();
    assert_eq!(exported.len(), 1, "{exported:?}");
    assert_eq!(exported[0]["kind"], "message");
    assert_eq!(exported[0]["body"], "Texte.");
    assert_eq!(exported[0]["group_id"], f.group);
    let stored: Uuid = sqlx::query_scalar("SELECT id FROM notifications WHERE user_id = $1")
        .bind(member)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(stored, mine);
}
