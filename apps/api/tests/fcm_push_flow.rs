//! The Android app's devices pushed by FCM (#1003), against a real
//! database: the agenda changes, the budget alerts and the expiry recap go
//! to a member's phones as to their browsers; the reminders do not, the
//! app schedules them itself (#668); FCM's answer is recorded on the
//! device as a push service's, `UNREGISTERED` forgetting it.
//!
//! Most tests stand recording senders in for the push services, as
//! `push_reminders_flow` does. The real sender (`fcm::FcmSender`) is
//! driven against a local stand-in for Google's token endpoint and FCM's
//! API: no test reaches Google.

mod common;

use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::IntoResponse;
use manage_our_home::agenda::change_notices::{push_changes, CHANGE_PUSH_TTL_SECS};
use manage_our_home::budget::alerts::{push_alerts, ALERT_PUSH_TTL_SECS};
use manage_our_home::jobs::scheduled_notifications::send_due_notifications;
use manage_our_home::jobs::stock_expiry_recap::{push_recap, RECAP_PUSH_TTL_SECS};
use manage_our_home::notifications::center::Entry;
use manage_our_home::notifications::fcm::{self, Message};
use manage_our_home::notifications::push::{Payload, PushKind, PushOutcome};
use sqlx::PgPool;
use uuid::Uuid;

const MOZILLA: &str = "https://updates.push.services.mozilla.com/wpush/v2/gAAAAABk";
const PHONE: &str = "dVq3mJ8kTq2xQ9Yw1ZbC0e:APA91bHk3s-Lp_Q7vR2mXy8T0aBcDeFg";
const GONE_PHONE: &str = "gone:APA91bHk3s-Lp_Q7vR2mXy8T0aBcDeFg";

async fn insert_user(db: &PgPool, email: &str, channel: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, display_name, age_declared_at, reminder_channel)
         VALUES ($1, 'not-a-real-hash', $1, now() - interval '1 year', $2)
         RETURNING id",
    )
    .bind(email)
    .bind(channel)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn add_device(db: &PgPool, user: Uuid, endpoint: &str, platform: &str) {
    sqlx::query("INSERT INTO push_subscriptions (user_id, endpoint, platform) VALUES ($1, $2, $3)")
        .bind(user)
        .bind(endpoint)
        .bind(platform)
        .execute(db)
        .await
        .unwrap();
}

/// A device's bookkeeping: delivered once (`last_success_at`), failures
/// in a row, pushes counted pending. `None` once forgotten.
async fn device(db: &PgPool, endpoint: &str) -> Option<(bool, i32, i32)> {
    sqlx::query_as(
        "SELECT last_success_at IS NOT NULL, consecutive_failures, pending_pushes
         FROM push_subscriptions WHERE endpoint = $1",
    )
    .bind(endpoint)
    .fetch_optional(db)
    .await
    .unwrap()
}

fn entries_of(members: &[Uuid]) -> Vec<Entry> {
    members
        .iter()
        .map(|&member| Entry {
            member,
            id: Uuid::new_v4(),
        })
        .collect()
}

/// A group owned by `by`, one event of theirs starting in 9 minutes, and
/// its reminder's notification due a minute ago.
async fn due_reminder(db: &PgPool, by: Uuid) -> Uuid {
    let group: Uuid = sqlx::query_scalar(
        "INSERT INTO groups (name, created_by) VALUES ('Maison', $1) RETURNING id",
    )
    .bind(by)
    .fetch_one(db)
    .await
    .unwrap();
    let event: Uuid = sqlx::query_scalar(
        "INSERT INTO events (group_id, created_by, title, starts_at, ends_at)
         VALUES ($1, $2, 'IRM', now() + interval '9 minutes', now() + interval '1 hour')
         RETURNING id",
    )
    .bind(group)
    .bind(by)
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
    sqlx::query_scalar(
        "INSERT INTO scheduled_notifications (event_reminder_id, event_id, occurrence_at, fire_at)
         VALUES ($1, $2, now() + interval '9 minutes', now() - interval '1 minute')
         RETURNING id",
    )
    .bind(reminder)
    .bind(event)
    .fetch_one(db)
    .await
    .unwrap()
}

