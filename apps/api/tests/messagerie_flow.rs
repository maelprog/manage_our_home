mod common;

use axum::http::{Method, StatusCode};
use common::{assert_status, call, json_body, set_cookie, test_router, test_state};
use futures::StreamExt;
use sqlx::PgPool;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

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

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Opens a WS connection to a group's message stream, asserting the
/// upgrade handshake succeeds (101).
async fn connect_ws(addr: std::net::SocketAddr, group_id: &str, cookie: &str) -> WsStream {
    let ws_url = format!("ws://{addr}/groups/{group_id}/messages/ws");
    let mut request = ws_url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert(axum::http::header::COOKIE, cookie.parse().unwrap());
    let (ws_stream, response) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    ws_stream
}

/// Waits (bounded) for the next text frame and parses it as a JSON event.
async fn next_ws_event(ws_stream: &mut WsStream) -> serde_json::Value {
    let received = tokio::time::timeout(Duration::from_secs(5), ws_stream.next())
        .await
        .expect("timed out waiting for WS event")
        .expect("stream ended")
        .unwrap();
    let WsMessage::Text(text) = received else {
        panic!("expected a text frame, got {received:?}");
    };
    serde_json::from_str(&text).unwrap()
}

async fn invite_and_join(
    router: &axum::Router,
    group_id: &str,
    owner_cookie: &str,
    member_cookie: &str,
) {
    let invite = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(owner_cookie),
        Some(serde_json::json!({})),
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
        Some(member_cookie),
        None,
    )
    .await;
    assert_status(&accept, StatusCode::OK);
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

/// AC #1-4: full create/list/update/delete cycle, with the content coming
/// back decrypted and the raw column unreadable without the key.
#[sqlx::test]
async fn full_message_lifecycle(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "msg-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/messages"),
        Some(&owner_cookie),
        Some(serde_json::json!({"content": "Bonjour tout le monde"})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let message = json_body(create).await;
    let message_id = message["id"].as_str().unwrap().to_string();
    assert_eq!(message["content"], "Bonjour tout le monde");
    assert!(message["edited_at"].is_null());

    // The raw column is encrypted — a plain-text lookup must find nothing.
    let raw_match: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM messages WHERE content::text LIKE '%Bonjour%' LIMIT 1")
            .fetch_optional(&db)
            .await
            .unwrap();
    assert!(raw_match.is_none());

    let update = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/messages/{message_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"content": "Bonjour, edited"})),
    )
    .await;
    assert_status(&update, StatusCode::OK);
    let updated = json_body(update).await;
    assert_eq!(updated["content"], "Bonjour, edited");
    assert!(!updated["edited_at"].is_null());

    let list = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/messages"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&list, StatusCode::OK);
    let list_body = json_body(list).await;
    assert_eq!(list_body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(list_body["has_more"], false);

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/messages/{message_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::NO_CONTENT);

    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM messages")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
}

/// AC #2: cursor pagination, newest first, `has_more` correctly reflects
/// whether another page exists (decision #7).
#[sqlx::test]
async fn cursor_pagination_orders_newest_first_and_reports_has_more(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "msg-page-owner@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    for i in 0..5 {
        let res = call(
            &router,
            Method::POST,
            &format!("/groups/{group_id}/messages"),
            Some(&owner_cookie),
            Some(serde_json::json!({"content": format!("message {i}")})),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
    }

    let first_page = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/messages?limit=2"),
        Some(&owner_cookie),
        None,
    )
    .await;
    let first_body = json_body(first_page).await;
    let first_messages = first_body["messages"].as_array().unwrap();
    assert_eq!(first_messages.len(), 2);
    assert_eq!(first_body["has_more"], true);
    assert_eq!(first_messages[0]["content"], "message 4");
    assert_eq!(first_messages[1]["content"], "message 3");

    let cursor_created_at = first_messages[1]["created_at"].as_str().unwrap();
    let cursor_id = first_messages[1]["id"].as_str().unwrap();

    let second_page = call(
        &router,
        Method::GET,
        &format!(
            "/groups/{group_id}/messages?limit=2&before_created_at={cursor_created_at}&before_id={cursor_id}"
        ),
        Some(&owner_cookie),
        None,
    )
    .await;
    let second_body = json_body(second_page).await;
    let second_messages = second_body["messages"].as_array().unwrap();
    assert_eq!(second_messages.len(), 2);
    assert_eq!(second_messages[0]["content"], "message 2");
    assert_eq!(second_messages[1]["content"], "message 1");
}

