//! The orchestrator's probes and the stop on SIGTERM (#424).

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::{assert_status, call, json_body, set_cookie, test_state};
use futures::StreamExt;
use manage_our_home::build_router;
use manage_our_home_http_guard::shutdown::serve;
use sqlx::PgPool;
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[sqlx::test]
async fn both_probes_answer_200_with_postgres_up(db: PgPool) {
    let router = build_router(test_state(db));
    let healthz = call(&router, Method::GET, "/healthz", None, None).await;
    assert_status(&healthz, StatusCode::OK);
    let readyz = call(&router, Method::GET, "/readyz", None, None).await;
    assert_status(&readyz, StatusCode::OK);
}

#[sqlx::test]
async fn readyz_is_503_with_postgres_unreachable(db: PgPool) {
    let mut state = test_state(db);
    // Nothing listens on port 1: every connection attempt is refused.
    state.db = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_secs(1))
        .connect_lazy("postgres://nobody:nothing@127.0.0.1:1/nothing")
        .unwrap();
    let router = build_router(state);
    let readyz = call(&router, Method::GET, "/readyz", None, None).await;
    assert_status(&readyz, StatusCode::SERVICE_UNAVAILABLE);
    // The process itself is fine.
    let healthz = call(&router, Method::GET, "/healthz", None, None).await;
    assert_status(&healthz, StatusCode::OK);
}

#[sqlx::test]
async fn readyz_is_503_once_draining_and_healthz_stays_200(db: PgPool) {
    let state = test_state(db);
    state.shutdown.trigger();
    let router = build_router(state);
    let readyz = call(&router, Method::GET, "/readyz", None, None).await;
    assert_status(&readyz, StatusCode::SERVICE_UNAVAILABLE);
    let healthz = call(&router, Method::GET, "/healthz", None, None).await;
    assert_status(&healthz, StatusCode::OK);
}

#[sqlx::test]
async fn probes_carry_no_cookie_and_are_never_limited(db: PgPool) {
    let router = build_router(test_state(db));
    for _ in 0..50 {
        for path in ["/healthz", "/readyz"] {
            let res = call(&router, Method::GET, path, None, None).await;
            assert_status(&res, StatusCode::OK);
            assert!(set_cookie(&res).is_none(), "{path} set a cookie");
        }
    }
}

/// Serves `router` the way `main.rs` does, stopping when the returned
/// sender fires (in place of SIGTERM).
async fn start(
    router: axum::Router,
    shutdown: manage_our_home_http_guard::Shutdown,
    grace: Duration,
) -> (
    SocketAddr,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(serve(
        listener,
        router,
        shutdown,
        async move {
            let _ = stopped.await;
        },
        grace,
    ));
    (addr, stop, server)
}

#[sqlx::test]
async fn a_request_in_flight_at_the_stop_is_answered(db: PgPool) {
    let state = test_state(db);
    let shutdown = state.shutdown.clone();
    let router = build_router(state).route(
        "/slow",
        axum::routing::get(|| async {
            tokio::time::sleep(Duration::from_millis(800)).await;
            "done"
        }),
    );
    let (addr, stop, server) = start(router, shutdown.clone(), Duration::from_secs(10)).await;

    let slow = tokio::spawn(async move {
        reqwest::get(format!("http://{addr}/slow"))
            .await?
            .text()
            .await
    });
    // The request is in the handler before the stop.
    tokio::time::sleep(Duration::from_millis(200)).await;
    stop.send(()).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(shutdown.is_draining());

    // No new connection is accepted.
    assert!(
        tokio::net::TcpStream::connect(addr).await.is_err(),
        "the listener still accepts after the stop"
    );

    // The request in flight finishes, and the server with it.
    assert_eq!(slow.await.unwrap().unwrap(), "done");
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the server outlived its last request")
        .unwrap()
        .unwrap();
}

#[sqlx::test]
async fn an_open_websocket_gets_a_close_frame_at_the_stop(db: PgPool) {
    let state = test_state(db.clone());
    let shutdown = state.shutdown.clone();
    let router = build_router(state);

    // An account and a group to open the socket on.
    let email = "ws-shutdown@example.test";
    let password = "shutdown-password1";
    call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": email, "password": password, "display_name": email, "declares_minimum_age": true, "accepts_terms": true})),
    )
    .await;
    let token = common::verification_token(&db, email).await;
    call(
        &router,
        Method::GET,
        &format!("/auth/verify-email?token={token}"),
        None,
        None,
    )
    .await;
    let login = call(
        &router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": email, "password": password})),
    )
    .await;
    let cookie = set_cookie(&login).unwrap();
    let group = call(
        &router,
        Method::POST,
        "/groups",
        Some(&cookie),
        Some(serde_json::json!({"name": "Foyer"})),
    )
    .await;
    assert_status(&group, StatusCode::CREATED);
    let group_id = json_body(group).await["id"].as_str().unwrap().to_string();

    // A grace period far beyond the test's bounds: the server must end
    // because the socket closed, not because time ran out.
    let (addr, stop, server) = start(router, shutdown.clone(), Duration::from_secs(60)).await;
    let mut request = format!("ws://{addr}/groups/{group_id}/messages/ws")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(axum::http::header::COOKIE, cookie.parse().unwrap());
    let (mut ws, response) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    tokio::time::sleep(Duration::from_millis(100)).await;
    // The open socket holds the process: axum's graceful shutdown does not
    // wait for an upgraded connection, the hold is what keeps the process
    // up until its Close frame is out.
    assert_eq!(shutdown.open_holds(), 1, "the open socket holds nothing");

    stop.send(()).unwrap();

    let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("no frame after the stop")
        .expect("the stream ended without a close frame")
        .unwrap();
    let WsMessage::Close(Some(close)) = frame else {
        panic!("expected a close frame, got {frame:?}");
    };
    assert_eq!(close.code, CloseCode::Away);
    assert_eq!(
        u16::from(close.code),
        manage_our_home::messagerie::ws::CLOSE_GOING_AWAY
    );
    drop(ws);

    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the server outlived its last socket")
        .unwrap()
        .unwrap();
    assert_eq!(shutdown.open_holds(), 0, "the closed socket still holds");
}