/// What the recording senders were handed: the browsers' endpoints, and
/// the phones' tokens with their message.
#[derive(Default)]
struct Sent {
    web: Mutex<Vec<String>>,
    fcm: Mutex<Vec<(String, Message)>>,
}

impl Sent {
    fn web(&self) -> Vec<String> {
        std::mem::take(&mut self.web.lock().unwrap())
    }

    fn fcm(&self) -> Vec<(String, Message)> {
        std::mem::take(&mut self.fcm.lock().unwrap())
    }
}

/// The three kinds that go by FCM, each pushed through its own function
/// with recording senders that deliver.
async fn push_kind(db: &PgPool, kind: PushKind, entries: &[Entry], sent: &Sent) {
    let send_push = |endpoint: String, _: i64, _: Option<Payload>| {
        sent.web.lock().unwrap().push(endpoint);
        async { PushOutcome::Delivered }
    };
    let send_fcm = |token: String, message: Message| {
        sent.fcm.lock().unwrap().push((token, message));
        async { PushOutcome::Delivered }
    };
    let (web, fcm) = (Some(&send_push), Some(&send_fcm));
    match kind {
        PushKind::AgendaChange => push_changes(db, web, fcm, entries).await,
        PushKind::BudgetAlert => push_alerts(db, web, fcm, entries).await,
        PushKind::StockExpiry => push_recap(db, web, fcm, entries).await,
        PushKind::AgendaReminder => unreachable!("not pushed through push_entries"),
    }
    .unwrap();
}

/// An agenda change, a budget alert and an expiry recap each push the
/// member's phone and their browser: the phone gets the kind, the
/// member's entries and the kind's TTL, and is stamped delivered with no
/// push counted pending — the message tells the app its kind.
#[sqlx::test]
async fn a_change_an_alert_and_a_recap_push_the_phone_and_the_browser(db: PgPool) {
    let member = insert_user(&db, "both@example.test", "push").await;
    add_device(&db, member, MOZILLA, "web").await;
    add_device(&db, member, PHONE, "fcm").await;
    let sent = Sent::default();

    for (kind, ttl) in [
        (PushKind::AgendaChange, CHANGE_PUSH_TTL_SECS),
        (PushKind::BudgetAlert, ALERT_PUSH_TTL_SECS),
        (PushKind::StockExpiry, RECAP_PUSH_TTL_SECS),
    ] {
        let entries = entries_of(&[member, member]);
        push_kind(&db, kind, &entries, &sent).await;
        assert_eq!(sent.web(), [MOZILLA], "{kind:?}");
        let mut ids: Vec<Uuid> = entries.iter().map(|e| e.id).collect();
        let mut phone = sent.fcm();
        assert_eq!(phone.len(), 1, "{kind:?}");
        let (token, mut message) = phone.remove(0);
        assert_eq!(token, PHONE);
        ids.sort();
        message.entries.sort();
        assert_eq!(
            message,
            Message {
                kind,
                entries: ids,
                ttl_secs: ttl,
            }
        );
        assert_eq!(device(&db, PHONE).await, Some((true, 0, 0)), "{kind:?}");
    }
}

/// A member whose channel is email alone gets no push on either.
#[sqlx::test]
async fn a_member_on_email_gets_no_push_on_the_phone(db: PgPool) {
    let member = insert_user(&db, "email@example.test", "email").await;
    add_device(&db, member, MOZILLA, "web").await;
    add_device(&db, member, PHONE, "fcm").await;
    let sent = Sent::default();
    push_kind(&db, PushKind::BudgetAlert, &entries_of(&[member]), &sent).await;
    assert!(sent.web().is_empty());
    assert!(sent.fcm().is_empty());
}