/// AC #3, #4: the author can edit/delete their own message; an owner/admin
/// can edit/delete another member's; a non-author standard member gets 403.
#[sqlx::test]
async fn only_author_or_admin_can_modify(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "msg-perm-owner@example.test",
        "owner-password1",
    )
    .await;
    let member_cookie = register_verify_login(
        &router,
        &db,
        "msg-perm-member@example.test",
        "member-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let group_uuid: uuid::Uuid = group_id.parse().unwrap();
    let token = sqlx::query_scalar!(
        "SELECT token FROM invitations WHERE group_id = $1",
        group_uuid
    )
    .fetch_optional(&db)
    .await
    .unwrap();
    if token.is_none() {
        let invite = call(
            &router,
            Method::POST,
            &format!("/groups/{group_id}/invitations"),
            Some(&owner_cookie),
            Some(serde_json::json!({})),
        )
        .await;
        assert_status(&invite, StatusCode::CREATED);
        let invite_token = json_body(invite).await["token"]
            .as_str()
            .unwrap()
            .to_string();
        let accept = call(
            &router,
            Method::POST,
            &format!("/groups/invitations/{invite_token}/accept"),
            Some(&member_cookie),
            None,
        )
        .await;
        assert_status(&accept, StatusCode::OK);
    }

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/messages"),
        Some(&member_cookie),
        Some(serde_json::json!({"content": "member's message"})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let message_id = json_body(create).await["id"].as_str().unwrap().to_string();

    // Owner (not author) may edit/delete the member's message.
    let owner_edit = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/messages/{message_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"content": "edited by owner"})),
    )
    .await;
    assert_status(&owner_edit, StatusCode::OK);

    let create2 = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/messages"),
        Some(&owner_cookie),
        Some(serde_json::json!({"content": "owner's message"})),
    )
    .await;
    let owner_message_id = json_body(create2).await["id"].as_str().unwrap().to_string();

    // Non-author standard member cannot edit/delete the owner's message.
    let member_edit_forbidden = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/messages/{owner_message_id}"),
        Some(&member_cookie),
        Some(serde_json::json!({"content": "hijacked"})),
    )
    .await;
    assert_status(&member_edit_forbidden, StatusCode::FORBIDDEN);

    let member_delete_forbidden = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/messages/{owner_message_id}"),
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&member_delete_forbidden, StatusCode::FORBIDDEN);
}

/// AC #5: a member of family A can neither read nor write family B's
/// thread, even knowing a valid message id (RLS + application scoping).
#[sqlx::test]
async fn cannot_access_another_familys_messages(db: PgPool) {
    let router = test_router(db.clone());
    let owner_a =
        register_verify_login(&router, &db, "msg-family-a@example.test", "owner-password1").await;
    let owner_b =
        register_verify_login(&router, &db, "msg-family-b@example.test", "owner-password1").await;
    let group_a = create_group(&router, &owner_a, "Famille A").await;
    let _group_b = create_group(&router, &owner_b, "Famille B").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_a}/messages"),
        Some(&owner_a),
        Some(serde_json::json!({"content": "secret A message"})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);

    // B isn't a member of A's group, so require_role rejects with 403
    // before any RLS-scoped query even runs.
    let list_as_b = call(
        &router,
        Method::GET,
        &format!("/groups/{group_a}/messages"),
        Some(&owner_b),
        None,
    )
    .await;
    assert_status(&list_as_b, StatusCode::FORBIDDEN);

    let create_as_b = call(
        &router,
        Method::POST,
        &format!("/groups/{group_a}/messages"),
        Some(&owner_b),
        Some(serde_json::json!({"content": "intrusion"})),
    )
    .await;
    assert_status(&create_as_b, StatusCode::FORBIDDEN);
}

