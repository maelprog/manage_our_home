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

async fn make_superadmin(db: &PgPool, email: &str) {
    sqlx::query!(
        "UPDATE users SET is_superadmin = true WHERE email = $1",
        email
    )
    .execute(db)
    .await
    .unwrap();
}

/// Front epic F9 (#24) gate contract: `GET /auth/me` reports `is_superadmin`
/// so `apps/web` can render (or hide) the `/admin` nav + route tree. `false`
/// for a plain account, `true` once the flag is set.
#[sqlx::test]
async fn auth_me_reports_superadmin_flag(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "me-superadmin@example.test",
        "super-password1",
    )
    .await;

    let before = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&before, StatusCode::OK);
    assert_eq!(
        json_body(before).await["is_superadmin"],
        serde_json::json!(false)
    );

    make_superadmin(&db, "me-superadmin@example.test").await;

    let after = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&after, StatusCode::OK);
    assert_eq!(
        json_body(after).await["is_superadmin"],
        serde_json::json!(true)
    );
}

/// AC #1: a non-superadmin user gets 403 on all three `/admin/*` routes.
#[sqlx::test]
async fn non_superadmin_gets_403_on_every_admin_route(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "plain-user@example.test", "plain-password1").await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let user_id: uuid::Uuid = sqlx::query_scalar!(
        "SELECT id FROM users WHERE email = $1",
        "plain-user@example.test"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let _ = group_id;

    let groups_res = call(&router, Method::GET, "/admin/groups", Some(&cookie), None).await;
    assert_status(&groups_res, StatusCode::FORBIDDEN);

    let users_res = call(&router, Method::GET, "/admin/users", Some(&cookie), None).await;
    assert_status(&users_res, StatusCode::FORBIDDEN);

    let deactivate_res = call(
        &router,
        Method::POST,
        &format!("/admin/users/{user_id}/deactivate"),
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&deactivate_res, StatusCode::FORBIDDEN);
}

/// Unauthenticated requests (no session cookie at all) get 401, not 403 —
/// mirrors `AuthUser`'s behavior for the regular endpoints.
#[sqlx::test]
async fn unauthenticated_gets_401_on_admin_routes(db: PgPool) {
    let router = test_router(db.clone());
    let res = call(&router, Method::GET, "/admin/groups", None, None).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
}

/// AC #2, #4, #6: a superadmin sees groups from families they are not a
/// member of, and every successful admin action writes one `audit_log`
/// row with the superadmin's `user_id` as actor.
#[sqlx::test]
async fn superadmin_sees_groups_across_families_and_is_audited(db: PgPool) {
    let router = test_router(db.clone());
    let owner_a = register_verify_login(
        &router,
        &db,
        "admin-family-a@example.test",
        "owner-password1",
    )
    .await;
    let owner_b = register_verify_login(
        &router,
        &db,
        "admin-family-b@example.test",
        "owner-password1",
    )
    .await;
    let superadmin_cookie =
        register_verify_login(&router, &db, "superadmin@example.test", "super-password1").await;
    make_superadmin(&db, "superadmin@example.test").await;

    let group_a = create_group(&router, &owner_a, "Famille A").await;
    let group_b = create_group(&router, &owner_b, "Famille B").await;

    let groups_res = call(
        &router,
        Method::GET,
        "/admin/groups",
        Some(&superadmin_cookie),
        None,
    )
    .await;
    assert_status(&groups_res, StatusCode::OK);
    let body = json_body(groups_res).await;
    let ids: Vec<String> = body["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&group_a), "expected to see family A's group");
    assert!(ids.contains(&group_b), "expected to see family B's group");

    let users_res = call(
        &router,
        Method::GET,
        "/admin/users",
        Some(&superadmin_cookie),
        None,
    )
    .await;
    assert_status(&users_res, StatusCode::OK);
    let users_body = json_body(users_res).await;
    let emails: Vec<String> = users_body["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["email"].as_str().unwrap().to_string())
        .collect();
    assert!(emails.contains(&"admin-family-a@example.test".to_string()));
    assert!(emails.contains(&"admin-family-b@example.test".to_string()));

    let superadmin_id: uuid::Uuid = sqlx::query_scalar!(
        "SELECT id FROM users WHERE email = $1",
        "superadmin@example.test"
    )
    .fetch_one(&db)
    .await
    .unwrap();

    let audit_count: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) as "count!" FROM audit_log WHERE actor_user_id = $1 AND action = 'admin.groups.list'"#,
        superadmin_id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(audit_count, 1);

    let users_audit_count: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) as "count!" FROM audit_log WHERE actor_user_id = $1 AND action = 'admin.users.list'"#,
        superadmin_id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(users_audit_count, 1);
}

