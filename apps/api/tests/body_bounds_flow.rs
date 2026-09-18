//! Bounds on request bodies (#219): how long a client may take to send
//! one, on every route, and how many uploads are read at once.
//!
//! The timing tests run on a router whose `BodyReadLimits` are shortened
//! to fractions of a second; the arithmetic at production values is
//! covered by `manage_our_home_http_guard`'s own tests.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, Method, Request, Response, StatusCode};
use bytes::Bytes;
use common::{
    assert_status, call, call_upload, json_body, real_minio_from_env, set_cookie, test_state,
};
use manage_our_home::{build_router, AppState};
use manage_our_home_http_guard::{BodyReadLimits, UploadGate};
use sqlx::PgPool;
use tokio::sync::mpsc;
use tower::ServiceExt;

const BOUNDARY: &str = "----manageourhomeboundsboundary";

/// Short enough for a test, in the same proportions as production: grace
/// under idle, idle well under total.
const SHORT: BodyReadLimits = BodyReadLimits {
    idle: Duration::from_millis(400),
    min_bytes_per_sec: 1_000,
    grace: Duration::from_millis(300),
    total: Duration::from_secs(3),
};

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
    let token: uuid::Uuid = sqlx::query_scalar(
        "SELECT token FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = $1",
    )
    .bind(email)
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

/// A member with a group and an event in it: the cookie and the event's
/// attachments URL.
async fn uploader(router: &axum::Router, db: &PgPool, email: &str) -> (String, String) {
    let cookie = register_verify_login(router, db, email, "bounds-password1").await;
    let group = call(
        router,
        Method::POST,
        "/groups",
        Some(&cookie),
        Some(serde_json::json!({"name": "Foyer"})),
    )
    .await;
    assert_status(&group, StatusCode::CREATED);
    let group_id = json_body(group).await["id"].as_str().unwrap().to_string();
    let starts_at = chrono::Utc::now() + chrono::Duration::days(1);
    let event = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&cookie),
        Some(serde_json::json!({
            "title": "Rendez-vous",
            "starts_at": starts_at,
            "ends_at": starts_at + chrono::Duration::hours(1),
        })),
    )
    .await;
    assert_status(&event, StatusCode::CREATED);
    let event_id = json_body(event).await["id"].as_str().unwrap().to_string();
    (
        cookie,
        format!("/groups/{group_id}/events/{event_id}/attachments"),
    )
}

/// A request body the test feeds by hand: it ends when the sender drops.
fn fed_body() -> (mpsc::Sender<Bytes>, Body) {
    let (tx, rx) = mpsc::channel::<Bytes>(16);
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|chunk| (Ok::<_, std::io::Error>(chunk), rx))
    });
    (tx, Body::from_stream(stream))
}

fn multipart_head() -> Bytes {
    Bytes::from(format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"scan.png\"\r\n\r\n"
    ))
}

fn upload_request(uri: &str, cookie: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::COOKIE, cookie)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(body)
        .unwrap()
}

/// Sends `chunk` every `every` until the receiving side is gone.
fn drip(tx: mpsc::Sender<Bytes>, chunk: &'static [u8], every: Duration) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            if tx.send(Bytes::from_static(chunk)).await.is_err() {
                return;
            }
        }
    });
}

fn state_with(db: PgPool, limits: BodyReadLimits, gate: Arc<UploadGate<uuid::Uuid>>) -> AppState {
    let mut state = test_state(db);
    state.body_read_limits = limits;
    state.upload_gate = gate;
    state
}