/// AC #8: exactly 4000 chars is accepted (boundary); 4001 is rejected
/// with 400, before any encryption/write.
#[sqlx::test]
async fn content_length_boundary(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "msg-boundary-owner@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let ok_content = "a".repeat(4000);
    let ok_res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/messages"),
        Some(&owner_cookie),
        Some(serde_json::json!({"content": ok_content})),
    )
    .await;
    assert_status(&ok_res, StatusCode::CREATED);

    let too_long_content = "a".repeat(4001);
    let too_long_res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/messages"),
        Some(&owner_cookie),
        Some(serde_json::json!({"content": too_long_content})),
    )
    .await;
    assert_status(&too_long_res, StatusCode::BAD_REQUEST);

    let empty_res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/messages"),
        Some(&owner_cookie),
        Some(serde_json::json!({"content": "   "})),
    )
    .await;
    assert_status(&empty_res, StatusCode::BAD_REQUEST);

    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM messages")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(remaining, 1);
}

/// Edit/delete race on the same message: whichever request's UPDATE/
/// DELETE ends up matching zero rows because the other already committed
/// first must see a plain 404, not a panic (per how `AppError::Sqlx`/
/// `NotFound` is already handled elsewhere). `FOR UPDATE` makes the
/// winner/loser order timing-dependent under real concurrency, so this
/// pins one concrete ordering (delete-before-edit) deterministically,
/// which is the case that actually needs the 404 path exercised — the
/// reverse order (edit-before-delete) can't 404 at all, since editing
/// never removes the row the delete is still looking for.
#[sqlx::test]
async fn concurrent_edit_and_delete_do_not_panic(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "msg-race-owner@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/messages"),
        Some(&owner_cookie),
        Some(serde_json::json!({"content": "will race"})),
    )
    .await;
    let message_id = json_body(create).await["id"].as_str().unwrap().to_string();
    let message_uri = format!("/groups/{group_id}/messages/{message_id}");

    let delete_res = call(
        &router,
        Method::DELETE,
        &message_uri,
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete_res, StatusCode::NO_CONTENT);

    // The message is already gone by the time this "losing" PATCH runs —
    // no panic, just a 404 via the WHERE clause matching zero rows.
    let edit_res = call(
        &router,
        Method::PATCH,
        &message_uri,
        Some(&owner_cookie),
        Some(serde_json::json!({"content": "edited after delete"})),
    )
    .await;
    assert_status(&edit_res, StatusCode::NOT_FOUND);

    // A second, concurrent delete of the same (already-deleted) message
    // must also 404 rather than panic.
    let second_delete_res = call(
        &router,
        Method::DELETE,
        &message_uri,
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&second_delete_res, StatusCode::NOT_FOUND);

    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM messages")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
}

