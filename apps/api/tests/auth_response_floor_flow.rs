//! The response-time floor of `POST /auth/register`,
//! `/auth/password/forgot` and `/auth/verify-email/resend` (#571): mail or
//! no mail, none of them answers before the floor, and neither a relay that
//! never answers nor a queue on the "account exists" notice cap makes one
//! answer much after it.

mod common;

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Response, StatusCode};
use common::{assert_status, call};
use lettre::{AsyncSmtpTransport, Tokio1Executor};
use manage_our_home::auth::response_floor::ResponseFloor;
use manage_our_home::email::{EmailSender, Outbox};
use sqlx::PgPool;

/// The floor of these tests, far below the production default so the suite
/// stays quick, and far above what the branches take without a relay.
const FLOOR: Duration = Duration::from_millis(700);

/// The deadlines of these tests, a fraction of [`FLOOR`] as in production.
const SMTP_TIMEOUT: Duration = Duration::from_millis(250);
const NOTICE_ADMISSION_TIMEOUT: Duration = Duration::from_millis(100);

/// How far past the floor an answer may land when a deadline fires: the
/// deadlines leave the work of the branch room under the floor, so only
/// the scheduling of the test runtime is left, with a wide allowance for a
/// loaded CI runner.
const MARGIN: Duration = Duration::from_millis(400);

/// A bound no answer of these routes comes near. A send that is not cut
/// short would hang the test on a relay that never answers; this turns the
/// hang into a failure.
const HANG: Duration = Duration::from_secs(20);

fn floor() -> ResponseFloor {
    ResponseFloor {
        floor: FLOOR,
        smtp_timeout: SMTP_TIMEOUT,
        notice_admission_timeout: NOTICE_ADMISSION_TIMEOUT,
    }
}

/// A router with the floor of these tests and its emails kept in an
/// [`Outbox`].
fn router_with_outbox(db: PgPool) -> (axum::Router, Outbox) {
    let mut state = common::test_state(db);
    let (email, outbox) = EmailSender::in_memory("noreply@example.test".parse().unwrap());
    state.email = email;
    state.auth_response_floor = floor();
    (manage_our_home::build_router(state), outbox)
}

/// A relay that accepts the connection and then says nothing, not even its
/// greeting: lettre's own timeout bounds the connection only, so without a
/// deadline on the whole send the request would wait on it for ever.
async fn silent_relay() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    (port, task)
}

fn router_with_relay(db: PgPool, port: u16) -> axum::Router {
    let mut state = common::test_state(db);
    let transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous("127.0.0.1")
        .port(port)
        .build();
    state.email = EmailSender::new(transport, "noreply@example.test".parse().unwrap());
    state.auth_response_floor = floor();
    manage_our_home::build_router(state)
}

fn registration(email: &str) -> serde_json::Value {
    serde_json::json!({
        "email": email,
        "password": "floor-password1",
        "display_name": "Floor",
        "declares_minimum_age": true,
        "accepts_terms": true,
    })
}

/// Calls the route and returns its answer, its body and how long it took.
async fn timed(
    router: &axum::Router,
    path: &str,
    body: serde_json::Value,
) -> (Response<Body>, Vec<u8>, Duration) {
    let started = Instant::now();
    let response = tokio::time::timeout(HANG, call(router, Method::POST, path, None, Some(body)))
        .await
        .unwrap_or_else(|_| panic!("{path} still had not answered after {HANG:?}"));
    let elapsed = started.elapsed();
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (Response::from_parts(parts, Body::empty()), bytes, elapsed)
}

fn assert_at_least_floor(what: &str, elapsed: Duration) {
    assert!(
        elapsed >= FLOOR,
        "{what} answered in {elapsed:?}, before the {FLOOR:?} floor"
    );
}

fn assert_within_margin(what: &str, elapsed: Duration) {
    assert_at_least_floor(what, elapsed);
    assert!(
        elapsed <= FLOOR + MARGIN,
        "{what} answered in {elapsed:?}, more than {MARGIN:?} past the {FLOOR:?} floor"
    );
}

