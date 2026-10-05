//! The holder of a deactivated account (#289): what a login gets it, the
//! one page a restricted session opens, the reactivation request, the
//! superadmin's answer to it, and what a pending request does to the purge
//! #256 set.

mod common;

use axum::http::{Method, StatusCode};
use chrono::{Duration, Utc};
use common::{assert_status, call, json_body, set_cookie, test_router};
use manage_our_home::jobs::account_purge::{purge_due_accounts, send_deactivation_notices};
use manage_our_home::jobs::retention_purge::{purge, RetentionCutoffs};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const PASSWORD: &str = "holder-password-1";

async fn register_verify(router: &axum::Router, db: &PgPool, email: &str) -> Uuid {
    let res = call(
        router,
        Method::POST,
        "/auth/register",
        None,
        Some(json!({"email": email, "password": PASSWORD, "display_name": email, "declares_minimum_age": true, "accepts_terms": true})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let token = common::verification_token(db, email).await;
    let res = call(
        router,
        Method::GET,
        &format!("/auth/verify-email?token={token}"),
        None,
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn login(router: &axum::Router, email: &str, password: &str) -> axum::response::Response {
    call(
        router,
        Method::POST,
        "/auth/login",
        None,
        Some(json!({"email": email, "password": password})),
    )
    .await
}

async fn login_cookie(router: &axum::Router, email: &str) -> String {
    let res = login(router, email, PASSWORD).await;
    assert_status(&res, StatusCode::OK);
    set_cookie(&res).unwrap()
}

/// A superadmin session, on an account of its own.
async fn superadmin(router: &axum::Router, db: &PgPool, email: &str) -> String {
    register_verify(router, db, email).await;
    sqlx::query("UPDATE users SET is_superadmin = true WHERE email = $1")
        .bind(email)
        .execute(db)
        .await
        .unwrap();
    login_cookie(router, email).await
}

async fn admin_post(router: &axum::Router, cookie: &str, path: &str) -> StatusCode {
    call(router, Method::POST, path, Some(cookie), None)
        .await
        .status()
}

/// Deactivates `user` through the superadmin route.
async fn deactivate(router: &axum::Router, admin: &str, user: Uuid) {
    assert_eq!(
        admin_post(router, admin, &format!("/admin/users/{user}/deactivate")).await,
        StatusCode::NO_CONTENT
    );
}

async fn count(db: &PgPool, sql: &str, user: Uuid) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
        .bind(user)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn sessions(db: &PgPool, user: Uuid) -> Vec<bool> {
    sqlx::query_scalar("SELECT restricted FROM sessions WHERE user_id = $1 AND revoked_at IS NULL")
        .bind(user)
        .fetch_all(db)
        .await
        .unwrap()
}

async fn request(router: &axum::Router, cookie: &str, message: Option<&str>) -> StatusCode {
    call(
        router,
        Method::POST,
        "/account/deactivated/reactivation-request",
        Some(cookie),
        Some(json!({ "message": message })),
    )
    .await
    .status()
}

/// A wrong password on a deactivated account gets the refusal an unknown
/// address gets (OWASP: nothing says what state the account is in), and
/// opens nothing. The right one opens a single restricted session.
#[sqlx::test]
async fn only_the_right_password_on_a_deactivated_account_opens_a_restricted_session(db: PgPool) {
    let router = test_router(db.clone());
    let admin = superadmin(&router, &db, "admin@example.test").await;
    let holder = register_verify(&router, &db, "holder@example.test").await;
    deactivate(&router, &admin, holder).await;

    let unknown = login(&router, "nobody@example.test", PASSWORD).await;
    assert_status(&unknown, StatusCode::UNAUTHORIZED);
    let unknown = json_body(unknown).await;
    let wrong = login(&router, "holder@example.test", "not-the-password-1").await;
    assert_status(&wrong, StatusCode::UNAUTHORIZED);
    assert!(set_cookie(&wrong).is_none());
    assert_eq!(json_body(wrong).await, unknown);
    assert!(sessions(&db, holder).await.is_empty());

    let right = login(&router, "holder@example.test", PASSWORD).await;
    assert_status(&right, StatusCode::OK);
    assert!(set_cookie(&right).is_some());
    assert_eq!(sessions(&db, holder).await, vec![true]);
}

/// The right password on a deactivated account whose address was never
/// verified is refused like on any other account: no session at all.
#[sqlx::test]
async fn an_unverified_deactivated_account_gets_no_session(db: PgPool) {
    let router = test_router(db.clone());
    let admin = superadmin(&router, &db, "admin@example.test").await;
    let holder = register_verify(&router, &db, "holder@example.test").await;
    deactivate(&router, &admin, holder).await;
    sqlx::query("UPDATE users SET email_verified = false WHERE id = $1")
        .bind(holder)
        .execute(&db)
        .await
        .unwrap();

    let res = login(&router, "holder@example.test", PASSWORD).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
    assert!(sessions(&db, holder).await.is_empty());
}

/// A restricted session opens the deactivated-account routes (and logout,
/// below) and nothing else: every other route — the superadmin's included, for a superadmin's
/// own deactivated account — answers 403 `account_deactivated`. A full
/// session, or none, gets a 401 on the deactivated-account routes.
#[sqlx::test]
async fn a_restricted_session_opens_only_the_deactivated_account_routes(db: PgPool) {
    let router = test_router(db.clone());
    let admin = superadmin(&router, &db, "admin@example.test").await;
    let holder = register_verify(&router, &db, "holder@example.test").await;
    sqlx::query("UPDATE users SET is_superadmin = true WHERE id = $1")
        .bind(holder)
        .execute(&db)
        .await
        .unwrap();
    let group = call(
        &router,
        Method::POST,
        "/groups",
        Some(&admin),
        Some(json!({"name": "Foyer"})),
    )
    .await;
    let group = json_body(group).await["id"].as_str().unwrap().to_string();
    deactivate(&router, &admin, holder).await;
    let restricted = login_cookie(&router, "holder@example.test").await;

    let refused: Vec<(Method, String, Option<serde_json::Value>)> = vec![
        (Method::GET, "/auth/me".into(), None),
        (Method::GET, "/groups".into(), None),
        (Method::POST, "/groups".into(), Some(json!({"name": "X"}))),
        (Method::GET, format!("/groups/{group}"), None),
        (Method::GET, "/account/export".into(), None),
        (
            Method::POST,
            "/account/delete".into(),
            Some(json!({"current_password": PASSWORD})),
        ),
        (Method::POST, "/account/delete/cancel".into(), None),
        (
            Method::POST,
            "/settings/password/change".into(),
            Some(json!({"current_password": PASSWORD, "new_password": "another-password-1"})),
        ),
        (Method::GET, "/admin/users".into(), None),
        (
            Method::POST,
            format!("/admin/users/{holder}/reactivate"),
            None,
        ),
    ];
    for (method, path, body) in refused {
        let res = call(&router, method.clone(), &path, Some(&restricted), body).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{method} {path}");
        assert_eq!(
            json_body(res).await["error"],
            "account_deactivated",
            "{method} {path}"
        );
    }
    // Still deactivated: the attempt on the reactivate route did nothing.
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM users WHERE id = $1 AND deactivated_at IS NOT NULL",
            holder
        )
        .await,
        1
    );

    let page = call(
        &router,
        Method::GET,
        "/account/deactivated",
        Some(&restricted),
        None,
    )
    .await;
    assert_status(&page, StatusCode::OK);
    let page = json_body(page).await;
    assert!(page["deactivated_at"].is_string(), "{page}");
    assert!(page["reactivation_requested_at"].is_null(), "{page}");

    for cookie in [Some(admin.as_str()), None] {
        for (method, path) in [
            (Method::GET, "/account/deactivated"),
            (Method::POST, "/account/deactivated/reactivation-request"),
        ] {
            let res = call(&router, method.clone(), path, cookie, Some(json!({}))).await;
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{method} {path}");
        }
    }
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM account_reactivation_requests WHERE user_id = $1",
            holder
        )
        .await,
        0
    );
}

