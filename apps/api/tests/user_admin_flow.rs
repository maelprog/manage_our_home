mod common;

use axum::http::{Method, StatusCode};
use common::{assert_status, call, json_body, session_id_of, set_cookie, test_router};
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
        Some(serde_json::json!({"email": email, "password": password, "display_name": email, "declares_minimum_age": true, "accepts_terms": true})),
    )
    .await;
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

/// AC #1: a non-superadmin user gets 403 on all seven `/admin/*` routes,
/// `designate_owner` included: its `SuperAdminUser` extractor runs before
/// the JSON body is read.
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

    let routes = [
        (Method::GET, "/admin/groups".to_string(), None),
        (
            Method::GET,
            format!("/admin/groups/{group_id}/members"),
            None,
        ),
        (
            Method::POST,
            format!("/admin/groups/{group_id}/owner"),
            Some(serde_json::json!({ "user_id": user_id })),
        ),
        (Method::GET, "/admin/users".to_string(), None),
        (
            Method::POST,
            format!("/admin/users/{user_id}/deactivate"),
            None,
        ),
        (
            Method::POST,
            format!("/admin/users/{user_id}/reactivate"),
            None,
        ),
        (
            Method::POST,
            format!("/admin/users/{user_id}/reactivation-request/refuse"),
            None,
        ),
    ];
    for (method, uri, body) in routes {
        let res = call(&router, method, &uri, Some(&cookie), body).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{uri}");
    }
}

/// Unauthenticated requests (no session cookie at all) get 401, not 403 —
/// mirrors `AuthUser`'s behavior for the regular endpoints.
#[sqlx::test]
async fn unauthenticated_gets_401_on_admin_routes(db: PgPool) {
    let router = test_router(db.clone());
    let res = call(&router, Method::GET, "/admin/groups", None, None).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
}

async fn login(router: &axum::Router, email: &str, password: &str) -> String {
    let res = call(
        router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": email, "password": password})),
    )
    .await;
    set_cookie(&res).unwrap()
}

/// #196: the admin gate validates the session before it looks at the
/// superadmin flag, so a request whose session is invalid gets 401 whatever
/// the account's flag says — never 403, which would tell a caller holding a
/// dead cookie that it names a live, non-superadmin account. Every
/// invalidity branch of the session check is walked for both flag values:
/// a superadmin's dead session must not get through either.
#[sqlx::test]
async fn invalid_session_gets_401_on_admin_routes_whatever_the_flag(db: PgPool) {
    let router = test_router(db.clone());

    let unknown = format!("session_id={}", uuid::Uuid::new_v4());
    for cookie in ["session_id=not-a-uuid", "session_id=", unknown.as_str()] {
        let res = call(&router, Method::GET, "/admin/groups", Some(cookie), None).await;
        assert_status(&res, StatusCode::UNAUTHORIZED);
    }

    for (superadmin, email) in [
        (false, "invalid-plain@example.test"),
        (true, "invalid-super@example.test"),
    ] {
        let password = "invalid-password1";
        let expected_when_valid = if superadmin {
            StatusCode::OK
        } else {
            StatusCode::FORBIDDEN
        };

        // Revoked session.
        let revoked = register_verify_login(&router, &db, email, password).await;
        if superadmin {
            make_superadmin(&db, email).await;
        }
        let res = call(&router, Method::GET, "/admin/groups", Some(&revoked), None).await;
        assert_status(&res, expected_when_valid);
        sqlx::query("UPDATE sessions SET revoked_at = now() WHERE id = $1")
            .bind(session_id_of(&db, &revoked).await)
            .execute(&db)
            .await
            .unwrap();
        let res = call(&router, Method::GET, "/admin/groups", Some(&revoked), None).await;
        assert_status(&res, StatusCode::UNAUTHORIZED);

        // Expired session.
        let expired = login(&router, email, password).await;
        let res = call(&router, Method::GET, "/admin/groups", Some(&expired), None).await;
        assert_status(&res, expected_when_valid);
        sqlx::query("UPDATE sessions SET expires_at = now() - interval '1 second' WHERE id = $1")
            .bind(session_id_of(&db, &expired).await)
            .execute(&db)
            .await
            .unwrap();
        let res = call(&router, Method::GET, "/admin/groups", Some(&expired), None).await;
        assert_status(&res, StatusCode::UNAUTHORIZED);

        // Idle session (#195), well within its absolute lifetime.
        let idle = login(&router, email, password).await;
        let res = call(&router, Method::GET, "/admin/groups", Some(&idle), None).await;
        assert_status(&res, expected_when_valid);
        sqlx::query("UPDATE sessions SET last_seen_at = now() - interval '8 days' WHERE id = $1")
            .bind(session_id_of(&db, &idle).await)
            .execute(&db)
            .await
            .unwrap();
        let res = call(&router, Method::GET, "/admin/groups", Some(&idle), None).await;
        assert_status(&res, StatusCode::UNAUTHORIZED);

        // Deactivated account, the session row itself left untouched.
        let orphaned = login(&router, email, password).await;
        let res = call(&router, Method::GET, "/admin/groups", Some(&orphaned), None).await;
        assert_status(&res, expected_when_valid);
        sqlx::query("UPDATE users SET deactivated_at = now() WHERE email = $1")
            .bind(email)
            .execute(&db)
            .await
            .unwrap();
        let res = call(&router, Method::GET, "/admin/groups", Some(&orphaned), None).await;
        assert_status(&res, StatusCode::UNAUTHORIZED);
    }
}

