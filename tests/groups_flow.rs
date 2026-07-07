mod common;

use axum::http::{Method, StatusCode};
use common::{assert_status, call, json_body, set_cookie, test_router};
use sqlx::PgPool;
use uuid::Uuid;

async fn register_verify_login(router: &axum::Router, db: &PgPool, email: &str, password: &str) -> String {
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
    call(router, Method::GET, &format!("/auth/verify-email?token={token}"), None, None).await;
    let login = call(router, Method::POST, "/auth/login", None, Some(serde_json::json!({"email": email, "password": password}))).await;
    set_cookie(&login).unwrap()
}

/// AC #9-#14: create -> invite -> accept -> role change -> leave.
#[sqlx::test]
async fn full_group_lifecycle(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let member_cookie = register_verify_login(&router, &db, "member@example.test", "member-password1").await;

    let create = call(&router, Method::POST, "/groups", Some(&owner_cookie), Some(serde_json::json!({"name": "Foyer"}))).await;
    assert_status(&create, StatusCode::CREATED);
    let group = json_body(create).await;
    let group_id = group["id"].as_str().unwrap().to_string();

    let invite = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(&owner_cookie),
        Some(serde_json::json!({"invited_email": "member@example.test"})),
    )
    .await;
    assert_status(&invite, StatusCode::CREATED);
    let invite_body = json_body(invite).await;
    let invite_token = invite_body["token"].as_str().unwrap().to_string();

    let accept = call(
        &router,
        Method::POST,
        &format!("/groups/invitations/{invite_token}/accept"),
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&accept, StatusCode::OK);

    // AC #14: re-using the same invitation token is rejected as Gone.
    let reuse = call(
        &router,
        Method::POST,
        &format!("/groups/invitations/{invite_token}/accept"),
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&reuse, StatusCode::GONE);

    let member_id: Uuid = sqlx::query_scalar!("SELECT id FROM users WHERE email = 'member@example.test'")
        .fetch_one(&db)
        .await
        .unwrap();

    let promote = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/members/{member_id}/role"),
        Some(&owner_cookie),
        Some(serde_json::json!({"role": "admin"})),
    )
    .await;
    assert_status(&promote, StatusCode::OK);

    // AC #13: the newly promoted admin cannot touch the owner.
    let owner_id: Uuid = sqlx::query_scalar!("SELECT id FROM users WHERE email = 'owner@example.test'")
        .fetch_one(&db)
        .await
        .unwrap();
    let admin_attacks_owner = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/members/{owner_id}/role"),
        Some(&member_cookie),
        Some(serde_json::json!({"role": "standard"})),
    )
    .await;
    assert_status(&admin_attacks_owner, StatusCode::FORBIDDEN);

    // AC #11: owner cannot leave without naming a successor.
    let leave_without_successor = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/leave"),
        Some(&owner_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&leave_without_successor, StatusCode::UNPROCESSABLE_ENTITY);

    let leave_with_successor = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/leave"),
        Some(&owner_cookie),
        Some(serde_json::json!({"new_owner_id": member_id})),
    )
    .await;
    assert_status(&leave_with_successor, StatusCode::OK);

    // AC #12: the last remaining member must delete the group, not leave.
    let last_member_leaves = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/leave"),
        Some(&member_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&last_member_leaves, StatusCode::CONFLICT);
}

/// AC #9: creating an 11th group is rejected.
#[sqlx::test]
async fn group_creation_capped_at_ten(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "prolific@example.test", "prolific-password1").await;

    for i in 0..10 {
        let res = call(&router, Method::POST, "/groups", Some(&cookie), Some(serde_json::json!({"name": format!("Group {i}")}))).await;
        assert_status(&res, StatusCode::CREATED);
    }
    let eleventh = call(&router, Method::POST, "/groups", Some(&cookie), Some(serde_json::json!({"name": "Group 11"}))).await;
    assert_status(&eleventh, StatusCode::UNPROCESSABLE_ENTITY);
}

/// AC #10: the DB itself enforces a single owner per group.
#[sqlx::test]
async fn only_one_owner_per_group_at_db_level(db: PgPool) {
    let user_id: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('single@example.test', 'x', 'Single', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let other_id: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('single2@example.test', 'x', 'Single2', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_id: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('G', $1) RETURNING id",
        user_id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    sqlx::query!("INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'owner')", group_id, user_id)
        .execute(&db)
        .await
        .unwrap();

    let result = sqlx::query!(
        "INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'owner')",
        group_id,
        other_id
    )
    .execute(&db)
    .await;
    assert!(result.is_err(), "expected unique violation for a second owner");
}