/// AC #6: a WS-connected client receives `message.created` in real time
/// when another member posts via REST. Spins up a real TCP listener since
/// tower's `oneshot` test harness doesn't support WS upgrades.
#[sqlx::test]
async fn ws_client_receives_message_created_event(db: PgPool) {
    // Both routers must share one `AppState` (and so one `MessageHub`
    // instance, via its inner `Arc`) — otherwise the REST call below
    // publishes into a hub the WS connection never subscribed to.
    let state = test_state(db.clone());
    let ws_router = manage_our_home::build_router(state.clone());
    let http_router = manage_our_home::build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, ws_router).await.unwrap();
    });

    let owner_cookie = register_verify_login(
        &http_router,
        &db,
        "msg-ws-owner@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&http_router, &owner_cookie, "Foyer").await;

    let ws_url = format!("ws://{addr}/groups/{group_id}/messages/ws");
    let mut request = ws_url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert(axum::http::header::COOKIE, owner_cookie.parse().unwrap());

    let (mut ws_stream, response) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);

    // Give the server task time to register the subscription before the
    // REST call publishes, since subscribe() happens asynchronously after
    // upgrade.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let create = call(
        &http_router,
        Method::POST,
        &format!("/groups/{group_id}/messages"),
        Some(&owner_cookie),
        Some(serde_json::json!({"content": "live push"})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);

    let received = tokio::time::timeout(Duration::from_secs(5), ws_stream.next())
        .await
        .expect("timed out waiting for WS event")
        .expect("stream ended")
        .unwrap();

    let WsMessage::Text(text) = received else {
        panic!("expected a text frame, got {received:?}");
    };
    let payload: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(payload["type"], "message.created");
    assert_eq!(payload["message"]["content"], "live push");

    ws_stream.close(None).await.ok();
}

/// Handshake gate: a user who was never a member of the group is rejected
/// with a plain HTTP 403 at upgrade time, before any socket opens — the
/// same `require_role` bar as the REST handlers. (AC #7 — closing an
/// already-open connection after the member is removed — is covered by
/// `removed_member_ws_closes_within_recheck_bound` below.)
#[sqlx::test]
async fn non_member_cannot_open_ws(db: PgPool) {
    let state = test_state(db.clone());
    let router = manage_our_home::build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router.clone()).await.unwrap();
    });

    let http_router = common::test_router(db.clone());
    let owner_cookie = register_verify_login(
        &http_router,
        &db,
        "msg-ws-nonmember-owner@example.test",
        "owner-password1",
    )
    .await;
    let outsider_cookie = register_verify_login(
        &http_router,
        &db,
        "msg-ws-outsider@example.test",
        "outsider-password1",
    )
    .await;
    let group_id = create_group(&http_router, &owner_cookie, "Foyer").await;

    let ws_url = format!("ws://{addr}/groups/{group_id}/messages/ws");
    let mut request = ws_url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert(axum::http::header::COOKIE, outsider_cookie.parse().unwrap());

    let err = tokio_tungstenite::connect_async(request).await.unwrap_err();
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        other => panic!("expected an HTTP 403 rejection, got {other:?}"),
    }
}