/// #196: the admin gate's 403 and 401 carry the same bodies as every other
/// `AppError::Forbidden` / `AppError::Unauthorized` — it adds no shape of
/// its own.
#[sqlx::test]
async fn admin_gate_rejections_keep_the_shared_error_bodies(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "body-plain@example.test", "body-password1").await;

    let res = call(&router, Method::GET, "/admin/users", Some(&cookie), None).await;
    assert_status(&res, StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(res).await,
        serde_json::json!({"error": "forbidden"})
    );

    let res = call(&router, Method::GET, "/admin/users", None, None).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(res).await,
        serde_json::json!({"error": "unauthorized"})
    );
}

/// #196: an admin request is activity on the session like any other, so it
/// refreshes `sessions.last_seen_at`. Until #196 the admin gate ran its own
/// copy of the session check, which skipped that refresh: a superadmin
/// working only in `/admin` kept a session that looked idle — the one an
/// inactivity timeout (#195) would have cut first.
#[sqlx::test]
async fn admin_request_refreshes_last_seen_at(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "seen-super@example.test", "seen-password1").await;
    make_superadmin(&db, "seen-super@example.test").await;
    let session_id = session_id_of(&db, &cookie).await;

    // Past the hourly refresh interval, within the 2 hours a superadmin
    // session may stay unused on the admin routes (#226).
    sqlx::query("UPDATE sessions SET last_seen_at = now() - interval '90 minutes' WHERE id = $1")
        .bind(session_id)
        .execute(&db)
        .await
        .unwrap();

    let res = call(&router, Method::GET, "/admin/groups", Some(&cookie), None).await;
    assert_status(&res, StatusCode::OK);

    let still_stale: bool = sqlx::query_scalar(
        "SELECT last_seen_at < now() - interval '1 hour' FROM sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(!still_stale, "an admin request must refresh last_seen_at");
}

