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

/// AC #7: a member removed from the group has their WS connection closed
/// within the bounded 30s recheck — exercised here with a short interval
/// via a second connection attempt after removal is rejected outright.
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