/// One pending request at a time, its note trimmed and bounded, written to
/// the audit log.
#[sqlx::test]
async fn the_holder_asks_for_reactivation_once_at_a_time(db: PgPool) {
    let router = test_router(db.clone());
    let admin = superadmin(&router, &db, "admin@example.test").await;
    let holder = register_verify(&router, &db, "holder@example.test").await;
    deactivate(&router, &admin, holder).await;
    let restricted = login_cookie(&router, "holder@example.test").await;

    let too_long = "a".repeat(1001);
    let res = call(
        &router,
        Method::POST,
        "/account/deactivated/reactivation-request",
        Some(&restricted),
        Some(json!({ "message": too_long })),
    )
    .await;
    assert_status(&res, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json_body(res).await["error"],
        "reactivation_message_too_long"
    );

    assert_eq!(
        request(&router, &restricted, Some("  Je reviens.  ")).await,
        StatusCode::CREATED
    );
    assert_eq!(
        request(&router, &restricted, None).await,
        StatusCode::CONFLICT
    );

    let message: Option<String> =
        sqlx::query_scalar("SELECT message FROM account_reactivation_requests WHERE user_id = $1")
            .bind(holder)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(message.as_deref(), Some("Je reviens."));
    let page = call(
        &router,
        Method::GET,
        "/account/deactivated",
        Some(&restricted),
        None,
    )
    .await;
    assert!(json_body(page).await["reactivation_requested_at"].is_string());
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM audit_log WHERE actor_user_id = $1
             AND action = 'account_reactivation_requested'",
            holder
        )
        .await,
        1
    );
}

