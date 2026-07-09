mod common;

use axum::http::{Method, StatusCode};
use common::{assert_status, call, json_body, set_cookie, test_router};
use sqlx::PgPool;
use uuid::Uuid;

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

/// AC #9-#14: create -> invite -> accept -> role change -> leave.
#[sqlx::test]
async fn full_group_lifecycle(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let member_cookie =
        register_verify_login(&router, &db, "member@example.test", "member-password1").await;

    let create = call(
        &router,
        Method::POST,
        "/groups",
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Foyer"})),
    )
    .await;
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

    let member_id: Uuid =
        sqlx::query_scalar!("SELECT id FROM users WHERE email = 'member@example.test'")
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
    let owner_id: Uuid =
        sqlx::query_scalar!("SELECT id FROM users WHERE email = 'owner@example.test'")
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

/// GET /groups returns exactly the caller's memberships with roles, is empty
/// for a user in no groups, and never leaks another user's groups.
#[sqlx::test]
async fn list_groups_is_scoped_to_caller(db: PgPool) {
    let router = test_router(db.clone());
    let alice = register_verify_login(&router, &db, "alice@example.test", "alice-password1").await;
    let bob = register_verify_login(&router, &db, "bob@example.test", "bob-password1").await;

    // Bob has no groups yet -> [].
    let empty = call(&router, Method::GET, "/groups", Some(&bob), None).await;
    assert_status(&empty, StatusCode::OK);
    assert_eq!(json_body(empty).await, serde_json::json!([]));

    // Alice creates two groups.
    for name in ["Alpha", "Beta"] {
        let res = call(
            &router,
            Method::POST,
            "/groups",
            Some(&alice),
            Some(serde_json::json!({"name": name})),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
    }

    let alice_list = call(&router, Method::GET, "/groups", Some(&alice), None).await;
    assert_status(&alice_list, StatusCode::OK);
    let list = json_body(alice_list).await;
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    for g in arr {
        assert_eq!(g["role"], "owner");
        assert!(g["group_id"].is_string());
        assert!(g["created_at"].is_string());
    }

    // Bob still sees none of Alice's groups.
    let bob_list = call(&router, Method::GET, "/groups", Some(&bob), None).await;
    assert_eq!(json_body(bob_list).await, serde_json::json!([]));
}

/// PATCH /groups/:id renames as admin/owner, is forbidden for a standard
/// member, and rejects an empty name with 422.
#[sqlx::test]
async fn rename_group_permissions_and_validation(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let member =
        register_verify_login(&router, &db, "member@example.test", "member-password1").await;

    let create = call(
        &router,
        Method::POST,
        "/groups",
        Some(&owner),
        Some(serde_json::json!({"name": "Old Name"})),
    )
    .await;
    let group_id = json_body(create).await["id"].as_str().unwrap().to_string();

    // Add member via invitation.
    let invite = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(&owner),
        Some(serde_json::json!({"invited_email": "member@example.test"})),
    )
    .await;
    let token = json_body(invite).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    call(
        &router,
        Method::POST,
        &format!("/groups/invitations/{token}/accept"),
        Some(&member),
        None,
    )
    .await;

    // Standard member cannot rename.
    let forbidden = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}"),
        Some(&member),
        Some(serde_json::json!({"name": "Hacked"})),
    )
    .await;
    assert_status(&forbidden, StatusCode::FORBIDDEN);

    // Empty-after-trim name is 422.
    let empty = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}"),
        Some(&owner),
        Some(serde_json::json!({"name": "   "})),
    )
    .await;
    assert_status(&empty, StatusCode::UNPROCESSABLE_ENTITY);

    // Owner renames successfully.
    let renamed = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}"),
        Some(&owner),
        Some(serde_json::json!({"name": "  New Name  "})),
    )
    .await;
    assert_status(&renamed, StatusCode::OK);
    assert_eq!(json_body(renamed).await["name"], "New Name");
}

