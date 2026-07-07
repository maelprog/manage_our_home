mod common;

use axum::http::{Method, StatusCode};
use common::{assert_status, call, json_body, set_cookie, test_router};
use sqlx::PgPool;
use uuid::Uuid;

/// E2E #1: register -> verify -> login -> create group -> invite ->
/// second account accepts -> transfer ownership -> original owner's
/// account deletion is blocked then unblocked once ownership is gone.
#[sqlx::test]
async fn full_journey_register_to_ownership_transfer_to_deletion(db: PgPool) {
    let router = test_router(db.clone());

    let register = call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": "founder@example.test", "password": "founder-password1", "display_name": "Founder"})),
    )
    .await;
    assert_status(&register, StatusCode::CREATED);

    let verify_token = sqlx::query_scalar!(
        "SELECT token FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = 'founder@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let verify = call(&router, Method::GET, &format!("/auth/verify-email?token={verify_token}"), None, None).await;
    assert_status(&verify, StatusCode::OK);

    let login = call(
        &router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "founder@example.test", "password": "founder-password1"})),
    )
    .await;
    assert_status(&login, StatusCode::OK);
    let founder_cookie = set_cookie(&login).unwrap();

    let create_group = call(&router, Method::POST, "/groups", Some(&founder_cookie), Some(serde_json::json!({"name": "Notre Famille"}))).await;
    assert_status(&create_group, StatusCode::CREATED);
    let group = json_body(create_group).await;
    let group_id = group["id"].as_str().unwrap().to_string();

    let invite = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(&founder_cookie),
        Some(serde_json::json!({"invited_email": "successor@example.test"})),
    )
    .await;
    assert_status(&invite, StatusCode::CREATED);
    let invite_token = json_body(invite).await["token"].as_str().unwrap().to_string();

    // Second account registers, verifies, logs in, and accepts.
    call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": "successor@example.test", "password": "successor-password1", "display_name": "Successor"})),
    )
    .await;
    let successor_verify_token = sqlx::query_scalar!(
        "SELECT token FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = 'successor@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    call(&router, Method::GET, &format!("/auth/verify-email?token={successor_verify_token}"), None, None).await;
    let successor_login = call(
        &router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "successor@example.test", "password": "successor-password1"})),
    )
    .await;
    let successor_cookie = set_cookie(&successor_login).unwrap();

    let accept = call(&router, Method::POST, &format!("/groups/invitations/{invite_token}/accept"), Some(&successor_cookie), None).await;
    assert_status(&accept, StatusCode::OK);

    let successor_id: Uuid = sqlx::query_scalar!("SELECT id FROM users WHERE email = 'successor@example.test'")
        .fetch_one(&db)
        .await
        .unwrap();

    // Founder tries to delete their account while still owner: blocked.
    let blocked = call(
        &router,
        Method::POST,
        "/account/delete",
        Some(&founder_cookie),
        Some(serde_json::json!({"current_password": "founder-password1"})),
    )
    .await;
    assert_status(&blocked, StatusCode::CONFLICT);

    // Transfer ownership by leaving with a named successor.
    let leave = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/leave"),
        Some(&founder_cookie),
        Some(serde_json::json!({"new_owner_id": successor_id})),
    )
    .await;
    assert_status(&leave, StatusCode::OK);

    // Now deletion succeeds.
    let unblocked = call(
        &router,
        Method::POST,
        "/account/delete",
        Some(&founder_cookie),
        Some(serde_json::json!({"current_password": "founder-password1"})),
    )
    .await;
    assert_status(&unblocked, StatusCode::OK);

    let founder_row = sqlx::query!("SELECT deletion_requested_at FROM users WHERE email = 'founder@example.test'")
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(founder_row.deletion_requested_at.is_some());
}

/// E2E #2: sensitive actions along the way (ownership transfer) are
/// recorded in `audit_log` (AC #16).
#[sqlx::test]
async fn sensitive_actions_are_audited(db: PgPool) {
    let router = test_router(db.clone());

    call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": "auditor@example.test", "password": "auditor-password1", "display_name": "Auditor"})),
    )
    .await;
    let token = sqlx::query_scalar!(
        "SELECT token FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = 'auditor@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    call(&router, Method::GET, &format!("/auth/verify-email?token={token}"), None, None).await;
    let login = call(&router, Method::POST, "/auth/login", None, Some(serde_json::json!({"email": "auditor@example.test", "password": "auditor-password1"}))).await;
    let cookie = set_cookie(&login).unwrap();

    call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": "member2@example.test", "password": "member2-password1", "display_name": "Member2"})),
    )
    .await;
    let token2 = sqlx::query_scalar!(
        "SELECT token FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = 'member2@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    call(&router, Method::GET, &format!("/auth/verify-email?token={token2}"), None, None).await;
    let login2 = call(&router, Method::POST, "/auth/login", None, Some(serde_json::json!({"email": "member2@example.test", "password": "member2-password1"}))).await;
    let cookie2 = set_cookie(&login2).unwrap();

    let create_group = call(&router, Method::POST, "/groups", Some(&cookie), Some(serde_json::json!({"name": "Audited"}))).await;
    let group_id = json_body(create_group).await["id"].as_str().unwrap().to_string();

    let invite = call(&router, Method::POST, &format!("/groups/{group_id}/invitations"), Some(&cookie), Some(serde_json::json!({"invited_email": "member2@example.test"}))).await;
    let invite_token = json_body(invite).await["token"].as_str().unwrap().to_string();
    call(&router, Method::POST, &format!("/groups/invitations/{invite_token}/accept"), Some(&cookie2), None).await;

    let member2_id: Uuid = sqlx::query_scalar!("SELECT id FROM users WHERE email = 'member2@example.test'")
        .fetch_one(&db)
        .await
        .unwrap();

    let leave = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/leave"),
        Some(&cookie),
        Some(serde_json::json!({"new_owner_id": member2_id})),
    )
    .await;
    assert_status(&leave, StatusCode::OK);

    let audit_count = sqlx::query_scalar!(
        "SELECT count(*) FROM audit_log WHERE action = 'ownership_transferred'"
    )
    .fetch_one(&db)
    .await
    .unwrap()
    .unwrap_or(0);
    assert_eq!(audit_count, 1);
}
