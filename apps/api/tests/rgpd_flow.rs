mod common;

use axum::http::{Method, StatusCode};
use common::{assert_status, call, json_body, set_cookie, test_router};
use sqlx::PgPool;

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

/// AC: `GET /account/export` returns every category of data the requesting
/// user authored, in the requesting user's own group — and the raw
/// `audit_log` gains an `account_data_exported` entry (architecture.md's
/// RGPD security requirement to audit-log export/deletion actions).
#[sqlx::test]
async fn export_returns_owned_data_across_categories(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "export-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &cookie, "Foyer Export").await;

    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&cookie),
        Some(serde_json::json!({"name": "Farine", "quantity": 2.0, "unit": "kg"})),
    )
    .await;

    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&cookie),
        Some(serde_json::json!({"name": "Courses", "amount": 12.5})),
    )
    .await;

    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/messages"),
        Some(&cookie),
        Some(serde_json::json!({"content": "Bonjour la famille"})),
    )
    .await;

    let export = call(&router, Method::GET, "/account/export", Some(&cookie), None).await;
    assert_status(&export, StatusCode::OK);
    let doc = json_body(export).await;

    assert_eq!(doc["profile"]["email"], "export-owner@example.test");
    assert_eq!(doc["group_memberships"][0]["name"], "Foyer Export");
    assert_eq!(doc["group_memberships"][0]["role"], "owner");
    assert_eq!(doc["stock_items"][0]["name"], "Farine");
    assert_eq!(doc["budget_entries"][0]["name"], "Courses");
    assert_eq!(doc["budget_entries"][0]["amount"], 12.5);
    assert_eq!(doc["messages"][0]["content"], "Bonjour la famille");

    let audit_action: Option<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE action = 'account_data_exported' LIMIT 1",
    )
    .fetch_optional(&db)
    .await
    .unwrap();
    assert_eq!(audit_action.as_deref(), Some("account_data_exported"));
}

/// AC: export is strictly self-scoped — a second member's content in the
/// same group must never appear in the first member's export.
#[sqlx::test]
async fn export_never_leaks_another_members_content(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "leak-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer Leak").await;

    let invite = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(&owner_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&invite, StatusCode::CREATED);
    let token = json_body(invite).await["token"]
        .as_str()
        .unwrap()
        .to_string();

    let member_cookie =
        register_verify_login(&router, &db, "leak-member@example.test", "member-password1").await;
    let accept = call(
        &router,
        Method::POST,
        &format!("/groups/invitations/{token}/accept"),
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&accept, StatusCode::OK);

    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/messages"),
        Some(&owner_cookie),
        Some(serde_json::json!({"content": "Secret du owner"})),
    )
    .await;

    let export = call(
        &router,
        Method::GET,
        "/account/export",
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&export, StatusCode::OK);
    let doc = json_body(export).await;
    assert!(doc["messages"].as_array().unwrap().is_empty());
    assert_eq!(doc["group_memberships"][0]["role"], "standard");
}

/// Front epic F10 prerequisite: `GET /auth/me` reports `has_password` and the
/// pending `deletion_requested_at`, so `apps/web` can decide whether the
/// deletion form must ask for the current password and whether to render the
/// grace-period banner + cancel action instead of the request form. Read live on
/// every request, so requesting/cancelling shows up on the next call with no
/// re-login.
#[sqlx::test]
async fn auth_me_reports_password_and_pending_deletion(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "me-rgpd@example.test", "owner-password1").await;

    let before = json_body(call(&router, Method::GET, "/auth/me", Some(&cookie), None).await).await;
    assert_eq!(before["has_password"], true);
    assert_eq!(before["deletion_requested_at"], serde_json::Value::Null);

    let delete = call(
        &router,
        Method::POST,
        "/account/delete",
        Some(&cookie),
        Some(serde_json::json!({"current_password": "owner-password1"})),
    )
    .await;
    assert_status(&delete, StatusCode::OK);

    let after = json_body(call(&router, Method::GET, "/auth/me", Some(&cookie), None).await).await;
    assert!(after["deletion_requested_at"].is_string());

    let cancel = call(
        &router,
        Method::POST,
        "/account/delete/cancel",
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&cancel, StatusCode::OK);

    let cancelled =
        json_body(call(&router, Method::GET, "/auth/me", Some(&cookie), None).await).await;
    assert_eq!(cancelled["deletion_requested_at"], serde_json::Value::Null);
}

/// AC: `/account/export` and `/privacy-policy` reject/serve as documented —
/// export requires a session, the privacy policy is public.
#[sqlx::test]
async fn export_requires_auth_and_privacy_policy_is_public(db: PgPool) {
    let router = test_router(db.clone());

    let unauthenticated = call(&router, Method::GET, "/account/export", None, None).await;
    assert_status(&unauthenticated, StatusCode::UNAUTHORIZED);

    let policy = call(&router, Method::GET, "/privacy-policy", None, None).await;
    assert_status(&policy, StatusCode::OK);
}