/// The superadmin sees the pending request with its note, and turns it
/// down: the account stays deactivated, the request goes, the refusal is
/// in the audit log, and the holder may ask again. Only a superadmin may.
#[sqlx::test]
async fn the_superadmin_sees_and_refuses_a_request(db: PgPool) {
    let router = test_router(db.clone());
    let admin = superadmin(&router, &db, "admin@example.test").await;
    register_verify(&router, &db, "plain@example.test").await;
    let plain = login_cookie(&router, "plain@example.test").await;
    let holder = register_verify(&router, &db, "holder@example.test").await;
    deactivate(&router, &admin, holder).await;
    let restricted = login_cookie(&router, "holder@example.test").await;
    let refuse = format!("/admin/users/{holder}/reactivation-request/refuse");

    assert_eq!(
        admin_post(&router, &admin, &refuse).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&router, &restricted, Some("Merci")).await,
        StatusCode::CREATED
    );
    sqlx::query("UPDATE users SET deactivation_notice_sent_at = now() WHERE id = $1")
        .bind(holder)
        .execute(&db)
        .await
        .unwrap();

    let list = call(&router, Method::GET, "/admin/users", Some(&admin), None).await;
    let list = json_body(list).await;
    let row = list["users"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == holder.to_string())
        .unwrap()
        .clone();
    assert!(row["reactivation_requested_at"].is_string(), "{row}");
    assert_eq!(row["reactivation_message"], "Merci");

    assert_eq!(
        admin_post(&router, &plain, &refuse).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        admin_post(&router, &admin, &refuse).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        admin_post(&router, &admin, &refuse).await,
        StatusCode::NOT_FOUND
    );

    let (deactivated, noticed): (bool, bool) = sqlx::query_as(
        "SELECT deactivated_at IS NOT NULL, deactivation_notice_sent_at IS NOT NULL
         FROM users WHERE id = $1",
    )
    .bind(holder)
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(deactivated);
    assert!(!noticed, "a refusal clears the warning so a fresh one goes");
    let page = call(
        &router,
        Method::GET,
        "/account/deactivated",
        Some(&restricted),
        None,
    )
    .await;
    assert!(json_body(page).await["reactivation_refused_at"].is_string());
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM account_reactivation_requests WHERE user_id = $1",
            holder
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM audit_log
             WHERE action = 'admin.user.reactivation_refuse' AND target_id = $1::text",
            holder
        )
        .await,
        1
    );

    // The restricted session still works, and a new request can be made.
    assert_eq!(
        request(&router, &restricted, None).await,
        StatusCode::CREATED
    );
}

