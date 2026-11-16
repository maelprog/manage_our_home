//! The notification centre (#534), against a real database: the events
//! that write a notification (a reminder coming due, a stock item nearing
//! its expiry and the daily recap of those items, a message, an invitation,
//! a grocery item, an agenda event added, changed or deleted, a calendar
//! import), the member's API
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
use manage_our_home::jobs::stock_expiry_recap::{push_recap, recap, RECAP_PUSH_TTL_SECS};
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
    let send = move |_to: String, _subject: String, _body: manage_our_home::email::EmailContent| async move {
        if email_ok {
            Ok(())
        } else {
            Err(anyhow::anyhow!("smtp down"))
        }
    };
    let send_push = |_endpoint: String, _ttl: i64| async { PushOutcome::Delivered };
    send_due_notifications(db, "https://maison.example", send, send_push)
        .await
        .unwrap();
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

// -- the daily expiry recap (#566) ---------------------------------------------

fn recap_bodies(page: &Value) -> Vec<String> {
    kinds_and_bodies(page)
        .into_iter()
        .map(|(kind, body)| {
            assert_eq!(kind, "stock_expiry");
            body
        })
        .collect()
}

/// One recap per member, of the rows of quantity > 0 within the « bientôt »
/// window, expired ones included, the soonest first; a second pass the same
/// day writes nothing.
#[sqlx::test]
async fn the_recap_notifies_every_member_once_a_day(db: PgPool) {
    let f = family(&db).await;
    let today = NaiveDate::from_ymd_opt(2030, 10, 30).unwrap();
    let day = |n: i64| today + Duration::days(n);
    stock_item(&db, &f.group, "Lait", 1.0, day(3)).await;
    stock_item(&db, &f.group, "Yaourt", 2.0, day(-1)).await;
    stock_item(&db, &f.group, "Riz", 1.0, day(4)).await;
    stock_item(&db, &f.group, "Beurre", 0.0, day(1)).await;

    let first = recap(&db, today, 9).await.unwrap();
    assert_eq!(first.written, 2);
    let mut expected = vec![
        user_id(&db, "owner@example.test").await,
        user_id(&db, "member@example.test").await,
    ];
    expected.sort_unstable();
    assert_eq!(first.recipients, expected);
    for cookie in [&f.owner, &f.member] {
        assert_eq!(
            recap_bodies(&list(&f.router, cookie, "").await),
            ["2 produits périment bientôt : Yaourt, Lait."]
        );
    }

    let second = recap(&db, today, 9).await.unwrap();
    assert_eq!((second.written, second.recipients.len()), (0, 0));
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM notifications")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(total, 2);
}

/// A row announced, its date moved to another day close by, is announced
/// again — alone, and once: the day after, nothing goes; a row of quantity 0
/// never is, whatever its date.
#[sqlx::test]
async fn the_recap_announces_a_moved_date_again_and_never_an_empty_row(db: PgPool) {
    let f = family(&db).await;
    let today = NaiveDate::from_ymd_opt(2030, 10, 30).unwrap();
    let day = |n: i64| today + Duration::days(n);
    let lait = stock_item(&db, &f.group, "Lait", 1.0, day(2)).await;
    stock_item(&db, &f.group, "Œufs", 6.0, day(1)).await;
    let beurre = stock_item(&db, &f.group, "Beurre", 0.0, day(1)).await;
    assert_eq!(recap(&db, today, 9).await.unwrap().written, 2);

    for (id, n) in [(lait, 3), (beurre, 0)] {
        sqlx::query("UPDATE stock_items SET expires_on = $2 WHERE id = $1")
            .bind(id)
            .bind(day(n))
            .execute(&db)
            .await
            .unwrap();
    }
    let next = day(1);
    assert_eq!(recap(&db, next, 9).await.unwrap().written, 2);
    assert_eq!(
        recap_bodies(&list(&f.router, &f.member, "").await),
        [
            "1 produit périme bientôt : Lait.",
            "2 produits périment bientôt : Œufs, Lait."
        ]
    );
    // The moved date is the one now marked announced: the next pass, with
    // Lait still close by, has nothing new to say.
    let after = recap(&db, day(2), 9).await.unwrap();
    assert_eq!((after.written, after.recipients.len()), (0, 0));
    let announced: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM stock_expiry_announcements WHERE stock_item_id = $1",
    )
    .bind(beurre)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(announced, 0);
}

// -- the recap's hour, per member (#700) -----------------------------------

async fn set_digest_hour(db: &PgPool, email: &str, hour: i16) {
    sqlx::query("UPDATE users SET expiry_digest_hour = $2 WHERE email = $1")
        .bind(email)
        .bind(hour)
        .execute(db)
        .await
        .unwrap();
}