/// AC #6 / decision #8: edits and deletions are pushed over WS like new
/// messages — a connected client receives `message.updated` (full message,
/// `edited_at` set) and `message.deleted` (id only) in order.
#[sqlx::test]
async fn ws_client_receives_updated_and_deleted_events(db: PgPool) {
    let state = test_state(db.clone());
    let ws_router = manage_our_home::build_router(state.clone());
    let http_router = manage_our_home::build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, ws_router).await.unwrap();
    });

    let owner_cookie = register_verify_login(
        &http_router,
        &db,
        "msg-ws-upd-owner@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&http_router, &owner_cookie, "Foyer").await;

    let mut ws_stream = connect_ws(addr, &group_id, &owner_cookie).await;
    // Same registration delay as ws_client_receives_message_created_event:
    // subscribe() happens asynchronously after the upgrade.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let create = call(
        &http_router,
        Method::POST,
        &format!("/groups/{group_id}/messages"),
        Some(&owner_cookie),
        Some(serde_json::json!({"content": "original"})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let message_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let created = next_ws_event(&mut ws_stream).await;
    assert_eq!(created["type"], "message.created");

    let update = call(
        &http_router,
        Method::PATCH,
        &format!("/groups/{group_id}/messages/{message_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"content": "edited live"})),
    )
    .await;
    assert_status(&update, StatusCode::OK);

    let updated = next_ws_event(&mut ws_stream).await;
    assert_eq!(updated["type"], "message.updated");
    assert_eq!(updated["message"]["id"].as_str().unwrap(), message_id);
    assert_eq!(updated["message"]["content"], "edited live");
    assert!(!updated["message"]["edited_at"].is_null());

    let delete = call(
        &http_router,
        Method::DELETE,
        &format!("/groups/{group_id}/messages/{message_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::NO_CONTENT);

    let deleted = next_ws_event(&mut ws_stream).await;
    assert_eq!(deleted["type"], "message.deleted");
    assert_eq!(deleted["id"].as_str().unwrap(), message_id);

    ws_stream.close(None).await.ok();
}

/// AC #6 (isolation half): a client connected to family B's thread never
/// receives family A's events — each family gets its own broadcast channel
/// keyed by `group_id`, so A's publish can't reach B's subscription at
/// all. Posting a sentinel into B afterwards and asserting it's the FIRST
/// frame B sees proves both non-receipt and that the socket was live the
/// whole time (a dead socket would pass a pure non-receipt check too).
#[sqlx::test]
async fn ws_client_does_not_receive_other_familys_events(db: PgPool) {
    let state = test_state(db.clone());
    let ws_router = manage_our_home::build_router(state.clone());
    let http_router = manage_our_home::build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, ws_router).await.unwrap();
    });

    let owner_a = register_verify_login(
        &http_router,
        &db,
        "msg-ws-iso-a@example.test",
        "owner-password1",
    )
    .await;
    let owner_b = register_verify_login(
        &http_router,
        &db,
        "msg-ws-iso-b@example.test",
        "owner-password1",
    )
    .await;
    let group_a = create_group(&http_router, &owner_a, "Famille A").await;
    let group_b = create_group(&http_router, &owner_b, "Famille B").await;

    let mut ws_b = connect_ws(addr, &group_b, &owner_b).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Family A posts first; if isolation were broken, this event would
    // reach B's socket ahead of the sentinel below.
    let create_a = call(
        &http_router,
        Method::POST,
        &format!("/groups/{group_a}/messages"),
        Some(&owner_a),
        Some(serde_json::json!({"content": "family A secret"})),
    )
    .await;
    assert_status(&create_a, StatusCode::CREATED);

    let create_b = call(
        &http_router,
        Method::POST,
        &format!("/groups/{group_b}/messages"),
        Some(&owner_b),
        Some(serde_json::json!({"content": "family B sentinel"})),
    )
    .await;
    assert_status(&create_b, StatusCode::CREATED);

    let first = next_ws_event(&mut ws_b).await;
    assert_eq!(first["type"], "message.created");
    assert_eq!(
        first["message"]["content"], "family B sentinel",
        "family B's socket received family A's event"
    );
    assert_eq!(first["message"]["group_id"].as_str().unwrap(), group_b);

    ws_b.close(None).await.ok();
}

/// AC #7: a member removed from the group (`DELETE /groups/:id/members/
/// :user_id`) has their open WS connection closed within the membership
/// recheck bound — 30s in production, shortened here through
/// `AppState::message_ws_recheck_interval` so the test doesn't sleep out
/// the real interval — while the remaining member's connection stays open.
#[sqlx::test]
async fn removed_member_ws_closes_within_recheck_bound(db: PgPool) {
    let mut state = test_state(db.clone());
    state.message_ws_recheck_interval = Duration::from_millis(200);
    let ws_router = manage_our_home::build_router(state.clone());
    let http_router = manage_our_home::build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, ws_router).await.unwrap();
    });

    let owner_cookie = register_verify_login(
        &http_router,
        &db,
        "msg-ws-removal-owner@example.test",
        "owner-password1",
    )
    .await;
    let member_cookie = register_verify_login(
        &http_router,
        &db,
        "msg-ws-removal-member@example.test",
        "member-password1",
    )
    .await;
    let group_id = create_group(&http_router, &owner_cookie, "Foyer").await;
    invite_and_join(&http_router, &group_id, &owner_cookie, &member_cookie).await;

    let member_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind("msg-ws-removal-member@example.test")
        .fetch_one(&db)
        .await
        .unwrap();

    let mut owner_ws = connect_ws(addr, &group_id, &owner_cookie).await;
    let mut member_ws = connect_ws(addr, &group_id, &member_cookie).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let remove = call(
        &http_router,
        Method::DELETE,
        &format!("/groups/{group_id}/members/{member_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&remove, StatusCode::NO_CONTENT);

    // The removed member's socket must be closed by the next recheck tick.
    // 5s of budget >> the 200ms interval, without being sleep-sensitive.
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match member_ws.next().await {
                Some(Ok(WsMessage::Close(_))) | None => break,
                Some(Ok(_)) => continue, // drain any in-flight frame
                Some(Err(_)) => break,
            }
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "removed member's WS was not closed within the recheck bound"
    );

    // The remaining member's connection survived the other's removal and
    // still receives pushes.
    let create = call(
        &http_router,
        Method::POST,
        &format!("/groups/{group_id}/messages"),
        Some(&owner_cookie),
        Some(serde_json::json!({"content": "still here"})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);

    let event = next_ws_event(&mut owner_ws).await;
    assert_eq!(event["type"], "message.created");
    assert_eq!(event["message"]["content"], "still here");

    owner_ws.close(None).await.ok();
}

// -- read state / unread (#73, blockers found by #98's round-2 verification) --
//
// The dashboard's "Messages non lus" card is built entirely on
// `POST /groups/:id/messages/read` and `GET …/messages?unread=true`, and
// neither had a flow test: both blockers of round 2 lived in exactly this
// surface. These cover the contract the web layer relies on
// (`apps/web/src/routes/messagerie/thread.rs::read_watermark`).

/// Posts a message and returns `(id, created_at)`.
async fn post_message(
    router: &axum::Router,
    group_id: &str,
    cookie: &str,
    content: &str,
) -> (String, String) {
    let res = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/messages"),
        Some(cookie),
        Some(serde_json::json!({ "content": content })),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let body = json_body(res).await;
    (
        body["id"].as_str().unwrap().to_string(),
        body["created_at"].as_str().unwrap().to_string(),
    )
}

/// `GET …/messages?unread=true`, returning the contents in the order the
/// API sent them plus `(has_more, unread_total)`.
async fn unread(
    router: &axum::Router,
    group_id: &str,
    cookie: &str,
    limit: Option<i64>,
) -> (Vec<String>, bool, i64) {
    let mut path = format!("/groups/{group_id}/messages?unread=true");
    if let Some(limit) = limit {
        path.push_str(&format!("&limit={limit}"));
    }
    let res = call(router, Method::GET, &path, Some(cookie), None).await;
    assert_status(&res, StatusCode::OK);
    let body = json_body(res).await;
    let contents = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["content"].as_str().unwrap().to_string())
        .collect();
    (
        contents,
        body["has_more"].as_bool().unwrap(),
        body["unread_total"].as_i64().unwrap(),
    )
}