/// #226: the admin routes refuse a superadmin session opened more than
/// 12 hours ago, or unused for more than 2 hours, with the bare 401 — while
/// the same session keeps opening the member routes. A fresh login opens
/// the admin routes again.
#[sqlx::test]
async fn an_old_or_idle_superadmin_session_is_refused_on_admin_routes_only(db: PgPool) {
    let router = test_router(db.clone());
    let email = "capped-super@example.test";
    let password = "capped-password1";
    register_verify_login(
        &router,
        &db,
        "capped-target@example.test",
        "target-password1",
    )
    .await;
    let target_id: uuid::Uuid = sqlx::query_scalar!(
        "SELECT id FROM users WHERE email = $1",
        "capped-target@example.test"
    )
    .fetch_one(&db)
    .await
    .unwrap();

    // Opened 12 h 1 min ago, used a minute ago.
    let old = register_verify_login(&router, &db, email, password).await;
    make_superadmin(&db, email).await;
    let res = call(&router, Method::GET, "/admin/users", Some(&old), None).await;
    assert_status(&res, StatusCode::OK);
    sqlx::query(
        "UPDATE sessions SET created_at = now() - interval '12 hours 1 minute', \
         last_seen_at = now() - interval '1 minute' WHERE id = $1",
    )
    .bind(session_id_of(&db, &old).await)
    .execute(&db)
    .await
    .unwrap();
    for path in ["/admin/groups", "/admin/users"] {
        let res = call(&router, Method::GET, path, Some(&old), None).await;
        assert_status(&res, StatusCode::UNAUTHORIZED);
        assert_eq!(
            json_body(res).await,
            serde_json::json!({"error": "unauthorized"})
        );
    }
    let res = call(
        &router,
        Method::POST,
        &format!("/admin/users/{target_id}/deactivate"),
        Some(&old),
        None,
    )
    .await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
    let deactivated: bool =
        sqlx::query_scalar("SELECT deactivated_at IS NOT NULL FROM users WHERE id = $1")
            .bind(target_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(!deactivated, "a refused admin request must change nothing");
    let res = call(&router, Method::GET, "/auth/me", Some(&old), None).await;
    assert_status(&res, StatusCode::OK);
    let res = call(&router, Method::GET, "/groups", Some(&old), None).await;
    assert_status(&res, StatusCode::OK);

    // Opened 3 h ago, unused for 2 h 1 min: idle for an admin session,
    // nowhere near the 7 days of #195.
    let idle = login(&router, email, password).await;
    sqlx::query(
        "UPDATE sessions SET created_at = now() - interval '3 hours', \
         last_seen_at = now() - interval '2 hours 1 minute' WHERE id = $1",
    )
    .bind(session_id_of(&db, &idle).await)
    .execute(&db)
    .await
    .unwrap();
    let res = call(&router, Method::GET, "/admin/groups", Some(&idle), None).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
    // No request revives it for the admin routes: neither a second admin
    // request, nor a member request in between — `apps/web` asks
    // `/auth/me` before every `/admin` page.
    let res = call(&router, Method::GET, "/admin/groups", Some(&idle), None).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
    for path in ["/auth/me", "/groups"] {
        let res = call(&router, Method::GET, path, Some(&idle), None).await;
        assert_status(&res, StatusCode::OK);
        let res = call(&router, Method::GET, "/admin/users", Some(&idle), None).await;
        assert_status(&res, StatusCode::UNAUTHORIZED);
    }
    // #339: that activity still counts for the session itself.
    // `last_seen_at` dates it — the sessions page shows it, and the 7 days
    // of #195 run from it — while the admin access stays closed through
    // `expires_at`: the 2 h timeout fell 1 minute ago, so the admin access
    // ended then, and `expires_at` lies 29 days 12 hours after that.
    let (seen_recently, expiry_at_closure): (bool, bool) = sqlx::query_as(
        "SELECT last_seen_at > now() - interval '1 minute', \
         expires_at > now() + interval '29 days 11 hours 57 minutes' \
         AND expires_at <= now() + interval '29 days 11 hours 59 minutes' \
         FROM sessions WHERE id = $1",
    )
    .bind(session_id_of(&db, &idle).await)
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(
        seen_recently,
        "member activity must refresh last_seen_at of a session idle for /admin"
    );
    assert!(
        expiry_at_closure,
        "closing the admin access moves expires_at to where the timeout fell"
    );
    let res = call(
        &router,
        Method::POST,
        &format!("/admin/users/{target_id}/deactivate"),
        Some(&idle),
        None,
    )
    .await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
    // Nor does the age cap lift after member requests on the old session.
    let res = call(&router, Method::GET, "/admin/groups", Some(&old), None).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);

    // Just within both bounds.
    let recent = login(&router, email, password).await;
    sqlx::query(
        "UPDATE sessions SET created_at = now() - interval '11 hours 59 minutes', \
         last_seen_at = now() - interval '1 hour 59 minutes' WHERE id = $1",
    )
    .bind(session_id_of(&db, &recent).await)
    .execute(&db)
    .await
    .unwrap();
    let res = call(&router, Method::GET, "/admin/groups", Some(&recent), None).await;
    assert_status(&res, StatusCode::OK);

    // Logging in again is the way back in.
    let fresh = login(&router, email, password).await;
    let res = call(&router, Method::GET, "/admin/users", Some(&fresh), None).await;
    assert_status(&res, StatusCode::OK);
}