/// POST /groups/:id/transfer-ownership swaps roles atomically with one audit
/// row; rejects non-owner (403), self target (422), and non-member (404).
#[sqlx::test]
async fn transfer_ownership_swaps_roles_and_audits(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let member =
        register_verify_login(&router, &db, "member@example.test", "member-password1").await;
    let _outsider =
        register_verify_login(&router, &db, "outsider@example.test", "outsider-password1").await;

    let create = call(
        &router,
        Method::POST,
        "/groups",
        Some(&owner),
        Some(serde_json::json!({"name": "Foyer"})),
    )
    .await;
    let group_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let invite = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(&owner),
        Some(serde_json::json!({"invited_email": "member@example.test"})),
    )
    .await;
    let token = json_body(invite).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    call(
        &router,
        Method::POST,
        &format!("/groups/invitations/{token}/accept"),
        Some(&member),
        None,
    )
    .await;

    let owner_id: Uuid =
        sqlx::query_scalar!("SELECT id FROM users WHERE email = 'owner@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();
    let member_id: Uuid =
        sqlx::query_scalar!("SELECT id FROM users WHERE email = 'member@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();
    let outsider_id: Uuid =
        sqlx::query_scalar!("SELECT id FROM users WHERE email = 'outsider@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();

    // Non-owner (the member) cannot transfer -> 403.
    let by_member = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/transfer-ownership"),
        Some(&member),
        Some(serde_json::json!({"new_owner_id": owner_id})),
    )
    .await;
    assert_status(&by_member, StatusCode::FORBIDDEN);

    // Owner transferring to itself -> 422.
    let to_self = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/transfer-ownership"),
        Some(&owner),
        Some(serde_json::json!({"new_owner_id": owner_id})),
    )
    .await;
    assert_status(&to_self, StatusCode::UNPROCESSABLE_ENTITY);

    // Owner transferring to a non-member -> 404.
    let to_outsider = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/transfer-ownership"),
        Some(&owner),
        Some(serde_json::json!({"new_owner_id": outsider_id})),
    )
    .await;
    assert_status(&to_outsider, StatusCode::NOT_FOUND);

    // Ensure outsider ID was actually distinct/valid (guards the test).
    assert_ne!(outsider_id, member_id);

    // Valid transfer -> 200, roles swapped, one audit row.
    let ok = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/transfer-ownership"),
        Some(&owner),
        Some(serde_json::json!({"new_owner_id": member_id})),
    )
    .await;
    assert_status(&ok, StatusCode::OK);

    let gid = Uuid::parse_str(&group_id).unwrap();
    let old_owner_role = sqlx::query_scalar!(
        r#"SELECT role::text as "role!" FROM group_members WHERE group_id = $1 AND user_id = $2"#,
        gid,
        owner_id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let new_owner_role = sqlx::query_scalar!(
        r#"SELECT role::text as "role!" FROM group_members WHERE group_id = $1 AND user_id = $2"#,
        gid,
        member_id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(old_owner_role, "admin");
    assert_eq!(new_owner_role, "owner");

    let audit_count = sqlx::query_scalar!(
        "SELECT count(*) FROM audit_log WHERE action = 'ownership_transferred' AND target_id = $1",
        group_id
    )
    .fetch_one(&db)
    .await
    .unwrap()
    .unwrap_or(0);
    assert_eq!(audit_count, 1);
}

/// GET /groups/:id members are enriched with display_name and email.
#[sqlx::test]
async fn get_group_members_include_identity(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;

    let create = call(
        &router,
        Method::POST,
        "/groups",
        Some(&owner),
        Some(serde_json::json!({"name": "Foyer"})),
    )
    .await;
    let group_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}"),
        Some(&owner),
        None,
    )
    .await;
    assert_status(&get, StatusCode::OK);
    let body = json_body(get).await;
    let members = body["members"].as_array().unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0]["email"], "owner@example.test");
    assert_eq!(members[0]["display_name"], "owner@example.test");
    assert_eq!(members[0]["role"], "owner");
}

/// AC #9: creating an 11th group is rejected.
#[sqlx::test]
async fn group_creation_capped_at_ten(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "prolific@example.test", "prolific-password1").await;

    for i in 0..10 {
        let res = call(
            &router,
            Method::POST,
            "/groups",
            Some(&cookie),
            Some(serde_json::json!({"name": format!("Group {i}")})),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
    }
    let eleventh = call(
        &router,
        Method::POST,
        "/groups",
        Some(&cookie),
        Some(serde_json::json!({"name": "Group 11"})),
    )
    .await;
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
    sqlx::query!(
        "INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'owner')",
        group_id,
        user_id
    )
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
    assert!(
        result.is_err(),
        "expected unique violation for a second owner"
    );
}