/// Reactivating grants the pending request: it goes, the audit entry says
/// it answered one, the restricted session ends, and the holder logs in to
/// a full session.
#[sqlx::test]
async fn reactivating_grants_the_pending_request(db: PgPool) {
    let router = test_router(db.clone());
    let admin = superadmin(&router, &db, "admin@example.test").await;
    let holder = register_verify(&router, &db, "holder@example.test").await;
    deactivate(&router, &admin, holder).await;
    let restricted = login_cookie(&router, "holder@example.test").await;
    assert_eq!(
        request(&router, &restricted, None).await,
        StatusCode::CREATED
    );

    assert_eq!(
        admin_post(
            &router,
            &admin,
            &format!("/admin/users/{holder}/reactivate")
        )
        .await,
        StatusCode::NO_CONTENT
    );

    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM account_reactivation_requests WHERE user_id = $1",
            holder
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &db,
            r#"SELECT count(*) FROM audit_log
               WHERE action = 'admin.user.reactivate' AND target_id = $1::text
                 AND metadata = '{"reactivation_request": true}'::jsonb"#,
            holder
        )
        .await,
        1
    );
    let page = call(
        &router,
        Method::GET,
        "/account/deactivated",
        Some(&restricted),
        None,
    )
    .await;
    assert_status(&page, StatusCode::UNAUTHORIZED);

    let full = login_cookie(&router, "holder@example.test").await;
    let me = call(&router, Method::GET, "/auth/me", Some(&full), None).await;
    assert_status(&me, StatusCode::OK);
    assert_eq!(sessions(&db, holder).await, vec![false]);
}

/// `POST /auth/logout` takes a restricted session like a full one: the
/// session is revoked and the cookie cleared.
#[sqlx::test]
async fn logging_out_ends_the_restricted_session(db: PgPool) {
    let router = test_router(db.clone());
    let admin = superadmin(&router, &db, "admin@example.test").await;
    let holder = register_verify(&router, &db, "holder@example.test").await;
    deactivate(&router, &admin, holder).await;
    let restricted = login_cookie(&router, "holder@example.test").await;

    let out = call(
        &router,
        Method::POST,
        "/auth/logout",
        Some(&restricted),
        None,
    )
    .await;
    assert_status(&out, StatusCode::NO_CONTENT);
    let cleared = out
        .headers()
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|c| c.starts_with("session_id=;") || c.starts_with("session_id=\"\";"));
    assert!(cleared, "{:?}", out.headers());
    let page = call(
        &router,
        Method::GET,
        "/account/deactivated",
        Some(&restricted),
        None,
    )
    .await;
    assert_status(&page, StatusCode::UNAUTHORIZED);
    assert!(sessions(&db, holder).await.is_empty());
}

/// Sets how long ago `user` was deactivated and warned, as the purge sees it.
async fn backdate(db: &PgPool, user: Uuid, deactivated: &str, notice_days_ago: Option<i32>) {
    sqlx::query(
        "UPDATE users SET deactivated_at = now() - $2::interval,
                          deactivation_notice_sent_at = now() - make_interval(days => $3)
         WHERE id = $1",
    )
    .bind(user)
    .bind(deactivated)
    .bind(notice_days_ago)
    .execute(db)
    .await
    .unwrap();
}

/// Sets how long ago the superadmin refused `user`'s reactivation request.
async fn backdate_refusal(db: &PgPool, user: Uuid, days_ago: i32) {
    sqlx::query(
        "UPDATE users SET reactivation_refused_at = now() - make_interval(days => $2)
         WHERE id = $1",
    )
    .bind(user)
    .bind(days_ago)
    .execute(db)
    .await
    .unwrap();
}

async fn purged(db: &PgPool, user: Uuid) -> bool {
    count(
        db,
        "SELECT count(*) FROM users WHERE id = $1 AND deleted_at IS NOT NULL",
        user,
    )
    .await
        == 1
}

