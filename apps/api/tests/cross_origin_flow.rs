//! Cross-origin requests refused on the real router (#223). The decision
//! itself is covered by `manage_our_home_http_guard::origin`'s own tests;
//! these check that every unsafe route and the Messagerie WebSocket go
//! through it, and that apps/web's calls — which carry no browser header —
//! are untouched.
//!
//! `common::test_state` sets `frontend_base_url` to
//! `http://localhost:5173`: that is the trusted origin here.

mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, Response, StatusCode};
use common::{assert_status, call, json_body, set_cookie, test_router, test_state};
use sqlx::PgPool;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;

const FRONTEND: &str = "http://localhost:5173";

/// `call`, with browser headers on top.
async fn call_from(
    router: &axum::Router,
    method: Method,
    uri: &str,
    cookie: Option<&str>,
    body: Option<serde_json::Value>,
    browser: &[(&str, &str)],
) -> Response<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "localhost:8080");
    if let Some(c) = cookie {
        builder = builder.header(header::COOKIE, c);
    }
    for (k, v) in browser {
        builder = builder.header(*k, *v);
    }
    let body = match body {
        Some(v) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&v).unwrap())
        }
        None => Body::empty(),
    };
    router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap()
}

fn registration(email: &str) -> serde_json::Value {
    serde_json::json!({
        "email": email,
        "password": "cross-origin-password1",
        "display_name": email,
        "declares_minimum_age": true, "accepts_terms": true,
    })
}

async fn users_named(db: &PgPool, email: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(db)
        .await
        .unwrap()
}

/// Registered, verified and logged in through plain calls (apps/web's
/// kind): the session cookie.
async fn member(router: &axum::Router, db: &PgPool, email: &str) -> String {
    let reg = call(
        router,
        Method::POST,
        "/auth/register",
        None,
        Some(registration(email)),
    )
    .await;
    assert_status(&reg, StatusCode::CREATED);
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
        Some(serde_json::json!({"email": email, "password": "cross-origin-password1"})),
    )
    .await;
    assert_status(&login, StatusCode::OK);
    set_cookie(&login).unwrap()
}

#[sqlx::test]
async fn a_cross_site_login_is_refused(db: PgPool) {
    let router = test_router(db.clone());
    member(&router, &db, "victim-target@example.test").await;

    for browser in [
        &[
            ("sec-fetch-site", "cross-site"),
            ("origin", "https://evil.test"),
        ][..],
        &[
            ("sec-fetch-site", "same-site"),
            ("origin", "http://evil.localhost:5173"),
        ][..],
        &[("origin", "https://evil.test")][..],
    ] {
        let resp = call_from(
            &router,
            Method::POST,
            "/auth/login",
            None,
            Some(serde_json::json!({
                "email": "victim-target@example.test",
                "password": "cross-origin-password1",
            })),
            browser,
        )
        .await;
        assert_status(&resp, StatusCode::FORBIDDEN);
        assert!(set_cookie(&resp).is_none(), "{browser:?} got a session");
        assert_eq!(json_body(resp).await["error"], "cross_origin_request");
    }
}

#[sqlx::test]
async fn a_cross_site_register_creates_nothing(db: PgPool) {
    let router = test_router(db.clone());
    let resp = call_from(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(registration("forged@example.test")),
        &[
            ("sec-fetch-site", "cross-site"),
            ("origin", "https://evil.test"),
        ],
    )
    .await;
    assert_status(&resp, StatusCode::FORBIDDEN);
    assert_eq!(users_named(&db, "forged@example.test").await, 0);
}

#[sqlx::test]
async fn a_same_origin_post_goes_through(db: PgPool) {
    let router = test_router(db.clone());
    for (email, browser) in [
        (
            "same-origin@example.test",
            &[("sec-fetch-site", "same-origin")][..],
        ),
        // The frontend's own origin, on another port (CI's layout).
        (
            "frontend@example.test",
            &[("sec-fetch-site", "same-site"), ("origin", FRONTEND)][..],
        ),
        // A browser without Fetch Metadata, through Caddy: one host.
        (
            "old-browser@example.test",
            &[("origin", "http://localhost:8080")][..],
        ),
    ] {
        let resp = call_from(
            &router,
            Method::POST,
            "/auth/register",
            None,
            Some(registration(email)),
            browser,
        )
        .await;
        assert_status(&resp, StatusCode::CREATED);
        assert_eq!(users_named(&db, email).await, 1, "{browser:?}");
    }
}

/// The routes a form cannot reach with a JSON body were the ones exposed:
/// a body-less `POST` needs no preflight.
#[sqlx::test]
async fn a_cross_site_body_less_post_is_refused(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = member(&router, &db, "logout-target@example.test").await;

    let forged = call_from(
        &router,
        Method::POST,
        "/auth/logout",
        Some(&cookie),
        None,
        &[
            ("sec-fetch-site", "cross-site"),
            ("origin", "https://evil.test"),
        ],
    )
    .await;
    assert_status(&forged, StatusCode::FORBIDDEN);

    // The session survived it.
    let me = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&me, StatusCode::OK);
}

/// What apps/web sends over the internal network: no `Origin`, no
/// `Sec-Fetch-Site`. Every other flow test is of this kind too.
#[sqlx::test]
async fn a_server_to_server_call_goes_through(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = member(&router, &db, "s2s@example.test").await;
    let resp = call_from(
        &router,
        Method::POST,
        "/groups",
        Some(&cookie),
        Some(serde_json::json!({"name": "Foyer"})),
        &[],
    )
    .await;
    assert_status(&resp, StatusCode::CREATED);
}

async fn ws_handshake(
    addr: std::net::SocketAddr,
    group_id: &str,
    cookie: &str,
    origin: &str,
) -> StatusCode {
    let mut request = format!("ws://{addr}/groups/{group_id}/messages/ws")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    request
        .headers_mut()
        .insert(header::ORIGIN, origin.parse().unwrap());
    match tokio_tungstenite::connect_async(request).await {
        Ok((mut ws, response)) => {
            ws.close(None).await.ok();
            response.status()
        }
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => response.status(),
        Err(other) => panic!("handshake failed outside HTTP: {other:?}"),
    }
}

/// Cross-Site WebSocket Hijacking: a member's cookie, sent by a page of
/// another origin, must not open the family's thread.
#[sqlx::test]
async fn a_websocket_from_a_foreign_origin_is_refused(db: PgPool) {
    let router = manage_our_home::build_router(test_state(db.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = router.clone();
    tokio::spawn(async move { axum::serve(listener, served).await.unwrap() });

    let cookie = member(&router, &db, "ws-member@example.test").await;
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

    // tungstenite sends no Sec-Fetch-Site: the Origin is judged against
    // the trusted one, then against Host.
    assert_eq!(
        ws_handshake(addr, &group_id, &cookie, "https://evil.test").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        ws_handshake(addr, &group_id, &cookie, "http://chat.localhost:5173").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        ws_handshake(addr, &group_id, &cookie, FRONTEND).await,
        StatusCode::SWITCHING_PROTOCOLS
    );
}