async fn mark_read(router: &axum::Router, group_id: &str, cookie: &str, up_to: Option<&str>) {
    let path = match up_to {
        Some(t) => format!("/groups/{group_id}/messages/read?up_to={t}"),
        None => format!("/groups/{group_id}/messages/read"),
    };
    let res = call(router, Method::POST, &path, Some(cookie), None).await;
    assert_status(&res, StatusCode::NO_CONTENT);
}

/// The nominal cycle: everything is unread until the marker is set, then
/// only what arrived after it.
#[sqlx::test]
async fn unread_lists_only_what_arrived_after_the_read_marker(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(
        &router,
        &db,
        "msg-read-owner@example.test",
        "owner-password1",
    )
    .await;
    let member = register_verify_login(
        &router,
        &db,
        "msg-read-member@example.test",
        "member-password1",
    )
    .await;
    let group_id = create_group(&router, &owner, "Foyer lecture").await;
    invite_and_join(&router, &group_id, &owner, &member).await;

    post_message(&router, &group_id, &member, "un").await;
    let (_, second_at) = post_message(&router, &group_id, &member, "deux").await;

    // Never read anything: both are unread.
    let (contents, has_more, total) = unread(&router, &group_id, &owner, None).await;
    assert_eq!(contents.len(), 2, "{contents:?}");
    assert!(!has_more);
    assert_eq!(total, 2);

    mark_read(&router, &group_id, &owner, Some(&second_at)).await;
    let (contents, _, total) = unread(&router, &group_id, &owner, None).await;
    assert!(contents.is_empty(), "{contents:?}");
    assert_eq!(total, 0);

    // A newer message is unread again — and only for the reader who marked.
    post_message(&router, &group_id, &member, "trois").await;
    let (contents, _, total) = unread(&router, &group_id, &owner, None).await;
    assert_eq!(contents, vec!["trois".to_string()]);
    assert_eq!(total, 1);

    // The member never marked anything read, so all three are unread for them.
    let (contents, _, total) = unread(&router, &group_id, &member, None).await;
    assert_eq!(contents.len(), 3, "{contents:?}");
    assert_eq!(total, 3);
}