/// A pending request suspends the purge 2 years after the deactivation,
/// and its warning. Once refused, the holder is warned afresh and the
/// purge waits the 30 days after that warning.
#[sqlx::test]
async fn a_pending_request_suspends_the_two_year_purge_and_its_warning(db: PgPool) {
    let router = test_router(db.clone());
    let admin = superadmin(&router, &db, "admin@example.test").await;
    // Due for purge: 2 years and a day, warned 31 days ago.
    let overdue = register_verify(&router, &db, "overdue@example.test").await;
    deactivate(&router, &admin, overdue).await;
    // Due for its warning: in its last 29 days.
    let warnable = register_verify(&router, &db, "warnable@example.test").await;
    deactivate(&router, &admin, warnable).await;
    for email in ["overdue@example.test", "warnable@example.test"] {
        let restricted = login_cookie(&router, email).await;
        assert_eq!(
            request(&router, &restricted, None).await,
            StatusCode::CREATED
        );
    }
    backdate(&db, overdue, "2 years 1 day", Some(31)).await;
    backdate(&db, warnable, "2 years -29 days", None).await;

    let sent = std::sync::Mutex::new(Vec::<String>::new());
    let record = |to: String, _: String, _: String| {
        sent.lock().unwrap().push(to);
        async { Ok::<(), anyhow::Error>(()) }
    };
    send_deactivation_notices(&db, "https://example.test/privacy-policy", &record)
        .await
        .unwrap();
    purge_due_accounts(&db).await.unwrap();
    assert!(sent.lock().unwrap().is_empty());
    assert!(!purged(&db, overdue).await);
    assert!(!purged(&db, warnable).await);

    assert_eq!(
        admin_post(
            &router,
            &admin,
            &format!("/admin/users/{overdue}/reactivation-request/refuse")
        )
        .await,
        StatusCode::NO_CONTENT
    );
    send_deactivation_notices(&db, "https://example.test/privacy-policy", &record)
        .await
        .unwrap();
    purge_due_accounts(&db).await.unwrap();
    assert_eq!(
        sent.lock().unwrap().clone(),
        vec!["overdue@example.test".to_string()]
    );
    assert!(!purged(&db, overdue).await, "30 days after the new warning");

    backdate(&db, overdue, "2 years 31 days", Some(30)).await;
    backdate_refusal(&db, overdue, 30).await;
    purge_due_accounts(&db).await.unwrap();
    assert!(purged(&db, overdue).await);
    assert!(!purged(&db, warnable).await);
}

/// A deletion the holder asked for goes through at 30 days, pending
/// reactivation request or not (#139), and takes the request with it.
#[sqlx::test]
async fn a_requested_deletion_is_purged_at_30_days_with_its_pending_request(db: PgPool) {
    let router = test_router(db.clone());
    let admin = superadmin(&router, &db, "admin@example.test").await;
    let holder = register_verify(&router, &db, "holder@example.test").await;
    deactivate(&router, &admin, holder).await;
    let restricted = login_cookie(&router, "holder@example.test").await;
    assert_eq!(
        request(&router, &restricted, Some("Revenir")).await,
        StatusCode::CREATED
    );
    sqlx::query(
        "UPDATE users SET deletion_requested_at = now() - interval '31 days' WHERE id = $1",
    )
    .bind(holder)
    .execute(&db)
    .await
    .unwrap();

    purge_due_accounts(&db).await.unwrap();

    assert!(purged(&db, holder).await);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM account_reactivation_requests WHERE user_id = $1",
            holder
        )
        .await,
        0
    );
}

/// The retention purge keeps the restricted session of a deactivated
/// account, and deletes the sessions its state refuses for good: a full
/// session of a deactivated account, a restricted one of an active account.
#[sqlx::test]
async fn the_retention_purge_keeps_only_the_sessions_the_account_state_allows(db: PgPool) {
    let router = test_router(db.clone());
    let admin = superadmin(&router, &db, "admin@example.test").await;
    let deactivated = register_verify(&router, &db, "deactivated@example.test").await;
    deactivate(&router, &admin, deactivated).await;
    let active = register_verify(&router, &db, "active@example.test").await;
    for (user, restricted) in [
        (deactivated, true),
        (deactivated, false),
        (active, true),
        (active, false),
    ] {
        sqlx::query(
            "INSERT INTO sessions (user_id, expires_at, restricted, token_hash)
             VALUES ($1, now() + interval '1 day', $2, gen_random_bytes(32))",
        )
        .bind(user)
        .bind(restricted)
        .execute(&db)
        .await
        .unwrap();
    }

    purge(&db, RetentionCutoffs::at(Utc::now() + Duration::seconds(1)))
        .await
        .unwrap();

    assert_eq!(sessions(&db, deactivated).await, vec![true]);
    assert_eq!(sessions(&db, active).await, vec![false]);
}