/// A reminder pushes the browser alone: the app schedules its own (#668).
/// The phone is left as it was — nothing sent, nothing recorded.
#[sqlx::test]
async fn a_reminder_pushes_the_browser_alone(db: PgPool) {
    let member = insert_user(&db, "remind@example.test", "push").await;
    add_device(&db, member, MOZILLA, "web").await;
    add_device(&db, member, PHONE, "fcm").await;
    let reminder = due_reminder(&db, member).await;

    let pushed = Mutex::new(Vec::new());
    let send = |_: String, _: String, _: manage_our_home::email::EmailContent| async {
        Ok::<(), anyhow::Error>(())
    };
    let send_push = |endpoint: String, _: i64, _: Option<Payload>| {
        pushed.lock().unwrap().push(endpoint);
        async { PushOutcome::Delivered }
    };
    send_due_notifications(&db, "https://maison.example", send, send_push)
        .await
        .unwrap();

    assert_eq!(pushed.into_inner().unwrap(), [MOZILLA]);
    assert_eq!(device(&db, PHONE).await, Some((false, 0, 0)));
    let status: String =
        sqlx::query_scalar("SELECT status FROM scheduled_notifications WHERE id = $1")
            .bind(reminder)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(status, "sent");
}

/// A server without FCM sends nothing by it: the browsers get their push
/// as before, and the phone is left as it was.
#[sqlx::test]
async fn without_fcm_the_browser_still_gets_its_push(db: PgPool) {
    let member = insert_user(&db, "nofcm@example.test", "push").await;
    add_device(&db, member, MOZILLA, "web").await;
    add_device(&db, member, PHONE, "fcm").await;
    let pushed = Mutex::new(Vec::new());
    let send_push = |endpoint: String, _: i64, _: Option<Payload>| {
        pushed.lock().unwrap().push(endpoint);
        async { PushOutcome::Delivered }
    };
    push_alerts(
        &db,
        Some(&send_push),
        common::NO_FCM,
        &entries_of(&[member]),
    )
    .await
    .unwrap();
    assert_eq!(pushed.into_inner().unwrap(), [MOZILLA]);
    assert_eq!(device(&db, PHONE).await, Some((false, 0, 0)));
    // Delivered; its push stays counted until the browser shows it.
    assert_eq!(device(&db, MOZILLA).await, Some((true, 0, 1)));
}

/// A failed FCM push is counted on the phone as a push service's failure.
#[sqlx::test]
async fn a_failed_fcm_push_is_counted_on_the_phone(db: PgPool) {
    let member = insert_user(&db, "failing@example.test", "push").await;
    add_device(&db, member, PHONE, "fcm").await;
    let send_fcm = |_: String, _: Message| async { PushOutcome::Failed("FCM answered 503".into()) };
    push_alerts(
        &db,
        None::<&common::NoWeb>,
        Some(&send_fcm),
        &entries_of(&[member]),
    )
    .await
    .unwrap();
    assert_eq!(device(&db, PHONE).await, Some((false, 1, 0)));
}

// -- the real sender, against a local stand-in for Google ---------------------

/// What the stand-in saw, and how it answers.
#[derive(Default)]
struct Google {
    /// The token requests' bodies, form-encoded.
    token_requests: Vec<String>,
    /// The sends: `Authorization` header and JSON body.
    sends: Vec<(String, serde_json::Value)>,
    /// Answer the next send 401, once.
    refuse_next: bool,
}

/// Google's token endpoint at `/token` and FCM's API under `/v1/…`: a
/// token `stand-in-<n>` for the n-th token request; for a send, 404
/// `UNREGISTERED` to [`GONE_PHONE`], 401 when told to, 200 otherwise.
async fn google_stand_in() -> (String, Arc<Mutex<Google>>) {
    let google = Arc::new(Mutex::new(Google::default()));
    let seen = google.clone();
    let handler = move |method: Method, uri: Uri, headers: HeaderMap, body: String| {
        let seen = seen.clone();
        async move {
            assert_eq!(method, Method::POST);
            let mut google = seen.lock().unwrap();
            if uri.path() == "/token" {
                google.token_requests.push(body);
                let n = google.token_requests.len();
                return axum::Json(serde_json::json!({
                    "access_token": format!("stand-in-{n}"),
                    "expires_in": 3599,
                    "token_type": "Bearer",
                }))
                .into_response();
            }
            assert_eq!(
                uri.path(),
                "/v1/projects/manage-our-home-test/messages:send"
            );
            let authorization = headers["authorization"].to_str().unwrap().to_string();
            let body: serde_json::Value = serde_json::from_str(&body).unwrap();
            let token = body["message"]["token"].as_str().unwrap().to_string();
            google.sends.push((authorization, body));
            if std::mem::take(&mut google.refuse_next) {
                return (StatusCode::UNAUTHORIZED, "{}").into_response();
            }
            if token == GONE_PHONE {
                return (
                    StatusCode::NOT_FOUND,
                    r#"{"error":{"code":404,"status":"NOT_FOUND","details":[{"@type":"type.googleapis.com/google.firebase.fcm.v1.FcmError","errorCode":"UNREGISTERED"}]}}"#,
                )
                    .into_response();
            }
            axum::Json(serde_json::json!({ "name": "projects/manage-our-home-test/messages/1" }))
                .into_response()
        }
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().fallback(handler))
            .await
            .unwrap();
    });
    (url, google)
}