/// Blocker B1 as a backend contract: marking read "up to" an older message
/// must leave everything newer unread. This is what lets
/// `apps/web`'s thread mark only as far as the page it actually rendered,
/// instead of up to `now()` — anything posted between the listing and the
/// mark had never been shown, and `0012_message_read_state.sql` has no way
/// to make a message unread again.
#[sqlx::test]
async fn marking_read_up_to_an_older_message_leaves_the_newer_ones_unread(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(
        &router,
        &db,
        "msg-part-owner@example.test",
        "owner-password1",
    )
    .await;
    let member = register_verify_login(
        &router,
        &db,
        "msg-part-member@example.test",
        "member-password1",
    )
    .await;
    let group_id = create_group(&router, &owner, "Foyer partiel").await;
    invite_and_join(&router, &group_id, &owner, &member).await;

    let (_, first_at) = post_message(&router, &group_id, &member, "vu").await;
    post_message(&router, &group_id, &member, "jamais affiché A").await;
    post_message(&router, &group_id, &member, "jamais affiché B").await;

    mark_read(&router, &group_id, &owner, Some(&first_at)).await;

    let (contents, _, total) = unread(&router, &group_id, &owner, None).await;
    assert_eq!(total, 2, "{contents:?}");
    assert!(
        contents.contains(&"jamais affiché A".to_string()),
        "{contents:?}"
    );
    assert!(
        contents.contains(&"jamais affiché B".to_string()),
        "{contents:?}"
    );
    assert!(!contents.contains(&"vu".to_string()), "{contents:?}");
}

/// The marker only moves forward: a late or stale request carrying an older
/// `up_to` cannot resurrect messages as unread (which the dashboard would
/// then re-announce), and replaying the same call is a no-op.
#[sqlx::test]
async fn the_read_marker_never_moves_backwards(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(
        &router,
        &db,
        "msg-mono-owner@example.test",
        "owner-password1",
    )
    .await;
    let member = register_verify_login(
        &router,
        &db,
        "msg-mono-member@example.test",
        "member-password1",
    )
    .await;
    let group_id = create_group(&router, &owner, "Foyer monotone").await;
    invite_and_join(&router, &group_id, &owner, &member).await;

    let (_, first_at) = post_message(&router, &group_id, &member, "un").await;
    let (_, second_at) = post_message(&router, &group_id, &member, "deux").await;

    mark_read(&router, &group_id, &owner, Some(&second_at)).await;
    mark_read(&router, &group_id, &owner, Some(&first_at)).await;
    mark_read(&router, &group_id, &owner, Some(&second_at)).await;

    let (contents, _, total) = unread(&router, &group_id, &owner, None).await;
    assert!(contents.is_empty(), "{contents:?}");
    assert_eq!(total, 0);
}