/// Inherited from #256: neither a password reset nor a new verification
/// link is issued to a deactivated account. The same requests on an active
/// account do issue one — the control that shows the filter does the work.
#[sqlx::test]
async fn no_reset_or_verification_token_is_issued_to_a_deactivated_account(db: PgPool) {
    let router = test_router(db.clone());
    let admin = superadmin(&router, &db, "admin@example.test").await;
    let holder = register_verify(&router, &db, "holder@example.test").await;
    let control = register_verify(&router, &db, "control@example.test").await;
    deactivate(&router, &admin, holder).await;
    sqlx::query("UPDATE users SET email_verified = false WHERE id = ANY($1)")
        .bind(vec![holder, control])
        .execute(&db)
        .await
        .unwrap();
    // Out of the resend cooldown.
    sqlx::query("UPDATE email_verification_tokens SET created_at = now() - interval '1 hour'")
        .execute(&db)
        .await
        .unwrap();

    for email in ["holder@example.test", "control@example.test"] {
        for path in ["/auth/password/forgot", "/auth/verify-email/resend"] {
            let res = call(
                &router,
                Method::POST,
                path,
                None,
                Some(json!({ "email": email })),
            )
            .await;
            assert_status(&res, StatusCode::OK);
        }
    }

    let reset = "SELECT count(*) FROM password_reset_tokens WHERE user_id = $1";
    let fresh = "SELECT count(*) FROM email_verification_tokens
                 WHERE user_id = $1 AND consumed_at IS NULL";
    assert_eq!(count(&db, reset, holder).await, 0);
    assert_eq!(count(&db, fresh, holder).await, 0);
    assert_eq!(count(&db, reset, control).await, 1);
    assert_eq!(count(&db, fresh, control).await, 1);
}

/// Arbitrage of 2026-09-29: after a refusal the holder may ask again, but
/// the deadline runs. An overdue account whose second request is pending
/// gets its warning and is purged.
#[sqlx::test]
async fn after_a_refusal_a_new_request_does_not_hold_back_an_overdue_purge(db: PgPool) {
    let router = test_router(db.clone());
    let admin = superadmin(&router, &db, "admin@example.test").await;
    let holder = register_verify(&router, &db, "holder@example.test").await;
    deactivate(&router, &admin, holder).await;
    let restricted = login_cookie(&router, "holder@example.test").await;
    assert_eq!(
        request(&router, &restricted, None).await,
        StatusCode::CREATED
    );
    assert_eq!(
        admin_post(
            &router,
            &admin,
            &format!("/admin/users/{holder}/reactivation-request/refuse")
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&router, &restricted, Some("Encore")).await,
        StatusCode::CREATED
    );

    // In its last 29 days: the warning goes despite the pending request.
    backdate(&db, holder, "2 years -29 days", None).await;
    let sent = std::sync::Mutex::new(Vec::<String>::new());
    let record = |to: String, _: String, _: String| {
        sent.lock().unwrap().push(to);
        async { Ok::<(), anyhow::Error>(()) }
    };
    send_deactivation_notices(&db, "https://example.test/privacy-policy", &record)
        .await
        .unwrap();
    assert_eq!(
        sent.lock().unwrap().clone(),
        vec!["holder@example.test".to_string()]
    );

    // Overdue, refused 32 days ago and warned 31 days ago: purged, pending
    // request and all.
    backdate(&db, holder, "2 years 1 day", Some(31)).await;
    backdate_refusal(&db, holder, 32).await;
    purge_due_accounts(&db).await.unwrap();
    assert!(purged(&db, holder).await);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM account_reactivation_requests WHERE user_id = $1",
            holder
        )
        .await,
        0
    );
}