async fn wait_for_in_flight(gate: &UploadGate<uuid::Uuid>, n: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while gate.in_flight() != n {
        assert!(
            Instant::now() < deadline,
            "expected {n} uploads in flight, still {}",
            gate.in_flight()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Bounded, so a body the router fails to cut fails the test instead of
/// hanging it.
async fn answer(router: &axum::Router, request: Request<Body>) -> Response<Body> {
    tokio::time::timeout(Duration::from_secs(10), router.clone().oneshot(request))
        .await
        .expect("no answer within 10 s")
        .unwrap()
}

fn assert_request_timeout(resp: &Response<Body>) {
    assert_status(resp, StatusCode::REQUEST_TIMEOUT);
    assert_eq!(
        resp.headers()
            .get(header::CONNECTION)
            .map(|v| v.to_str().unwrap()),
        Some("close"),
        "a 408 must close the connection: the rest of the body was never read"
    );
}

// ---------------------------------------------------------------------------
// Time bounds
// ---------------------------------------------------------------------------

/// A drip that never falls silent long enough for `idle`, but stays far
/// under the minimum rate, is cut once grace ends — long before `total` —
/// and gives its upload permit back.
#[sqlx::test]
async fn an_upload_dripped_under_the_minimum_rate_is_answered_408(db: PgPool) {
    let gate = UploadGate::new(8, 2);
    let router = build_router(state_with(db.clone(), SHORT, gate.clone()));
    let (cookie, uri) = uploader(&router, &db, "drip@example.test").await;

    let (tx, body) = fed_body();
    tx.send(multipart_head()).await.unwrap();
    // 10 bytes every 50 ms: 200 B/s, never 400 ms of silence.
    drip(tx, b"0123456789", Duration::from_millis(50));

    let started = Instant::now();
    let resp = answer(&router, upload_request(&uri, &cookie, body)).await;
    let took = started.elapsed();

    assert_request_timeout(&resp);
    assert_eq!(json_body(resp).await["error"], "request_timeout");
    assert!(
        took < SHORT.total,
        "cut after {took:?}, not before the total"
    );
    assert_eq!(gate.in_flight(), 0, "the permit must be back once answered");
}

/// A body that starts generously and then goes quiet is cut at `idle`.
#[sqlx::test]
async fn an_upload_that_falls_silent_is_answered_408(db: PgPool) {
    let router = build_router(state_with(db.clone(), SHORT, UploadGate::new(8, 2)));
    let (cookie, uri) = uploader(&router, &db, "silent@example.test").await;

    let (tx, body) = fed_body();
    tx.send(multipart_head()).await.unwrap();
    // 4 kB covers the rate for 4 s, past the total: only `idle` can cut it.
    tx.send(Bytes::from(vec![b'x'; 4_000])).await.unwrap();

    let resp = answer(&router, upload_request(&uri, &cookie, body)).await;
    drop(tx);
    assert_request_timeout(&resp);
}

/// Every route is bounded, not only the uploads: a JSON body dripped
/// under the rate gets the same 408, where `Json`'s own rejection would
/// have said 400.
#[sqlx::test]
async fn a_json_body_dripped_is_answered_408(db: PgPool) {
    let router = build_router(state_with(db.clone(), SHORT, UploadGate::new(8, 2)));
    let cookie = register_verify_login(&router, &db, "json@example.test", "bounds-password1").await;

    let (tx, body) = fed_body();
    tx.send(Bytes::from_static(b"{\"name\": \"")).await.unwrap();
    drip(tx, b"a", Duration::from_millis(50));

    let request = Request::builder()
        .method(Method::POST)
        .uri("/groups")
        .header(header::COOKIE, &cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .unwrap();
    let resp = answer(&router, request).await;
    assert_request_timeout(&resp);
}

/// The bounds do not get in the way of an upload sent at a normal pace.
/// Needs a real MinIO; skipped otherwise (see `real_minio_from_env`).
#[sqlx::test]
async fn an_upload_at_a_normal_pace_is_created_under_the_bounds(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping an_upload_at_a_normal_pace_is_created_under_the_bounds: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let gate = UploadGate::new(8, 2);
    let mut state = state_with(db.clone(), SHORT, gate.clone());
    state.storage = manage_our_home::storage::Storage::new(s3, bucket);
    let router = build_router(state);
    let (cookie, uri) = uploader(&router, &db, "normal@example.test").await;

    // A PNG sent in five chunks, 50 ms apart: 2 kB each, 40 kB/s.
    let (tx, body) = fed_body();
    tokio::spawn(async move {
        tx.send(multipart_head()).await.unwrap();
        let mut png = vec![0u8; 10_000];
        png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        for chunk in png.chunks(2_000) {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send(Bytes::copy_from_slice(chunk)).await.unwrap();
        }
        tx.send(Bytes::from(format!("\r\n--{BOUNDARY}--\r\n")))
            .await
            .unwrap();
    });

    let resp = answer(&router, upload_request(&uri, &cookie, body)).await;
    assert_status(&resp, StatusCode::CREATED);
    assert_eq!(gate.in_flight(), 0);
}

// ---------------------------------------------------------------------------
// Upload permits
// ---------------------------------------------------------------------------

/// Starts an upload whose body never finishes and waits until it holds
/// its permit. The returned task ends when the sender is dropped, or is
/// aborted to stand for a client that disconnects.
async fn hold_upload(
    router: &axum::Router,
    gate: &UploadGate<uuid::Uuid>,
    uri: &str,
    cookie: &str,
) -> (mpsc::Sender<Bytes>, tokio::task::JoinHandle<Response<Body>>) {
    let before = gate.in_flight();
    let (tx, body) = fed_body();
    tx.send(multipart_head()).await.unwrap();
    let request = upload_request(uri, cookie, body);
    let router = router.clone();
    let task = tokio::spawn(async move { router.oneshot(request).await.unwrap() });
    wait_for_in_flight(gate, before + 1).await;
    (tx, task)
}

/// A body `upload_attachment` reads whole and refuses on its content
/// (422): proof the request got past the gate, with no storage involved.
async fn upload_past_the_gate(router: &axum::Router, uri: &str, cookie: &str) -> Response<Body> {
    call_upload(
        router,
        uri,
        cookie,
        "notes.txt",
        b"plain text, not an allowed type",
    )
    .await
}

/// One account at its two uploads is turned away at the third, with
/// `Retry-After`, while another account still gets through.
#[sqlx::test]
async fn a_third_upload_from_one_account_is_turned_away_and_others_are_not(db: PgPool) {
    let gate = UploadGate::new(8, 2);
    let router = build_router(state_with(
        db.clone(),
        BodyReadLimits::PRODUCTION,
        gate.clone(),
    ));
    let (a_cookie, a_uri) = uploader(&router, &db, "busy-a@example.test").await;
    let (b_cookie, b_uri) = uploader(&router, &db, "busy-b@example.test").await;

    let _first = hold_upload(&router, &gate, &a_uri, &a_cookie).await;
    let _second = hold_upload(&router, &gate, &a_uri, &a_cookie).await;

    let third = upload_past_the_gate(&router, &a_uri, &a_cookie).await;
    assert_status(&third, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(third.headers()[header::RETRY_AFTER], "30");
    assert_eq!(json_body(third).await["error"], "uploads_busy");

    let other = upload_past_the_gate(&router, &b_uri, &b_cookie).await;
    assert_status(&other, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(gate.in_flight(), 2);
}

/// With the process pool at N, the N+1th upload is turned away; a client
/// that disconnects mid-body gives its permit back.
#[sqlx::test]
async fn the_upload_past_the_process_pool_is_turned_away_until_a_client_disconnects(db: PgPool) {
    let gate = UploadGate::new(2, 2);
    let router = build_router(state_with(
        db.clone(),
        BodyReadLimits::PRODUCTION,
        gate.clone(),
    ));
    let (a_cookie, a_uri) = uploader(&router, &db, "pool-a@example.test").await;
    let (b_cookie, b_uri) = uploader(&router, &db, "pool-b@example.test").await;
    let (c_cookie, c_uri) = uploader(&router, &db, "pool-c@example.test").await;

    let (_a_tx, a_task) = hold_upload(&router, &gate, &a_uri, &a_cookie).await;
    let _b = hold_upload(&router, &gate, &b_uri, &b_cookie).await;

    let refused = upload_past_the_gate(&router, &c_uri, &c_cookie).await;
    assert_status(&refused, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(refused.headers()[header::RETRY_AFTER], "30");

    // The client behind A goes away: hyper drops the request's future.
    a_task.abort();
    let _ = a_task.await;
    wait_for_in_flight(&gate, 1).await;

    let admitted = upload_past_the_gate(&router, &c_uri, &c_cookie).await;
    assert_status(&admitted, StatusCode::UNPROCESSABLE_ENTITY);
}