async fn insert_user(db: &PgPool, email: &str, verified: bool, deactivated: bool) {
    sqlx::query(
        "INSERT INTO users (email, password_hash, display_name, email_verified, age_declared_at,
                            deactivated_at)
         VALUES ($1, 'unused', 'Floor', $2, now(), CASE WHEN $3 THEN now() END)",
    )
    .bind(email)
    .bind(verified)
    .bind(deactivated)
    .execute(db)
    .await
    .unwrap();
}

fn mails_to(outbox: &Outbox, email: &str) -> usize {
    outbox.emails().iter().filter(|e| e.to == email).count()
}

/// `register` answers no sooner than the floor whether it mails a link (a
/// new address), the "account exists" notice (a verified one) or nothing at
/// all (a deactivated one).
#[sqlx::test]
async fn register_waits_for_the_floor_mail_or_no_mail(db: PgPool) {
    let (router, outbox) = router_with_outbox(db.clone());
    insert_user(&db, "verified@example.test", true, false).await;
    insert_user(&db, "deactivated@example.test", true, true).await;

    for (email, mails) in [
        ("new@example.test", 1),
        ("verified@example.test", 1),
        ("deactivated@example.test", 0),
    ] {
        let (response, body, elapsed) = timed(&router, "/auth/register", registration(email)).await;
        assert_status(&response, StatusCode::CREATED);
        assert!(body.is_empty());
        assert_eq!(mails_to(&outbox, email), mails, "{email}");
        assert_at_least_floor(email, elapsed);
    }
}