/// The real sender: one access token for every push while it is fresh,
/// fetched with the service account's assertion; a data message naming
/// the kind and the entries, nothing else; `UNREGISTERED` forgets the
/// phone, a delivery stamps the other.
#[sqlx::test]
async fn unregistered_forgets_the_phone_and_one_token_serves_every_push(db: PgPool) {
    let (url, google) = google_stand_in().await;
    let sender = fcm::sender(Arc::new(common::test_fcm_sender(&url)));
    let member = insert_user(&db, "phones@example.test", "push").await;
    add_device(&db, member, PHONE, "fcm").await;
    add_device(&db, member, GONE_PHONE, "fcm").await;
    let entries = entries_of(&[member]);

    push_alerts(&db, None::<&common::NoWeb>, Some(&sender), &entries)
        .await
        .unwrap();
    assert_eq!(
        device(&db, GONE_PHONE).await,
        None,
        "UNREGISTERED forgets it"
    );
    assert_eq!(device(&db, PHONE).await, Some((true, 0, 0)));

    push_changes(&db, None::<&common::NoWeb>, Some(&sender), &entries)
        .await
        .unwrap();

    let google = google.lock().unwrap();
    assert_eq!(google.token_requests.len(), 1, "one token for every push");
    let form = &google.token_requests[0];
    assert!(
        form.starts_with(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion="
        ),
        "{form}"
    );
    assert_eq!(
        form.rsplit('=').next().unwrap().split('.').count(),
        3,
        "a JWT"
    );

    assert_eq!(google.sends.len(), 3, "two phones, then the one left");
    for (authorization, _) in &google.sends {
        assert_eq!(authorization, "Bearer stand-in-1");
    }
    let (_, last) = google.sends.last().unwrap();
    assert_eq!(
        last,
        &serde_json::json!({
            "message": {
                "token": PHONE,
                "data": { "kind": "agenda_change", "entries": entries[0].id.to_string() },
                "android": { "priority": "HIGH", "ttl": format!("{CHANGE_PUSH_TTL_SECS}s") },
            }
        })
    );
}

/// A 401 drops the access token: the push fails, the next one fetches a
/// new token.
#[sqlx::test]
async fn a_refused_access_token_is_fetched_again(db: PgPool) {
    let (url, google) = google_stand_in().await;
    let sender = fcm::sender(Arc::new(common::test_fcm_sender(&url)));
    let member = insert_user(&db, "refused@example.test", "push").await;
    add_device(&db, member, PHONE, "fcm").await;

    google.lock().unwrap().refuse_next = true;
    push_alerts(
        &db,
        None::<&common::NoWeb>,
        Some(&sender),
        &entries_of(&[member]),
    )
    .await
    .unwrap();
    assert_eq!(device(&db, PHONE).await, Some((false, 1, 0)));

    push_alerts(
        &db,
        None::<&common::NoWeb>,
        Some(&sender),
        &entries_of(&[member]),
    )
    .await
    .unwrap();
    assert_eq!(device(&db, PHONE).await, Some((true, 0, 0)));
    let google = google.lock().unwrap();
    assert_eq!(google.token_requests.len(), 2);
    let used: Vec<&str> = google.sends.iter().map(|(a, _)| a.as_str()).collect();
    assert_eq!(used, ["Bearer stand-in-1", "Bearer stand-in-2"]);
}