/// #226: the cap is a superadmin's. A member's session of the same age
/// still gets the 403 on the admin routes — the flag is checked first — and
/// keeps opening everything else.
#[sqlx::test]
async fn an_old_member_session_still_gets_403_on_admin_routes(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "old-plain@example.test", "old-password1").await;
    sqlx::query(
        "UPDATE sessions SET created_at = now() - interval '20 days', \
         last_seen_at = now() - interval '3 days' WHERE id = $1",
    )
    .bind(session_id_of(&db, &cookie).await)
    .execute(&db)
    .await
    .unwrap();
    let res = call(&router, Method::GET, "/admin/groups", Some(&cookie), None).await;
    assert_status(&res, StatusCode::FORBIDDEN);
    let res = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&res, StatusCode::OK);
}

/// #339: closing the admin access is a superadmin's. A member's session
/// idle past the 2 hours of the admin timeout, within its first 12 hours,
/// keeps the `expires_at` its opening gave it — the opening date plus the
/// 30 days, both read off the database's clock.
#[sqlx::test]
async fn an_idle_member_session_keeps_its_expiry(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "idle-plain@example.test", "idle-password1").await;
    let session_id = session_id_of(&db, &cookie).await;
    let expiry_is_opening_plus_ttl = || async {
        sqlx::query_scalar::<_, bool>(
            "SELECT expires_at = created_at + interval '30 days' FROM sessions WHERE id = $1",
        )
        .bind(session_id)
        .fetch_one(&db)
        .await
        .unwrap()
    };
    assert!(
        expiry_is_opening_plus_ttl().await,
        "a session's expires_at is its opening plus 30 days"
    );

    sqlx::query(
        "UPDATE sessions SET created_at = created_at - interval '3 hours', \
         expires_at = expires_at - interval '3 hours', \
         last_seen_at = now() - interval '2 hours 1 minute' WHERE id = $1",
    )
    .bind(session_id)
    .execute(&db)
    .await
    .unwrap();
    for path in ["/auth/me", "/groups"] {
        let res = call(&router, Method::GET, path, Some(&cookie), None).await;
        assert_status(&res, StatusCode::OK);
    }
    let seen_recently: bool = sqlx::query_scalar(
        "SELECT last_seen_at > now() - interval '1 minute' FROM sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(seen_recently);
    assert!(
        expiry_is_opening_plus_ttl().await,
        "a member's activity must leave expires_at where the opening set it"
    );
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
/// cookie stops working) and sets `deactivated_at` — not `deleted_at`, the
/// purge's mark (#256) — with an audit_log row.
#[sqlx::test]
async fn deactivate_revokes_sessions_and_sets_deactivated_at(db: PgPool) {
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

    let (deactivated, purged): (bool, bool) = sqlx::query_as(
        "SELECT deactivated_at IS NOT NULL, deleted_at IS NOT NULL FROM users WHERE id = $1",
    )
    .bind(target_id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(deactivated);
    assert!(!purged);

    // Nor can the account open a new full one: the right password opens a
    // restricted session (#289), which the app refuses.
    let relogin = call(
        &router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "target-user@example.test", "password": "target-password1"})),
    )
    .await;
    assert_status(&relogin, StatusCode::OK);
    let restricted = set_cookie(&relogin).unwrap();
    let refused = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}"),
        Some(&restricted),
        None,
    )
    .await;
    assert_status(&refused, StatusCode::FORBIDDEN);
    assert_eq!(json_body(refused).await["error"], "account_deactivated");

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

async fn admin_post(
    router: &axum::Router,
    cookie: &str,
    user: uuid::Uuid,
    action: &str,
) -> StatusCode {
    call(
        router,
        Method::POST,
        &format!("/admin/users/{user}/{action}"),
        Some(cookie),
        None,
    )
    .await
    .status()
}