/// AC #3, #4: deactivating a user revokes all active sessions (the old
/// cookie stops working) and sets `deleted_at`, with an audit_log row.
#[sqlx::test]
async fn deactivate_revokes_sessions_and_sets_deleted_at(db: PgPool) {
    let router = test_router(db.clone());
    let target_cookie =
        register_verify_login(&router, &db, "target-user@example.test", "target-password1").await;
    let superadmin_cookie =
        register_verify_login(&router, &db, "superadmin2@example.test", "super-password1").await;
    make_superadmin(&db, "superadmin2@example.test").await;

    // Prove the target's session works before deactivation.
    let group_id = create_group(&router, &target_cookie, "Foyer").await;
    let pre_check = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}"),
        Some(&target_cookie),
        None,
    )
    .await;
    assert_status(&pre_check, StatusCode::OK);

    let target_id: uuid::Uuid = sqlx::query_scalar!(
        "SELECT id FROM users WHERE email = $1",
        "target-user@example.test"
    )
    .fetch_one(&db)
    .await
    .unwrap();

    let deactivate_res = call(
        &router,
        Method::POST,
        &format!("/admin/users/{target_id}/deactivate"),
        Some(&superadmin_cookie),
        None,
    )
    .await;
    assert_status(&deactivate_res, StatusCode::NO_CONTENT);

    // The old session cookie no longer works.
    let post_check = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}"),
        Some(&target_cookie),
        None,
    )
    .await;
    assert_status(&post_check, StatusCode::UNAUTHORIZED);

    let deleted_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar!("SELECT deleted_at FROM users WHERE id = $1", target_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(deleted_at.is_some());

    let superadmin_id: uuid::Uuid = sqlx::query_scalar!(
        "SELECT id FROM users WHERE email = $1",
        "superadmin2@example.test"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let audit_count: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) as "count!" FROM audit_log WHERE actor_user_id = $1 AND action = 'admin.user.deactivate' AND target_id = $2"#,
        superadmin_id,
        target_id.to_string(),
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(audit_count, 1);
}

/// Deactivating a user that doesn't exist (or is already deactivated)
/// 404s rather than silently succeeding.
#[sqlx::test]
async fn deactivate_unknown_user_returns_404(db: PgPool) {
    let router = test_router(db.clone());
    let superadmin_cookie =
        register_verify_login(&router, &db, "superadmin3@example.test", "super-password1").await;
    make_superadmin(&db, "superadmin3@example.test").await;

    let res = call(
        &router,
        Method::POST,
        &format!("/admin/users/{}/deactivate", uuid::Uuid::new_v4()),
        Some(&superadmin_cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);
}

/// Regression check on existing RLS: a non-superadmin member of family A
/// still cannot see family B via the *regular* (non-admin) endpoints. This
/// proves Epic #8's `admin_db`/`BYPASSRLS` addition didn't weaken the
/// tenant-isolation boundary for ordinary requests.
#[sqlx::test]
async fn regular_endpoints_still_enforce_family_isolation(db: PgPool) {
    let router = test_router(db.clone());
    let owner_a = register_verify_login(
        &router,
        &db,
        "regress-family-a@example.test",
        "owner-password1",
    )
    .await;
    let owner_b = register_verify_login(
        &router,
        &db,
        "regress-family-b@example.test",
        "owner-password1",
    )
    .await;
    let group_a = create_group(&router, &owner_a, "Famille A").await;
    let _group_b = create_group(&router, &owner_b, "Famille B").await;

    // `messages` is scoped via the app-level `require_role` check (in
    // addition to RLS), so this exercises the same isolation boundary the
    // Messagerie epic already proved (see `messagerie_flow.rs`); `get_group`
    // itself relies solely on `FORCE ROW LEVEL SECURITY`, which the local
    // test DB role (a Postgres superuser, same as CI's) always bypasses
    // regardless of Epic #8 — not a regression this test can observe.
    let res = call(
        &router,
        Method::GET,
        &format!("/groups/{group_a}/messages"),
        Some(&owner_b),
        None,
    )
    .await;
    assert_status(&res, StatusCode::FORBIDDEN);
}

/// A superadmin who is *also* a plain member of their own family still
/// uses the regular endpoints for that family — the admin routes are a
/// separate, additive capability, not a replacement of normal membership
/// rules.
#[sqlx::test]
async fn superadmin_membership_in_own_family_is_unaffected(db: PgPool) {
    let router = test_router(db.clone());
    let superadmin_cookie = register_verify_login(
        &router,
        &db,
        "superadmin-member@example.test",
        "super-password1",
    )
    .await;
    make_superadmin(&db, "superadmin-member@example.test").await;

    let group_id = create_group(&router, &superadmin_cookie, "Foyer").await;
    let get_res = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}"),
        Some(&superadmin_cookie),
        None,
    )
    .await;
    assert_status(&get_res, StatusCode::OK);
}