/// A new deactivation starts afresh: the refusal of the previous period
/// no longer counts, and a first request suspends the purge again.
#[sqlx::test]
async fn reactivating_forgets_the_refusal_for_the_next_deactivation(db: PgPool) {
    let router = test_router(db.clone());
    let admin = superadmin(&router, &db, "admin@example.test").await;
    let holder = register_verify(&router, &db, "holder@example.test").await;
    deactivate(&router, &admin, holder).await;
    let restricted = login_cookie(&router, "holder@example.test").await;
    assert_eq!(
        request(&router, &restricted, None).await,
        StatusCode::CREATED
    );
    let base = format!("/admin/users/{holder}");
    assert_eq!(
        admin_post(
            &router,
            &admin,
            &format!("{base}/reactivation-request/refuse")
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        admin_post(&router, &admin, &format!("{base}/reactivate")).await,
        StatusCode::NO_CONTENT
    );
    let refused =
        "SELECT count(*) FROM users WHERE id = $1 AND reactivation_refused_at IS NOT NULL";
    assert_eq!(count(&db, refused, holder).await, 0);

    deactivate(&router, &admin, holder).await;
    let restricted = login_cookie(&router, "holder@example.test").await;
    assert_eq!(
        request(&router, &restricted, None).await,
        StatusCode::CREATED
    );
    backdate(&db, holder, "2 years 1 day", Some(31)).await;
    purge_due_accounts(&db).await.unwrap();
    assert!(!purged(&db, holder).await);
}

/// A deactivated account with a pending first request, 2 years and 60 days
/// after its deactivation, warned before it asked: well past the date #256
/// set, which the request suspended.
async fn overdue_with_a_pending_request(
    router: &axum::Router,
    db: &PgPool,
    admin: &str,
    email: &str,
) -> Uuid {
    let holder = register_verify(router, db, email).await;
    deactivate(router, admin, holder).await;
    let restricted = login_cookie(router, email).await;
    assert_eq!(
        request(router, &restricted, None).await,
        StatusCode::CREATED
    );
    backdate(db, holder, "2 years 60 days", Some(90)).await;
    holder
}

async fn refuse(router: &axum::Router, admin: &str, user: Uuid) {
    assert_eq!(
        admin_post(
            router,
            admin,
            &format!("/admin/users/{user}/reactivation-request/refuse")
        )
        .await,
        StatusCode::NO_CONTENT
    );
}

/// #296: the refusal clears the warning. If the new one fails to go out,
/// the purge still waits 30 days after the refusal — the holder is never
/// purged without being warned since.
#[sqlx::test]
async fn a_late_refusal_whose_new_warning_fails_is_not_purged_for_30_days(db: PgPool) {
    let router = test_router(db.clone());
    let admin = superadmin(&router, &db, "admin@example.test").await;
    let holder = overdue_with_a_pending_request(&router, &db, &admin, "holder@example.test").await;
    refuse(&router, &admin, holder).await;

    let tried = std::sync::Mutex::new(0);
    let failing = |_: String, _: String, _: String| {
        *tried.lock().unwrap() += 1;
        async { Err::<(), anyhow::Error>(anyhow::anyhow!("relay refused")) }
    };
    send_deactivation_notices(&db, "https://example.test/privacy-policy", &failing)
        .await
        .unwrap();
    purge_due_accounts(&db).await.unwrap();
    assert_eq!(*tried.lock().unwrap(), 1);
    assert!(!purged(&db, holder).await);

    backdate_refusal(&db, holder, 29).await;
    purge_due_accounts(&db).await.unwrap();
    assert!(!purged(&db, holder).await, "29 days after the refusal");

    backdate_refusal(&db, holder, 30).await;
    purge_due_accounts(&db).await.unwrap();
    assert!(purged(&db, holder).await, "30 days after the refusal");
}

/// #296: a refusal that lands between a pass's warnings and its purge
/// leaves no warning sent since, and nothing is purged in that pass.
#[sqlx::test]
async fn a_refusal_between_the_warnings_and_the_purge_purges_nothing(db: PgPool) {
    let router = test_router(db.clone());
    let admin = superadmin(&router, &db, "admin@example.test").await;
    let holder = overdue_with_a_pending_request(&router, &db, &admin, "holder@example.test").await;

    let sent = std::sync::Mutex::new(Vec::<String>::new());
    let record = |to: String, _: String, _: String| {
        sent.lock().unwrap().push(to);
        async { Ok::<(), anyhow::Error>(()) }
    };
    send_deactivation_notices(&db, "https://example.test/privacy-policy", &record)
        .await
        .unwrap();
    refuse(&router, &admin, holder).await;
    purge_due_accounts(&db).await.unwrap();
    assert!(sent.lock().unwrap().is_empty());
    assert!(!purged(&db, holder).await);
}

/// A deactivation starts with no refusal on record, whatever the row
/// held — the only way a stale `reactivation_refused_at` could cost a
/// first request its suspension of the purge.
#[sqlx::test]
async fn a_deactivation_clears_any_recorded_refusal(db: PgPool) {
    let router = test_router(db.clone());
    let admin = superadmin(&router, &db, "admin@example.test").await;
    let holder = register_verify(&router, &db, "holder@example.test").await;
    backdate_refusal(&db, holder, 1).await;
    deactivate(&router, &admin, holder).await;
    let refused =
        "SELECT count(*) FROM users WHERE id = $1 AND reactivation_refused_at IS NOT NULL";
    assert_eq!(count(&db, refused, holder).await, 0);
}

/// Lets `days` go by for `user`'s deactivation, warning and refusal dates.
async fn days_pass(db: &PgPool, user: Uuid, days: i32) {
    sqlx::query(
        "UPDATE users
         SET deactivated_at = deactivated_at - make_interval(days => $2),
             deactivation_notice_sent_at = deactivation_notice_sent_at - make_interval(days => $2),
             reactivation_refused_at = reactivation_refused_at - make_interval(days => $2)
         WHERE id = $1",
    )
    .bind(user)
    .bind(days)
    .execute(db)
    .await
    .unwrap();
}

/// #313: only the first refusal postpones the purge. A second one, 20 days
/// later, sends no new warning and leaves the purge 30 days after the
/// first refusal and its warning.
#[sqlx::test]
async fn a_second_refusal_does_not_postpone_the_purge_again(db: PgPool) {
    let router = test_router(db.clone());
    let admin = superadmin(&router, &db, "admin@example.test").await;
    let holder = overdue_with_a_pending_request(&router, &db, &admin, "holder@example.test").await;
    refuse(&router, &admin, holder).await;

    let sent = std::sync::Mutex::new(Vec::<String>::new());
    let record = |to: String, _: String, _: String| {
        sent.lock().unwrap().push(to);
        async { Ok::<(), anyhow::Error>(()) }
    };
    send_deactivation_notices(&db, "https://example.test/privacy-policy", &record)
        .await
        .unwrap();
    assert_eq!(
        sent.lock().unwrap().len(),
        1,
        "warned after the first refusal"
    );

    days_pass(&db, holder, 20).await;
    let restricted = login_cookie(&router, "holder@example.test").await;
    assert_eq!(
        request(&router, &restricted, Some("Encore")).await,
        StatusCode::CREATED
    );
    refuse(&router, &admin, holder).await;
    let unchanged = "SELECT count(*) FROM users WHERE id = $1
                       AND reactivation_refused_at < now() - interval '19 days'
                       AND deactivation_notice_sent_at < now() - interval '19 days'";
    assert_eq!(count(&db, unchanged, holder).await, 1);

    send_deactivation_notices(&db, "https://example.test/privacy-policy", &record)
        .await
        .unwrap();
    purge_due_accounts(&db).await.unwrap();
    assert_eq!(sent.lock().unwrap().len(), 1, "no warning after the second");
    assert!(
        !purged(&db, holder).await,
        "20 days after the first refusal"
    );

    days_pass(&db, holder, 10).await;
    purge_due_accounts(&db).await.unwrap();
    assert!(purged(&db, holder).await, "30 days after the first refusal");
}