/// A request whose fields are invalid says nothing about any account: its
/// 422 is not held back.
#[sqlx::test]
async fn an_invalid_registration_is_refused_at_once(db: PgPool) {
    let (router, _) = router_with_outbox(db);
    let mut body = registration("new@example.test");
    body["accepts_terms"] = serde_json::json!(false);
    let (response, _, elapsed) = timed(&router, "/auth/register", body).await;
    assert_status(&response, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(elapsed < FLOOR, "the 422 waited {elapsed:?}");
}

/// `forgot_password` answers no sooner than the floor, for an account it
/// mails and for an address it knows nothing about.
#[sqlx::test]
async fn forgot_password_waits_for_the_floor_mail_or_no_mail(db: PgPool) {
    let (router, outbox) = router_with_outbox(db.clone());
    insert_user(&db, "known@example.test", true, false).await;

    for (email, mails) in [("known@example.test", 1), ("unknown@example.test", 0)] {
        let (response, body, elapsed) = timed(
            &router,
            "/auth/password/forgot",
            serde_json::json!({ "email": email }),
        )
        .await;
        assert_status(&response, StatusCode::OK);
        assert!(body.is_empty());
        assert_eq!(mails_to(&outbox, email), mails, "{email}");
        assert_at_least_floor(email, elapsed);
    }
}

/// `resend_verification` answers no sooner than the floor, for an
/// unverified account it mails and for an address it knows nothing about.
#[sqlx::test]
async fn resend_verification_waits_for_the_floor_mail_or_no_mail(db: PgPool) {
    let (router, outbox) = router_with_outbox(db.clone());
    insert_user(&db, "unverified@example.test", false, false).await;

    for (email, mails) in [("unverified@example.test", 1), ("unknown@example.test", 0)] {
        let (response, body, elapsed) = timed(
            &router,
            "/auth/verify-email/resend",
            serde_json::json!({ "email": email }),
        )
        .await;
        assert_status(&response, StatusCode::OK);
        assert!(body.is_empty());
        assert_eq!(mails_to(&outbox, email), mails, "{email}");
        assert_at_least_floor(email, elapsed);
    }
}

/// A relay that never answers is given up on within the SMTP deadline: the
/// answer lands within the margin of the floor, and is the one an unknown
/// address gets.
#[sqlx::test]
async fn a_relay_that_never_answers_does_not_push_the_answer_past_the_floor(db: PgPool) {
    let (port, relay) = silent_relay().await;
    let router = router_with_relay(db.clone(), port);
    insert_user(&db, "known@example.test", true, false).await;
    insert_user(&db, "unverified@example.test", false, false).await;

    for (path, known, status) in [
        (
            "/auth/password/forgot",
            "known@example.test",
            StatusCode::OK,
        ),
        (
            "/auth/verify-email/resend",
            "unverified@example.test",
            StatusCode::OK,
        ),
    ] {
        let (sent, sent_body, sent_elapsed) =
            timed(&router, path, serde_json::json!({ "email": known })).await;
        let (unknown, unknown_body, unknown_elapsed) = timed(
            &router,
            path,
            serde_json::json!({ "email": "unknown@example.test" }),
        )
        .await;
        assert_status(&sent, status);
        assert_status(&unknown, status);
        assert_eq!(sent_body, unknown_body, "{path}");
        assert_eq!(
            format!("{:?}", sent.headers()),
            format!("{:?}", unknown.headers()),
            "{path}"
        );
        assert_within_margin(&format!("{path} with a silent relay"), sent_elapsed);
        assert_within_margin(&format!("{path} for an unknown address"), unknown_elapsed);
    }
    relay.abort();
}

/// The same for `register`, on both of its branches that mail: the
/// verification link of a new address and the "account exists" notice of
/// a verified one. Each answers within the margin of the floor, and as a
/// deactivated account, which is mailed nothing, is answered.
#[sqlx::test]
async fn a_relay_that_never_answers_does_not_push_a_registration_past_the_floor(db: PgPool) {
    let (port, relay) = silent_relay().await;
    let router = router_with_relay(db.clone(), port);
    insert_user(&db, "verified@example.test", true, false).await;
    insert_user(&db, "deactivated@example.test", true, true).await;

    let (nothing, nothing_body, nothing_elapsed) = timed(
        &router,
        "/auth/register",
        registration("deactivated@example.test"),
    )
    .await;
    assert_status(&nothing, StatusCode::CREATED);
    assert_within_margin("a registration that mails nothing", nothing_elapsed);

    for email in ["new@example.test", "verified@example.test"] {
        let (sent, sent_body, sent_elapsed) =
            timed(&router, "/auth/register", registration(email)).await;
        assert_status(&sent, StatusCode::CREATED);
        assert_eq!(sent_body, nothing_body, "{email}");
        assert_eq!(
            format!("{:?}", sent.headers()),
            format!("{:?}", nothing.headers()),
            "{email}"
        );
        assert_within_margin(&format!("{email} with a silent relay"), sent_elapsed);
    }
    relay.abort();
}

/// The "account exists" notice cap decides one address after the other on
/// a pool of one connection (#570): a burst on one address queues the
/// notices of the others. A notice that cannot be admitted within its
/// deadline is not sent, and the registration answers within the margin of
/// the floor, as for a new address.
#[sqlx::test]
async fn a_queue_on_the_notice_cap_does_not_push_the_answer_past_the_floor(db: PgPool) {
    let (router, outbox) = router_with_outbox(db.clone());
    let email = "queued@example.test";
    insert_user(&db, email, true, false).await;

    // Every admission reads `rate_limit_log`: with the table locked, the
    // admission waits as it would behind a queue of others.
    let mut lock = manage_our_home::db::begin(&db).await.unwrap();
    sqlx::query("LOCK TABLE rate_limit_log IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *lock)
        .await
        .unwrap();

    let (response, body, elapsed) = timed(&router, "/auth/register", registration(email)).await;
    lock.rollback().await.unwrap();

    assert_status(&response, StatusCode::CREATED);
    assert!(body.is_empty());
    assert_eq!(mails_to(&outbox, email), 0);
    assert_within_margin("a registration behind a queued notice", elapsed);
}