/// Two members of one family at two hours, 7 and 20: each gets the recap
/// at theirs, once that day — a row added in between goes to the second
/// that day and to the first the next day, alone.
#[sqlx::test]
async fn two_members_get_the_recap_at_their_own_hours_once_a_day(db: PgPool) {
    let f = family(&db).await;
    set_digest_hour(&db, "owner@example.test", 7).await;
    set_digest_hour(&db, "member@example.test", 20).await;
    let owner = user_id(&db, "owner@example.test").await;
    let member = user_id(&db, "member@example.test").await;
    let today = NaiveDate::from_ymd_opt(2030, 10, 30).unwrap();
    let day = |n: i64| today + Duration::days(n);
    stock_item(&db, &f.group, "Lait", 1.0, day(1)).await;

    for hour in 0..7 {
        let done = recap(&db, today, hour).await.unwrap();
        assert_eq!((done.written, done.recipients.len()), (0, 0), "{hour} h");
    }
    let at_seven = recap(&db, today, 7).await.unwrap();
    assert_eq!((at_seven.written, at_seven.recipients), (1, vec![owner]));

    stock_item(&db, &f.group, "Riz", 1.0, day(2)).await;
    for hour in 8..20 {
        let done = recap(&db, today, hour).await.unwrap();
        assert_eq!((done.written, done.recipients.len()), (0, 0), "{hour} h");
    }
    let at_eight_pm = recap(&db, today, 20).await.unwrap();
    assert_eq!(
        (at_eight_pm.written, at_eight_pm.recipients),
        (1, vec![member])
    );
    for hour in 21..24 {
        let done = recap(&db, today, hour).await.unwrap();
        assert_eq!((done.written, done.recipients.len()), (0, 0), "{hour} h");
    }

    assert_eq!(
        recap_bodies(&list(&f.router, &f.owner, "").await),
        ["1 produit périme bientôt : Lait."]
    );
    assert_eq!(
        recap_bodies(&list(&f.router, &f.member, "").await),
        ["2 produits périment bientôt : Lait, Riz."]
    );

    // The next day: Riz, which the owner has not been told of yet.
    let next = recap(&db, day(1), 7).await.unwrap();
    assert_eq!((next.written, next.recipients), (1, vec![owner]));
    assert_eq!(
        recap_bodies(&list(&f.router, &f.owner, "").await)[0],
        "1 produit périme bientôt : Riz."
    );
    // And the member, at 20, nothing new: no notification, the day marked.
    assert_eq!(recap(&db, day(1), 20).await.unwrap().written, 0);
    let sent_on: Option<NaiveDate> =
        sqlx::query_scalar("SELECT expiry_digest_sent_on FROM users WHERE id = $1")
            .bind(member)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(sent_on, Some(day(1)));
}

/// A pass missed at the member's hour sends at the next one, that day, and
/// once: the day of the last pass is in the database, so a restarted
/// process, or a second one, does not send it again.
#[sqlx::test]
async fn a_missed_hour_sends_once_at_the_next_pass(db: PgPool) {
    let f = family(&db).await;
    set_digest_hour(&db, "owner@example.test", 9).await;
    set_digest_hour(&db, "member@example.test", 9).await;
    let today = NaiveDate::from_ymd_opt(2030, 10, 30).unwrap();
    stock_item(&db, &f.group, "Lait", 1.0, today).await;

    assert_eq!(recap(&db, today, 14).await.unwrap().written, 2);
    stock_item(&db, &f.group, "Œufs", 1.0, today).await;
    assert_eq!(recap(&db, today, 14).await.unwrap().written, 0);
    assert_eq!(recap(&db, today, 23).await.unwrap().written, 0);
}

/// The hour is read and written by `/account/notifications`: 9 by default,
/// 0 to 23, anything else refused and the hour left as it was; a request
/// without it leaves it too.
#[sqlx::test]
async fn the_recap_hour_is_set_from_0_to_23_and_9_by_default(db: PgPool) {
    let f = family(&db).await;
    let settings = |cookie: String| {
        let router = f.router.clone();
        async move {
            let res = call(
                &router,
                Method::GET,
                "/account/notifications",
                Some(&cookie),
                None,
            )
            .await;
            assert_status(&res, StatusCode::OK);
            json_body(res).await
        }
    };
    let put = |body: Value| {
        let (router, cookie) = (f.router.clone(), f.owner.clone());
        async move {
            call(
                &router,
                Method::PUT,
                "/account/notifications",
                Some(&cookie),
                Some(body),
            )
            .await
        }
    };
    assert_eq!(settings(f.owner.clone()).await["expiry_digest_hour"], 9);

    for hour in [24, -1, 100] {
        let res = put(json!({"reminder_channel": "push", "expiry_digest_hour": hour})).await;
        assert_status(&res, StatusCode::BAD_REQUEST);
        assert_eq!(
            json_body(res).await["error"],
            "invalid_expiry_digest_hour",
            "{hour}"
        );
    }
    assert_eq!(settings(f.owner.clone()).await["expiry_digest_hour"], 9);

    for hour in [0, 23, 7] {
        let res = put(json!({"reminder_channel": "push", "expiry_digest_hour": hour})).await;
        assert_status(&res, StatusCode::NO_CONTENT);
        assert_eq!(settings(f.owner.clone()).await["expiry_digest_hour"], hour);
    }
    let res = put(json!({"reminder_channel": "email"})).await;
    assert_status(&res, StatusCode::NO_CONTENT);
    let s = settings(f.owner.clone()).await;
    assert_eq!(
        (
            s["reminder_channel"].clone(),
            s["expiry_digest_hour"].clone()
        ),
        (json!("email"), json!(7))
    );
    // The other member's hour is theirs.
    assert_eq!(settings(f.member.clone()).await["expiry_digest_hour"], 9);
}

/// The hourly pass skips a member a recap told of the row for its date,
/// and still tells the member whose recap has not come yet.
#[sqlx::test]
async fn the_hourly_pass_skips_only_the_members_the_recap_told(db: PgPool) {
    let f = family(&db).await;
    set_digest_hour(&db, "member@example.test", 20).await;
    let today = NaiveDate::from_ymd_opt(2030, 10, 30).unwrap();
    stock_item(&db, &f.group, "Lait", 1.0, today + Duration::days(2)).await;
    assert_eq!(recap(&db, today, 9).await.unwrap().written, 1);
    assert_eq!(notify_expiring(&db, today).await.unwrap(), 1);
    assert_eq!(
        kinds_and_bodies(&list(&f.router, &f.member, "").await),
        [(
            "stock_expiry".to_string(),
            "« Lait » est à consommer jusqu'au 01/11/2030.".to_string()
        )]
    );
}