/// #256: the superadmin gives a deactivated account back. Login works
/// again, the purge clock and its warning are cleared, and the action is
/// in `audit_log`. A second reactivation, or one of an account never
/// deactivated, 404s.
#[sqlx::test]
async fn reactivate_gives_the_account_back_and_is_audited(db: PgPool) {
    let router = test_router(db.clone());
    register_verify_login(&router, &db, "back@example.test", "back-password1").await;
    let superadmin_cookie =
        register_verify_login(&router, &db, "superadmin4@example.test", "super-password1").await;
    make_superadmin(&db, "superadmin4@example.test").await;
    let target: uuid::Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind("back@example.test")
        .fetch_one(&db)
        .await
        .unwrap();

    assert_eq!(
        admin_post(&router, &superadmin_cookie, target, "reactivate").await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        admin_post(&router, &superadmin_cookie, target, "deactivate").await,
        StatusCode::NO_CONTENT
    );
    // A warning already sent, as the purge job would have stamped it.
    sqlx::query("UPDATE users SET deactivation_notice_sent_at = now() WHERE id = $1")
        .bind(target)
        .execute(&db)
        .await
        .unwrap();
    let login_body =
        serde_json::json!({"email": "back@example.test", "password": "back-password1"});
    // The right password opens only a restricted session (#289).
    let restricted = call(
        &router,
        Method::POST,
        "/auth/login",
        None,
        Some(login_body.clone()),
    )
    .await;
    assert_status(&restricted, StatusCode::OK);
    let restricted = set_cookie(&restricted).unwrap();
    let me = call(&router, Method::GET, "/auth/me", Some(&restricted), None).await;
    assert_status(&me, StatusCode::FORBIDDEN);

    assert_eq!(
        admin_post(&router, &superadmin_cookie, target, "reactivate").await,
        StatusCode::NO_CONTENT
    );

    // The restricted session does not become a full one: it is revoked,
    // and opens neither the app nor the deactivated-account page.
    for path in ["/auth/me", "/account/deactivated"] {
        let res = call(&router, Method::GET, path, Some(&restricted), None).await;
        assert_status(&res, StatusCode::UNAUTHORIZED);
    }
    let live_restricted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sessions WHERE user_id = $1 AND restricted AND revoked_at IS NULL",
    )
    .bind(target)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(live_restricted, 0);

    let (deactivated, noticed): (bool, bool) = sqlx::query_as(
        "SELECT deactivated_at IS NOT NULL, deactivation_notice_sent_at IS NOT NULL
         FROM users WHERE id = $1",
    )
    .bind(target)
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(!deactivated);
    assert!(!noticed);
    let back = call(&router, Method::POST, "/auth/login", None, Some(login_body)).await;
    assert_status(&back, StatusCode::OK);

    let audited: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM audit_log a JOIN users u ON u.id = a.actor_user_id
         WHERE u.email = 'superadmin4@example.test'
           AND a.action = 'admin.user.reactivate' AND a.target_id = $1
           AND a.metadata = '{"reactivation_request": false}'::jsonb"#,
    )
    .bind(target.to_string())
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(audited, 1);

    assert_eq!(
        admin_post(&router, &superadmin_cookie, target, "reactivate").await,
        StatusCode::NOT_FOUND
    );
}

/// #256: a purged account can be neither deactivated nor reactivated —
/// the purge leaves nothing to lock or to give back.
#[sqlx::test]
async fn a_purged_account_is_404_to_both_admin_actions(db: PgPool) {
    let router = test_router(db.clone());
    let superadmin_cookie =
        register_verify_login(&router, &db, "superadmin5@example.test", "super-password1").await;
    make_superadmin(&db, "superadmin5@example.test").await;
    let purged: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, display_name, deleted_at, deactivated_at)
         VALUES ('deleted-x@deleted.invalid', 'Utilisateur supprimé', now(), now() - interval '3 years')
         RETURNING id",
    )
    .fetch_one(&db)
    .await
    .unwrap();

    for action in ["deactivate", "reactivate"] {
        assert_eq!(
            admin_post(&router, &superadmin_cookie, purged, action).await,
            StatusCode::NOT_FOUND,
            "{action}"
        );
    }
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
    // Messagerie epic already proved (see `messagerie_flow.rs`). `get_group`
    // used to rely solely on `FORCE ROW LEVEL SECURITY`, which the local test
    // DB role (a Postgres superuser, same as CI's) always bypasses; it now
    // filters on membership in the query, and
    // `groups_flow::get_group_is_refused_to_a_non_member` covers that.
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