/// `up_to` is clamped to `now()`: a caller cannot mark messages that do not
/// exist yet as read, which would otherwise silence the card for good.
#[sqlx::test]
async fn a_future_up_to_cannot_mark_messages_that_do_not_exist_yet(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(
        &router,
        &db,
        "msg-fut-owner@example.test",
        "owner-password1",
    )
    .await;
    let member = register_verify_login(
        &router,
        &db,
        "msg-fut-member@example.test",
        "member-password1",
    )
    .await;
    let group_id = create_group(&router, &owner, "Foyer futur").await;
    invite_and_join(&router, &group_id, &owner, &member).await;

    mark_read(
        &router,
        &group_id,
        &owner,
        Some("2099-01-01T00:00:00.000000Z"),
    )
    .await;
    post_message(&router, &group_id, &member, "après").await;

    let (contents, _, total) = unread(&router, &group_id, &owner, None).await;
    assert_eq!(contents, vec!["après".to_string()]);
    assert_eq!(total, 1);
}

/// Mi3: the unread page is capped by `limit`, so it has to say how much it
/// is not showing — the dashboard renders "+N autre(s)" from `unread_total`
/// and `has_more`. Both used to be `false`/absent, and a member with eight
/// unread messages saw five with no hint of the rest.
#[sqlx::test]
async fn a_capped_unread_page_reports_how_many_are_left(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(
        &router,
        &db,
        "msg-cap-owner@example.test",
        "owner-password1",
    )
    .await;
    let member = register_verify_login(
        &router,
        &db,
        "msg-cap-member@example.test",
        "member-password1",
    )
    .await;
    let group_id = create_group(&router, &owner, "Foyer plafonné").await;
    invite_and_join(&router, &group_id, &owner, &member).await;

    for i in 0..8 {
        post_message(&router, &group_id, &member, &format!("message {i}")).await;
    }

    let (contents, has_more, total) = unread(&router, &group_id, &owner, Some(5)).await;
    assert_eq!(contents.len(), 5, "{contents:?}");
    assert!(has_more, "five of eight unread is not the whole set");
    assert_eq!(total, 8);

    // Newest first, same order as a normal listing.
    assert_eq!(contents[0], "message 7");

    let (contents, has_more, total) = unread(&router, &group_id, &owner, Some(50)).await;
    assert_eq!(contents.len(), 8);
    assert!(!has_more);
    assert_eq!(total, 8);
}

/// The read marker is per (family, member) and RLS-scoped: marking read in
/// one family says nothing about another.
#[sqlx::test]
async fn the_read_marker_is_scoped_to_one_family(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(
        &router,
        &db,
        "msg-scope-owner@example.test",
        "owner-password1",
    )
    .await;
    let member = register_verify_login(
        &router,
        &db,
        "msg-scope-member@example.test",
        "member-password1",
    )
    .await;
    let first = create_group(&router, &owner, "Foyer un").await;
    let second = create_group(&router, &owner, "Foyer deux").await;
    invite_and_join(&router, &first, &owner, &member).await;
    invite_and_join(&router, &second, &owner, &member).await;

    let (_, first_at) = post_message(&router, &first, &member, "chez un").await;
    post_message(&router, &second, &member, "chez deux").await;

    mark_read(&router, &first, &owner, Some(&first_at)).await;

    assert_eq!(unread(&router, &first, &owner, None).await.2, 0);
    assert_eq!(unread(&router, &second, &owner, None).await.2, 1);
}

/// A non-member cannot advance a read marker in a family they don't belong
/// to — same `require_role` bar as every other endpoint on the thread.
#[sqlx::test]
async fn a_non_member_cannot_mark_a_family_thread_read(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(
        &router,
        &db,
        "msg-out-owner@example.test",
        "owner-password1",
    )
    .await;
    let outsider = register_verify_login(
        &router,
        &db,
        "msg-out-other@example.test",
        "other-password1",
    )
    .await;
    let group_id = create_group(&router, &owner, "Foyer fermé").await;

    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/messages/read"),
        Some(&outsider),
        None,
    )
    .await;
    assert_status(&res, StatusCode::FORBIDDEN);

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM message_read_state")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}