/// Each family's recap names its own rows only, to its own members: the
/// members of another family read nothing of this one's stock.
#[sqlx::test]
async fn the_recap_stays_within_the_family(db: PgPool) {
    let f = family(&db).await;
    let other = register_verify_login(&f.router, &db, "other@example.test").await;
    let other_group = create_group(&f.router, &other, "Ailleurs").await;
    let today = NaiveDate::from_ymd_opt(2030, 10, 30).unwrap();
    stock_item(&db, &f.group, "Lait", 1.0, today).await;
    stock_item(&db, &other_group, "Riz", 1.0, today + Duration::days(1)).await;

    let done = recap(&db, today, 9).await.unwrap();
    assert_eq!(done.written, 3);
    for cookie in [&f.owner, &f.member] {
        assert_eq!(
            recap_bodies(&list(&f.router, cookie, "").await),
            ["1 produit périme bientôt : Lait."]
        );
    }
    let page = list(&f.router, &other, "").await;
    assert_eq!(recap_bodies(&page), ["1 produit périme bientôt : Riz."]);
    assert_eq!(page["notifications"][0]["group_id"], other_group);
}

/// A deactivated account and a purged one are members still, and get no
/// recap: neither can read its centre.
#[sqlx::test]
async fn the_recap_skips_a_deactivated_or_purged_member(db: PgPool) {
    let f = family(&db).await;
    let third = register_verify_login(&f.router, &db, "third@example.test").await;
    invite_and_join(&f.router, &f.group, &f.owner, &third).await;
    let owner = user_id(&db, "owner@example.test").await;
    let member = user_id(&db, "member@example.test").await;
    let third = user_id(&db, "third@example.test").await;
    for (id, sql) in [
        (
            member,
            "UPDATE users SET deactivated_at = now() WHERE id = $1",
        ),
        (third, "UPDATE users SET deleted_at = now() WHERE id = $1"),
    ] {
        sqlx::query(sql).bind(id).execute(&db).await.unwrap();
    }
    let today = NaiveDate::from_ymd_opt(2030, 10, 30).unwrap();
    stock_item(&db, &f.group, "Lait", 1.0, today).await;

    let done = recap(&db, today, 9).await.unwrap();
    assert_eq!((done.written, done.recipients), (1, vec![owner]));
    let notified: Vec<Uuid> = sqlx::query_scalar("SELECT user_id FROM notifications")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(notified, [owner]);
}

/// The recap is pushed on the member's reminder channel, as the budget
/// alerts are: a member on email only gets it in the centre, devices or
/// not; `push` and `both` get it pushed.
#[sqlx::test]
async fn the_recap_push_follows_each_member_s_channel(db: PgPool) {
    let f = family(&db).await;
    let third = register_verify_login(&f.router, &db, "third@example.test").await;
    invite_and_join(&f.router, &f.group, &f.owner, &third).await;
    for (email, channel) in [
        ("owner@example.test", "push"),
        ("member@example.test", "email"),
        ("third@example.test", "both"),
    ] {
        let id: Uuid = sqlx::query_scalar(
            "UPDATE users SET reminder_channel = $2 WHERE email = $1 RETURNING id",
        )
        .bind(email)
        .bind(channel)
        .fetch_one(&db)
        .await
        .unwrap();
        sqlx::query("INSERT INTO push_subscriptions (user_id, endpoint) VALUES ($1, $2)")
            .bind(id)
            .bind(format!("https://fcm.googleapis.com/fcm/send/{channel}"))
            .execute(&db)
            .await
            .unwrap();
    }
    let today = NaiveDate::from_ymd_opt(2030, 10, 30).unwrap();
    stock_item(&db, &f.group, "Lait", 1.0, today).await;

    let done = recap(&db, today, 9).await.unwrap();
    assert_eq!(done.recipients.len(), 3);
    let pushed = std::sync::Mutex::new(Vec::new());
    let send_push = |endpoint: String, _ttl: i64| {
        pushed.lock().unwrap().push(endpoint);
        async { PushOutcome::Delivered }
    };
    push_recap(&db, &send_push, &done.recipients).await.unwrap();
    let mut pushed = pushed.into_inner().unwrap();
    pushed.sort();
    assert_eq!(
        pushed,
        ["both", "push"].map(|c| format!("https://fcm.googleapis.com/fcm/send/{c}"))
    );
    assert_eq!(
        recap_bodies(&list(&f.router, &f.member, "").await),
        ["1 produit périme bientôt : Lait."]
    );
}

/// A member without a device reads the recap in the centre and gets no
/// push; a member with one gets it pushed, as a `stock_expiry` push.
#[sqlx::test]
async fn the_recap_is_pushed_to_the_members_who_have_a_device(db: PgPool) {
    let f = family(&db).await;
    let owner = user_id(&db, "owner@example.test").await;
    let endpoint = "https://fcm.googleapis.com/fcm/send/recap-owner";
    sqlx::query("INSERT INTO push_subscriptions (user_id, endpoint) VALUES ($1, $2)")
        .bind(owner)
        .bind(endpoint)
        .execute(&db)
        .await
        .unwrap();
    let today = NaiveDate::from_ymd_opt(2030, 10, 30).unwrap();
    stock_item(&db, &f.group, "Lait", 1.0, today).await;

    let done = recap(&db, today, 9).await.unwrap();
    let pushed = std::sync::Mutex::new(Vec::new());
    let send_push = |endpoint: String, ttl: i64| {
        pushed.lock().unwrap().push((endpoint, ttl));
        async { PushOutcome::Delivered }
    };
    push_recap(&db, &send_push, &done.recipients).await.unwrap();
    assert_eq!(
        pushed.into_inner().unwrap(),
        [(endpoint.to_string(), RECAP_PUSH_TTL_SECS)]
    );
    let kind: Option<String> =
        sqlx::query_scalar("SELECT pending_push_kind FROM push_subscriptions")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(kind.as_deref(), Some("stock_expiry"));
    assert_eq!(
        recap_bodies(&list(&f.router, &f.member, "").await),
        ["1 produit périme bientôt : Lait."]
    );
}

/// The hourly pass does not announce again a row the recap announced for
/// its date; its date moved, it does.
#[sqlx::test]
async fn the_hourly_pass_skips_a_row_the_recap_announced(db: PgPool) {
    let f = family(&db).await;
    let today = NaiveDate::from_ymd_opt(2030, 10, 30).unwrap();
    let lait = stock_item(&db, &f.group, "Lait", 1.0, today + Duration::days(2)).await;
    assert_eq!(recap(&db, today, 9).await.unwrap().written, 2);
    assert_eq!(notify_expiring(&db, today).await.unwrap(), 0);

    sqlx::query("UPDATE stock_items SET expires_on = $2 WHERE id = $1")
        .bind(lait)
        .bind(today + Duration::days(1))
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(notify_expiring(&db, today).await.unwrap(), 2);
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

// -- the agenda changes (#848) ------------------------------------------------

/// A change to the agenda is a kind of notification and of push the schema
/// takes (migration 0060), listed and filtered as the others; a kind it
/// does not know is still refused, in both columns.
#[sqlx::test]
async fn an_agenda_change_is_a_kind_the_schema_takes(db: PgPool) {
    let f = family(&db).await;
    let owner = user_id(&db, "owner@example.test").await;
    let change = insert(&db, owner, &f.group, "agenda_change", 1).await;
    insert(&db, owner, &f.group, "agenda_reminder", 2).await;
    let changes = list(&f.router, &f.owner, "?kind=agenda_change").await;
    assert_eq!(ids(&changes), [change]);
    assert_eq!(changes["notifications"][0]["kind"], "agenda_change");

    let refused = sqlx::query(
        "INSERT INTO notifications (user_id, group_id, kind, body)
         VALUES ($1, $2::uuid, 'agenda_changed', 'Texte.')",
    )
    .bind(owner)
    .bind(&f.group)
    .execute(&db)
    .await
    .unwrap_err();
    assert_eq!(
        refused.as_database_error().unwrap().constraint(),
        Some("notifications_kind_check")
    );

    sqlx::query(
        "INSERT INTO push_subscriptions (user_id, endpoint, pending_push_kind, pending_pushes)
         VALUES ($1, 'https://fcm.googleapis.com/fcm/send/agenda-change', 'agenda_change', 1)",
    )
    .bind(owner)
    .execute(&db)
    .await
    .unwrap();
    let refused = sqlx::query("UPDATE push_subscriptions SET pending_push_kind = 'agenda'")
        .execute(&db)
        .await
        .unwrap_err();
    assert_eq!(
        refused.as_database_error().unwrap().constraint(),
        Some("push_subscriptions_pending_push_kind_check")
    );
}

/// Every account is told of the agenda changes unless it says otherwise:
/// the setting is on for an account just made, and can be turned off.
#[sqlx::test]
async fn an_account_is_told_of_the_agenda_changes_by_default(db: PgPool) {
    family(&db).await;
    let on: Vec<bool> = sqlx::query_scalar("SELECT notify_agenda_changes FROM users")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(on, [true, true]);
    sqlx::query("UPDATE users SET notify_agenda_changes = false WHERE email = $1")
        .bind("member@example.test")
        .execute(&db)
        .await
        .unwrap();
    let off: bool = sqlx::query_scalar("SELECT notify_agenda_changes FROM users WHERE email = $1")
        .bind("member@example.test")
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(!off);
    let refused = sqlx::query("UPDATE users SET notify_agenda_changes = NULL")
        .execute(&db)
        .await;
    assert!(refused.is_err());
}

/// The agenda-change setting is read and written by
/// `/account/notifications` (#850): on by default, turned off and on
/// again without touching the channel nor the recap's hour, left as it is
/// by a request without it, anything but a boolean refused — and only the
/// caller's own account is changed.
#[sqlx::test]
async fn the_agenda_change_setting_is_the_caller_s_own(db: PgPool) {
    let f = family(&db).await;
    let settings = |cookie: String| {
        let router = f.router.clone();
        async move {
            let res = call(
                &router,
                Method::GET,
                "/account/notifications",
                Some(&cookie),
                None,
            )
            .await;
            assert_status(&res, StatusCode::OK);
            json_body(res).await
        }
    };
    let put = |body: Value| {
        let (router, cookie) = (f.router.clone(), f.owner.clone());
        async move {
            call(
                &router,
                Method::PUT,
                "/account/notifications",
                Some(&cookie),
                Some(body),
            )
            .await
        }
    };
    let state = |s: Value| {
        (
            s["reminder_channel"].clone(),
            s["expiry_digest_hour"].clone(),
            s["notify_agenda_changes"].clone(),
        )
    };
    assert_eq!(
        settings(f.owner.clone()).await["notify_agenda_changes"],
        true
    );
    set_digest_hour(&db, "owner@example.test", 18).await;

    let res = put(json!({"reminder_channel": "both", "notify_agenda_changes": false})).await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(
        state(settings(f.owner.clone()).await),
        (json!("both"), json!(18), json!(false))
    );

    // Without the field, the setting stays as it is.
    let res = put(json!({"reminder_channel": "email", "expiry_digest_hour": 7})).await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(
        state(settings(f.owner.clone()).await),
        (json!("email"), json!(7), json!(false))
    );

    for wrong in [json!("no"), json!(0)] {
        let res = put(json!({"reminder_channel": "email", "notify_agenda_changes": wrong})).await;
        assert!(res.status().is_client_error(), "{wrong}: {}", res.status());
    }
    assert_eq!(
        settings(f.owner.clone()).await["notify_agenda_changes"],
        false
    );

    let res = put(json!({"reminder_channel": "email", "notify_agenda_changes": true})).await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(
        state(settings(f.owner.clone()).await),
        (json!("email"), json!(7), json!(true))
    );
    // Without the field, an enabled setting stays enabled too.
    let res = put(json!({"reminder_channel": "both"})).await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(
        state(settings(f.owner.clone()).await),
        (json!("both"), json!(7), json!(true))
    );

    // The member turns theirs off: the owner's stays on.
    let res = call(
        &f.router,
        Method::PUT,
        "/account/notifications",
        Some(&f.member),
        Some(json!({"reminder_channel": "push", "notify_agenda_changes": false})),
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(
        settings(f.member.clone()).await["notify_agenda_changes"],
        false
    );
    assert_eq!(
        settings(f.owner.clone()).await["notify_agenda_changes"],
        true
    );
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

// -- agenda changes (#849) -------------------------------------------------------

/// `family` with a third member, `third@example.test`, signed in.
async fn family_of_three(db: &PgPool) -> (Family, String) {
    let f = family(db).await;
    let third = register_verify_login(&f.router, db, "third@example.test").await;
    invite_and_join(&f.router, &f.group, &f.owner, &third).await;
    (f, third)
}

/// 14:00 UTC on 10 December 2026 is 15:00 in Paris.
const DINNER_AT: &str = "2026-12-10T14:00:00Z";
const DINNER_ENDS: &str = "2026-12-10T15:00:00Z";

async fn create_event(f: &Family, cookie: &str, fields: Value) -> String {
    let mut body = json!({"title": "Dîner", "starts_at": DINNER_AT, "ends_at": DINNER_ENDS});
    body.as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    let res = call(
        &f.router,
        Method::POST,
        &format!("/groups/{}/events", f.group),
        Some(cookie),
        Some(body),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

async fn patch_event(f: &Family, cookie: &str, event: &str, fields: Value) -> Value {
    let res = call(
        &f.router,
        Method::PATCH,
        &format!("/groups/{}/events/{event}", f.group),
        Some(cookie),
        Some(fields),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    json_body(res).await
}

async fn delete_event(f: &Family, cookie: &str, event: &str, query: &str) {
    let res = call(
        &f.router,
        Method::DELETE,
        &format!("/groups/{}/events/{event}{query}", f.group),
        Some(cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
}

/// Every `agenda_change` row, as (recipient's address, body), by address
/// then by time.
async fn agenda_rows(db: &PgPool) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT u.email, n.body FROM notifications n JOIN users u ON u.id = n.user_id
         WHERE n.kind = 'agenda_change' ORDER BY u.email, n.created_at, n.id",
    )
    .fetch_all(db)
    .await
    .unwrap()
}

async fn clear_notifications(db: &PgPool) {
    sqlx::query("DELETE FROM notifications")
        .execute(db)
        .await
        .unwrap();
}

fn rows(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected
        .iter()
        .map(|(to, body)| (to.to_string(), body.to_string()))
        .collect()
}

/// A shared event, added, changed and deleted by its creator: every other
/// member is told each time, its author never, and every row names the
/// event — the deleted one included.
#[sqlx::test]
async fn a_shared_event_tells_every_other_member_of_each_write(db: PgPool) {
    let (f, _third) = family_of_three(&db).await;
    let event = create_event(&f, &f.owner, json!({})).await;
    assert_eq!(
        agenda_rows(&db).await,
        rows(&[
            (
                "member@example.test",
                "owner@example.test a ajouté « Dîner » le 10/12/2026 à 15:00."
            ),
            (
                "third@example.test",
                "owner@example.test a ajouté « Dîner » le 10/12/2026 à 15:00."
            ),
        ])
    );
    assert_eq!(
        kinds_and_bodies(&list(&f.router, &f.member, "?kind=agenda_change").await),
        [(
            "agenda_change".to_string(),
            "owner@example.test a ajouté « Dîner » le 10/12/2026 à 15:00.".to_string()
        )]
    );
    assert_eq!(unread(&f.router, &f.owner).await, 0);

    clear_notifications(&db).await;
    patch_event(&f, &f.owner, &event, json!({"location": "Parc"})).await;
    assert_eq!(
        agenda_rows(&db).await,
        rows(&[
            (
                "member@example.test",
                "owner@example.test a modifié « Dîner » : lieu."
            ),
            (
                "third@example.test",
                "owner@example.test a modifié « Dîner » : lieu."
            ),
        ])
    );

    clear_notifications(&db).await;
    delete_event(&f, &f.owner, &event, "").await;
    assert_eq!(
        agenda_rows(&db).await,
        rows(&[
            (
                "member@example.test",
                "owner@example.test a supprimé « Dîner » du 10/12/2026 à 15:00."
            ),
            (
                "third@example.test",
                "owner@example.test a supprimé « Dîner » du 10/12/2026 à 15:00."
            ),
        ])
    );
    let objects: Vec<(Option<Uuid>, Option<Uuid>)> =
        sqlx::query_as("SELECT object_id, actor_id FROM notifications")
            .fetch_all(&db)
            .await
            .unwrap();
    let owner = user_id(&db, "owner@example.test").await;
    let event = Uuid::parse_str(&event).unwrap();
    assert_eq!(objects, [(Some(event), Some(owner)); 2]);
}

/// A private event tells its participants alone: the member outside it has
/// no row, and nothing of its title reaches their centre.
#[sqlx::test]
async fn a_private_event_tells_its_participants_alone(db: PgPool) {
    let (f, third) = family_of_three(&db).await;
    let member = user_id(&db, "member@example.test").await;
    let owner = user_id(&db, "owner@example.test").await;
    let event = create_event(
        &f,
        &f.owner,
        json!({"title": "Surprise", "is_private": true, "assignee_ids": [owner, member]}),
    )
    .await;
    // Changed and deleted by its assignee, it tells its creator.
    patch_event(&f, &f.member, &event, json!({"title": "Surprise !"})).await;
    delete_event(&f, &f.member, &event, "").await;
    assert_eq!(
        agenda_rows(&db).await,
        rows(&[
            (
                "member@example.test",
                "owner@example.test a ajouté « Surprise » le 10/12/2026 à 15:00."
            ),
            (
                "owner@example.test",
                "member@example.test a modifié « Surprise ! » : titre."
            ),
            (
                "owner@example.test",
                "member@example.test a supprimé « Surprise ! » du 10/12/2026 à 15:00."
            ),
        ])
    );
    let page = list(&f.router, &third, "").await;
    assert_eq!(page["notifications"], json!([]));
    assert!(!page.to_string().contains("Surprise"));
}

/// The setting off, an account deactivated or purged: told nothing.
#[sqlx::test]
async fn a_member_who_opted_out_or_whose_account_is_gone_is_not_told(db: PgPool) {
    let (f, _third) = family_of_three(&db).await;
    let fourth = register_verify_login(&f.router, &db, "fourth@example.test").await;
    invite_and_join(&f.router, &f.group, &f.owner, &fourth).await;
    for (update, email) in [
        (
            "UPDATE users SET notify_agenda_changes = false WHERE email = $1",
            "member@example.test",
        ),
        (
            "UPDATE users SET deactivated_at = now() WHERE email = $1",
            "third@example.test",
        ),
        (
            "UPDATE users SET deleted_at = now() WHERE email = $1",
            "fourth@example.test",
        ),
    ] {
        sqlx::query(update).bind(email).execute(&db).await.unwrap();
    }
    let event = create_event(&f, &f.owner, json!({})).await;
    patch_event(&f, &f.owner, &event, json!({"title": "Déjeuner"})).await;
    delete_event(&f, &f.owner, &event, "").await;
    assert_eq!(agenda_rows(&db).await, rows(&[]));
}

/// What a member does not see of an event in the list is no news: its
/// description, its reminders, a task's completion — nor a form sent back
/// unchanged, with an empty location and the assignees in another order.
#[sqlx::test]
async fn a_write_that_changes_nothing_visible_tells_no_one(db: PgPool) {
    let f = family(&db).await;
    let owner = user_id(&db, "owner@example.test").await;
    let member = user_id(&db, "member@example.test").await;
    let event = create_event(&f, &f.owner, json!({"assignee_ids": [owner, member]})).await;
    let task = create_event(&f, &f.owner, json!({"title": "Courses", "is_task": true})).await;
    clear_notifications(&db).await;

    patch_event(
        &f,
        &f.owner,
        &event,
        json!({"description": "Apporter le dessert"}),
    )
    .await;
    patch_event(
        &f,
        &f.owner,
        &event,
        json!({"title": "Dîner", "location": "", "starts_at": DINNER_AT, "ends_at": DINNER_ENDS,
               "all_day": false, "rrule": "", "assignee_ids": [member, owner], "is_private": false}),
    )
    .await;
    let reminder = call(
        &f.router,
        Method::POST,
        &format!("/groups/{}/events/{event}/reminders", f.group),
        Some(&f.owner),
        Some(json!({"offset_minutes": 60})),
    )
    .await;
    assert_status(&reminder, StatusCode::CREATED);
    patch_event(&f, &f.owner, &task, json!({"completed": true})).await;
    assert_eq!(agenda_rows(&db).await, rows(&[]));
}

/// A series changed or deleted under each scope tells each member once,
/// and the occurrences split off it are told by what the request changed
/// alone.
#[sqlx::test]
async fn a_series_tells_once_per_write_whatever_its_scope(db: PgPool) {
    let f = family(&db).await;
    // Weekly from Monday 7 December 2026, 09:00 UTC (10:00 in Paris).
    let series = json!({"title": "Piscine", "starts_at": "2026-12-07T09:00:00Z",
                        "ends_at": "2026-12-07T10:00:00Z", "rrule": "FREQ=WEEKLY;COUNT=5"});
    let second = "2026-12-14T09:00:00Z";
    for scope in ["all", "this", "this_and_following"] {
        let event = create_event(&f, &f.owner, series.clone()).await;
        clear_notifications(&db).await;
        patch_event(
            &f,
            &f.owner,
            &event,
            json!({"title": "Natation", "scope": scope, "occurrence_at": second}),
        )
        .await;
        assert_eq!(
            agenda_rows(&db).await,
            rows(&[(
                "member@example.test",
                "owner@example.test a modifié « Natation » : titre."
            )]),
            "PATCH {scope}"
        );
    }
    // Moved an hour later under `this`: its date and hour, nothing else,
    // about the event that now holds the occurrence.
    let event = create_event(&f, &f.owner, series.clone()).await;
    clear_notifications(&db).await;
    let moved = patch_event(
        &f,
        &f.owner,
        &event,
        json!({"scope": "this", "occurrence_at": second,
               "starts_at": "2026-12-14T10:00:00Z", "ends_at": "2026-12-14T11:00:00Z"}),
    )
    .await;
    assert_eq!(
        agenda_rows(&db).await,
        rows(&[(
            "member@example.test",
            "owner@example.test a modifié « Piscine » : date et heure."
        )])
    );
    let object: Option<Uuid> = sqlx::query_scalar("SELECT object_id FROM notifications")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(object.unwrap().to_string(), moved["id"].as_str().unwrap());

    for (query, day) in [
        (format!("?scope=this&occurrence_at={second}"), "14/12/2026"),
        (
            format!("?scope=this_and_following&occurrence_at={second}"),
            "14/12/2026",
        ),
        ("?scope=all".to_string(), "07/12/2026"),
    ] {
        let event = create_event(&f, &f.owner, series.clone()).await;
        clear_notifications(&db).await;
        delete_event(&f, &f.owner, &event, &query).await;
        let expected = format!("owner@example.test a supprimé « Piscine » du {day} à 10:00.");
        assert_eq!(
            agenda_rows(&db).await,
            rows(&[("member@example.test", &expected)]),
            "DELETE {query}"
        );
    }
}

/// A device of `email`'s account, under an endpoint no push service owns:
/// `push::send` answers it `Gone` without any request, and the device is
/// then forgotten — which is how a test sees that it was pushed.
async fn unreachable_device(db: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO push_subscriptions (user_id, endpoint)
         SELECT id, 'https://push.invalid/' || gen_random_uuid() FROM users WHERE email = $1
         RETURNING id",
    )
    .bind(email)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn device_kept(db: &PgPool, id: Uuid) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM push_subscriptions WHERE id = $1)")
        .bind(id)
        .fetch_one(db)
        .await
        .unwrap()
}

/// Waits, 10 s at most, for `device` to have been pushed (and forgotten).
async fn pushed_within_10_s(db: &PgPool, device: Uuid, what: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while device_kept(db, device).await {
        assert!(
            std::time::Instant::now() < deadline,
            "{what}: the device was not pushed within 10 s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// #851: once each write committed, the members told whose channel
/// includes notifications get a push on their devices — not its author,
/// nor a member on email. Every write that tells, under each of the
/// handlers' commits: an addition, a change of the whole series and of one
/// occurrence (split off it), a deletion of one occurrence, of the
/// following ones, and of the whole.
#[sqlx::test]
async fn each_agenda_write_pushes_the_members_told_on_notifications(db: PgPool) {
    let (f, _third) = family_of_three(&db).await;
    sqlx::query("UPDATE users SET reminder_channel = 'email' WHERE email = 'third@example.test'")
        .execute(&db)
        .await
        .unwrap();
    let series = json!({"title": "Piscine", "starts_at": "2026-12-07T09:00:00Z",
                        "ends_at": "2026-12-07T10:00:00Z", "rrule": "FREQ=WEEKLY;COUNT=5"});
    let owner_device = unreachable_device(&db, "owner@example.test").await;
    let third_device = unreachable_device(&db, "third@example.test").await;

    let mut event = String::new();
    for write in [
        "POST",
        "PATCH",
        "PATCH this",
        "DELETE this",
        "DELETE this_and_following",
        "DELETE all",
    ] {
        let member_device = unreachable_device(&db, "member@example.test").await;
        match write {
            "POST" => event = create_event(&f, &f.owner, series.clone()).await,
            "PATCH" => {
                patch_event(&f, &f.owner, &event, json!({"location": "Parc"})).await;
            }
            // Splits the occurrence of 21 December off the series.
            "PATCH this" => {
                let fields = json!({"title": "Natation", "scope": "this",
                                    "occurrence_at": "2026-12-21T09:00:00Z"});
                patch_event(&f, &f.owner, &event, fields).await;
            }
            "DELETE this" => {
                let query = "?scope=this&occurrence_at=2026-12-14T09:00:00Z";
                delete_event(&f, &f.owner, &event, query).await;
            }
            "DELETE this_and_following" => {
                let query = "?scope=this_and_following&occurrence_at=2026-12-28T09:00:00Z";
                delete_event(&f, &f.owner, &event, query).await;
            }
            _ => delete_event(&f, &f.owner, &event, "").await,
        }
        pushed_within_10_s(&db, member_device, write).await;
    }
    assert!(device_kept(&db, owner_device).await, "the author");
    assert!(device_kept(&db, third_device).await, "a member on email");
}

/// Made private, an event tells nothing to those who stop seeing it; a
/// member added to a private one's participants is told it was added, one
/// taken off it nothing; made shared again, it is added for those who see
/// it now.
#[sqlx::test]
async fn a_change_of_privacy_tells_who_sees_the_event_afterwards(db: PgPool) {
    let (f, _third) = family_of_three(&db).await;
    let owner = user_id(&db, "owner@example.test").await;
    let member = user_id(&db, "member@example.test").await;
    let third = user_id(&db, "third@example.test").await;
    let event = create_event(&f, &f.owner, json!({"assignee_ids": [owner, member]})).await;

    clear_notifications(&db).await;
    patch_event(
        &f,
        &f.owner,
        &event,
        json!({"is_private": true, "title": "Secret"}),
    )
    .await;
    assert_eq!(
        agenda_rows(&db).await,
        rows(&[(
            "member@example.test",
            "owner@example.test a modifié « Secret » : titre."
        )])
    );

    clear_notifications(&db).await;
    patch_event(
        &f,
        &f.owner,
        &event,
        json!({"assignee_ids": [owner, member, third]}),
    )
    .await;
    assert_eq!(
        agenda_rows(&db).await,
        rows(&[
            (
                "member@example.test",
                "owner@example.test a modifié « Secret » : participants."
            ),
            (
                "third@example.test",
                "owner@example.test a ajouté « Secret » le 10/12/2026 à 15:00."
            ),
        ])
    );

    clear_notifications(&db).await;
    patch_event(
        &f,
        &f.owner,
        &event,
        json!({"assignee_ids": [owner, member]}),
    )
    .await;
    assert_eq!(
        agenda_rows(&db).await,
        rows(&[(
            "member@example.test",
            "owner@example.test a modifié « Secret » : participants."
        )])
    );

    clear_notifications(&db).await;
    patch_event(&f, &f.owner, &event, json!({"is_private": false})).await;
    assert_eq!(
        agenda_rows(&db).await,
        rows(&[(
            "third@example.test",
            "owner@example.test a ajouté « Secret » le 10/12/2026 à 15:00."
        )])
    );
}

/// An event mirrored from an imported calendar tells no one of a change
/// nor of its deletion.
#[sqlx::test]
async fn an_imported_event_tells_no_one(db: PgPool) {
    let f = family(&db).await;
    let owner = user_id(&db, "owner@example.test").await;
    let event = create_event(&f, &f.owner, json!({})).await;
    sqlx::query(
        "WITH import AS (
             INSERT INTO calendar_imports (group_id, created_by, label, feed_url)
             VALUES ($1::uuid, $2, 'Agenda', '\\x00'::bytea) RETURNING id
         )
         INSERT INTO calendar_import_events (calendar_import_id, event_id, external_uid)
         SELECT id, $3::uuid, 'uid-849' FROM import",
    )
    .bind(&f.group)
    .bind(owner)
    .bind(&event)
    .execute(&db)
    .await
    .unwrap();
    clear_notifications(&db).await;
    patch_event(&f, &f.owner, &event, json!({"title": "Déjeuner"})).await;
    delete_event(&f, &f.owner, &event, "").await;
    assert_eq!(agenda_rows(&db).await, rows(&[]));
}

// -- calendar imports (#852) ------------------------------------------------------

/// An ICS feed of the events `uids`, each an hour on its own day of
/// December 2026. Each was last modified on 1 January, but `moved`, on
/// 1 February: the stamp a re-import compares to tell a changed event.
fn feed(uids: std::ops::Range<u32>, moved: Option<u32>) -> String {
    let mut ics = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n");
    for uid in uids {
        let day = uid % 28 + 1;
        let modified = if moved == Some(uid) {
            "20260201T090000Z"
        } else {
            "20260101T090000Z"
        };
        ics.push_str(&format!(
            "BEGIN:VEVENT\r\nUID:import-852-{uid}@google.com\r\nDTSTAMP:20260101T090000Z\r\n\
             DTSTART:202612{day:02}T090000Z\r\nDTEND:202612{day:02}T100000Z\r\n\
             SUMMARY:Cours {uid}\r\nLAST-MODIFIED:{modified}\r\nEND:VEVENT\r\n"
        ));
    }
    ics.push_str("END:VCALENDAR\r\n");
    ics
}

/// Serves `bodies` over plain HTTP on 127.0.0.1, one per connection and in
/// order: a calendar feed that changes between two syncs.
async fn serve_feeds(bodies: Vec<String>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for body in bodies {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/calendar\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        }
    });
    format!("http://{addr}/feed.ics")
}

/// The family's calendar import of a feed serving `bodies` in order,
/// registered by its owner.
async fn calendar_import(f: &Family, bodies: Vec<String>) -> String {
    let feed_url = serve_feeds(bodies).await;
    let res = call(
        &f.router,
        Method::POST,
        &format!("/groups/{}/calendar-imports", f.group),
        Some(&f.owner),
        Some(json!({"label": "École", "feed_url": feed_url})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

/// Runs the import `import` as `cookie`; returns its counters.
async fn run_import(f: &Family, cookie: &str, import: &str) -> Value {
    let res = call(
        &f.router,
        Method::POST,
        &format!("/groups/{}/calendar-imports/{import}/import", f.group),
        Some(cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    json_body(res).await
}

/// #852: an import that added or changed events writes one row per other
/// member, with their exact count and no title; a re-import that changed
/// nothing writes none; its author is never told, nor a member who opted
/// out.
#[sqlx::test]
async fn an_import_tells_each_other_member_once_with_its_count(db: PgPool) {
    let (f, _third) = family_of_three(&db).await;
    let member = user_id(&db, "member@example.test").await;
    let import = calendar_import(
        &f,
        vec![
            feed(0..30, None),
            feed(0..30, None),
            // The first event moved upstream, a 31st is new.
            feed(0..31, Some(0)),
        ],
    )
    .await;

    // 30 new events, imported by a member: one row for each other member.
    let run = run_import(&f, &f.member, &import).await;
    assert_eq!(
        (run["imported"].as_u64(), run["updated"].as_u64()),
        (Some(30), Some(0))
    );
    let told = "member@example.test a importé 30 événements.";
    assert_eq!(
        agenda_rows(&db).await,
        rows(&[("owner@example.test", told), ("third@example.test", told)])
    );
    // About no single event, by the member who ran it.
    let about: Vec<(Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "SELECT object_id, actor_id FROM notifications WHERE kind = 'agenda_change'",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(about, vec![(None, Some(member)); 2]);

    // The same feed again: nothing changed, nobody told.
    clear_notifications(&db).await;
    let run = run_import(&f, &f.member, &import).await;
    assert_eq!(run["skipped"].as_u64(), Some(30));
    assert_eq!(agenda_rows(&db).await, rows(&[]));

    // One event changed and one added, imported by the owner; the third
    // member opted out.
    sqlx::query(
        "UPDATE users SET notify_agenda_changes = false WHERE email = 'third@example.test'",
    )
    .execute(&db)
    .await
    .unwrap();
    let run = run_import(&f, &f.owner, &import).await;
    assert_eq!(
        (run["imported"].as_u64(), run["updated"].as_u64()),
        (Some(1), Some(1))
    );
    assert_eq!(
        agenda_rows(&db).await,
        rows(&[(
            "member@example.test",
            "owner@example.test a importé 2 événements."
        )])
    );
}

/// #852: once the import committed, the members it told whose channel
/// includes notifications get a push — not its author, nor a member on
/// email.
#[sqlx::test]
async fn an_import_pushes_the_members_it_told_on_notifications(db: PgPool) {
    let (f, _third) = family_of_three(&db).await;
    sqlx::query("UPDATE users SET reminder_channel = 'email' WHERE email = 'third@example.test'")
        .execute(&db)
        .await
        .unwrap();
    let import = calendar_import(&f, vec![feed(0..1, None)]).await;
    let owner_device = unreachable_device(&db, "owner@example.test").await;
    let member_device = unreachable_device(&db, "member@example.test").await;
    let third_device = unreachable_device(&db, "third@example.test").await;

    run_import(&f, &f.member, &import).await;
    pushed_within_10_s(&db, owner_device, "import").await;
    assert!(device_kept(&db, member_device).await, "the author");
    assert!(device_kept(&db, third_device).await, "a member on email");
}

/// A 500-character title broken by new lines is stored, and its
/// notifications stay one line within the column's 300 characters.
#[sqlx::test]
async fn a_long_title_still_fits_the_notification(db: PgPool) {
    let f = family(&db).await;
    let title = format!(
        "{}\n{}\r\n{}",
        "t".repeat(200),
        "u".repeat(200),
        "v".repeat(98)
    );
    let event = create_event(&f, &f.owner, json!({ "title": title })).await;
    patch_event(&f, &f.owner, &event, json!({"location": "Parc"})).await;
    delete_event(&f, &f.owner, &event, "").await;
    let bodies: Vec<String> =
        sqlx::query_scalar("SELECT body FROM notifications WHERE kind = 'agenda_change'")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(bodies.len(), 3);
    for body in bodies {
        assert!(body.chars().count() <= 300, "{body}");
        assert!(!body.contains(['\n', '\r']), "{body}");
    }
}
