//! A purged account cannot get back in, by any route (#139).
//!
//! The purge anonymises the `users` row rather than deleting it, so the
//! row, its id and its `deleted_at` stay: each route that opens a session
//! or writes a credential has to refuse it on its own. One test per route,
//! each against an account taken through the real deletion request and the
//! real purge job. A support-deactivated account (`deactivated_at` set, email,
//! password and tokens kept) is the case where a token still exists, so the
//! two token routes are also tried against one.

mod common;

use axum::http::{Method, StatusCode};
use common::{assert_status, call, set_cookie};
use manage_our_home::jobs::account_purge::purge_due_accounts;
use sqlx::PgPool;
use uuid::Uuid;

const PASSWORD: &str = "purged-pass-1";

/// Same stub as `auth_flow.rs`: a local Google token endpoint and userinfo
/// endpoint serving one fixed profile.
async fn router_with_google_stub(db: PgPool, sub: &str, email: &str) -> axum::Router {
    let profile = serde_json::json!({
        "sub": sub,
        "email": email,
        "email_verified": true,
        "name": "Google User",
    });
    let google = axum::Router::new()
        .route(
            "/token",
            axum::routing::post(|| async {
                axum::Json(serde_json::json!({
                    "access_token": "local-access-token",
                    "token_type": "bearer",
                }))
            }),
        )
        .route(
            "/userinfo",
            axum::routing::get(move || {
                let profile = profile.clone();
                async move { axum::Json(profile) }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, google).await.unwrap() });

    let mut state = common::test_state(db);
    state.google_oauth = state
        .google_oauth
        .set_token_uri(oauth2::TokenUrl::new(format!("{base}/token")).unwrap());
    state.google_userinfo_url = format!("{base}/userinfo");
    manage_our_home::build_router(state)
}

async fn google_callback(router: &axum::Router) -> axum::response::Response {
    let verifier = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFG";
    call(
        router,
        Method::GET,
        "/auth/google/callback?code=c&state=s",
        Some(&format!(
            "google_oauth_state=s; google_oauth_pkce_verifier={verifier}"
        )),
        None,
    )
    .await
}

async fn login(router: &axum::Router, email: &str, password: &str) -> axum::response::Response {
    call(
        router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": email, "password": password})),
    )
    .await
}

/// Registers, verifies and logs `email` in; returns the session cookie.
async fn register_verify_login(router: &axum::Router, db: &PgPool, email: &str) -> String {
    call(
        router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({
            "email": email, "password": PASSWORD, "display_name": "Purged",
            "declares_minimum_age": true
        })),
    )
    .await;
    let token: Uuid = sqlx::query_scalar(
        "SELECT t.token FROM email_verification_tokens t
         JOIN users u ON u.id = t.user_id WHERE u.email = $1",
    )
    .bind(email)
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
    set_cookie(&login(router, email, PASSWORD).await).unwrap()
}

async fn user_id(db: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn count(db: &PgPool, sql: &str, user: Uuid) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
        .bind(user)
        .fetch_one(db)
        .await
        .unwrap()
}

struct Purged {
    router: axum::Router,
    id: Uuid,
    email: String,
    /// A session cookie opened before the deletion request.
    old_session: String,
}

/// An account with a password and a Google identity, taken through
/// `POST /account/delete`, a grace period moved 31 days back, and the purge.
async fn purged_account(db: &PgPool) -> Purged {
    let email = "purged@example.test".to_string();
    let router = router_with_google_stub(db.clone(), "google-sub-purged", &email).await;
    let old_session = register_verify_login(&router, db, &email).await;
    let id = user_id(db, &email).await;
    // Binds the Google identity; the control that the stub reaches
    // `create_session`.
    let bound = google_callback(&router).await;
    assert!(bound.status().is_redirection(), "{}", bound.status());

    let request = call(
        &router,
        Method::POST,
        "/account/delete",
        Some(&old_session),
        Some(serde_json::json!({"current_password": PASSWORD})),
    )
    .await;
    assert!(request.status().is_success(), "{}", request.status());
    sqlx::query(
        "UPDATE users SET deletion_requested_at = now() - interval '31 days' WHERE id = $1",
    )
    .bind(id)
    .execute(db)
    .await
    .unwrap();
    purge_due_accounts(db).await.unwrap();
    let deleted: bool =
        sqlx::query_scalar("SELECT deleted_at IS NOT NULL FROM users WHERE id = $1")
            .bind(id)
            .fetch_one(db)
            .await
            .unwrap();
    assert!(deleted, "the purge ran");

    Purged {
        router,
        id,
        email,
        old_session,
    }
}

async fn insert_token(db: &PgPool, table: &str, user: Uuid) -> Uuid {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {table} (user_id, expires_at)
         VALUES ($1, now() + interval '1 hour') RETURNING token"
    )))
    .bind(user)
    .fetch_one(db)
    .await
    .unwrap()
}

#[sqlx::test]
async fn password_login_is_refused_under_the_old_and_the_anonymised_address(db: PgPool) {
    let purged = purged_account(&db).await;

    assert_status(
        &login(&purged.router, &purged.email, PASSWORD).await,
        StatusCode::UNAUTHORIZED,
    );
    let anonymised = format!("deleted-{}@deleted.invalid", purged.id);
    assert_status(
        &login(&purged.router, &anonymised, PASSWORD).await,
        StatusCode::UNAUTHORIZED,
    );
}

#[sqlx::test]
async fn a_session_opened_before_the_purge_is_refused(db: PgPool) {
    let purged = purged_account(&db).await;

    let me = call(
        &purged.router,
        Method::GET,
        "/auth/me",
        Some(&purged.old_session),
        None,
    )
    .await;
    assert_status(&me, StatusCode::UNAUTHORIZED);
}

/// The purge deletes the Google identity and replaces the email, so the
/// callback finds neither: signing in with the same Google account opens a
/// brand-new account. The purged one gets no session and no identity back.
#[sqlx::test]
async fn google_sign_in_never_reaches_the_purged_account(db: PgPool) {
    let purged = purged_account(&db).await;

    let response = google_callback(&purged.router).await;

    let sessions = "SELECT count(*) FROM sessions WHERE user_id = $1";
    let identities = "SELECT count(*) FROM oauth_identities WHERE user_id = $1";
    assert_eq!(count(&db, sessions, purged.id).await, 0);
    assert_eq!(count(&db, identities, purged.id).await, 0);
    assert!(response.status().is_redirection(), "{}", response.status());
    // The session went to a new account under the Google address.
    let new_id = user_id(&db, &purged.email).await;
    assert_ne!(new_id, purged.id);
    assert_eq!(count(&db, sessions, new_id).await, 1);
}

#[sqlx::test]
async fn a_password_reset_request_issues_no_token_for_the_purged_account(db: PgPool) {
    let purged = purged_account(&db).await;

    for email in [
        purged.email.clone(),
        format!("deleted-{}@deleted.invalid", purged.id),
    ] {
        call(
            &purged.router,
            Method::POST,
            "/auth/password/forgot",
            None,
            Some(serde_json::json!({"email": email})),
        )
        .await;
    }
    let tokens = "SELECT count(*) FROM password_reset_tokens WHERE user_id = $1";
    assert_eq!(count(&db, tokens, purged.id).await, 0);
}

/// Were a reset token to exist for the account, using it must not give the
/// anonymised row a password again.
#[sqlx::test]
async fn a_reset_token_cannot_give_the_purged_account_a_password(db: PgPool) {
    let purged = purged_account(&db).await;
    let token = insert_token(&db, "password_reset_tokens", purged.id).await;

    let reset = call(
        &purged.router,
        Method::POST,
        "/auth/password/reset",
        None,
        Some(serde_json::json!({"token": token, "new_password": "back-in-pass-1"})),
    )
    .await;
    assert_status(&reset, StatusCode::NOT_FOUND);
    let with_password = "SELECT count(*) FROM users WHERE id = $1 AND password_hash IS NOT NULL";
    assert_eq!(count(&db, with_password, purged.id).await, 0);
}

#[sqlx::test]
async fn a_verification_token_is_refused_for_the_purged_account(db: PgPool) {
    let purged = purged_account(&db).await;
    sqlx::query("UPDATE users SET email_verified = false WHERE id = $1")
        .bind(purged.id)
        .execute(&db)
        .await
        .unwrap();
    let token = insert_token(&db, "email_verification_tokens", purged.id).await;

    let verify = call(
        &purged.router,
        Method::GET,
        &format!("/auth/verify-email?token={token}"),
        None,
        None,
    )
    .await;
    assert_status(&verify, StatusCode::NOT_FOUND);
    let verified = "SELECT count(*) FROM users WHERE id = $1 AND email_verified";
    assert_eq!(count(&db, verified, purged.id).await, 0);
}

#[sqlx::test]
async fn an_invitation_cannot_be_accepted_with_the_old_session(db: PgPool) {
    let purged = purged_account(&db).await;
    let owner: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, display_name)
         VALUES ('host@example.test', 'not-a-real-hash', 'Host') RETURNING id",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group: Uuid = sqlx::query_scalar(
        "INSERT INTO groups (name, created_by) VALUES ('Hôtes', $1) RETURNING id",
    )
    .bind(owner)
    .fetch_one(&db)
    .await
    .unwrap();
    let token: Uuid = sqlx::query_scalar(
        "INSERT INTO invitations (group_id, created_by, expires_at)
         VALUES ($1, $2, now() + interval '7 days') RETURNING token",
    )
    .bind(group)
    .bind(owner)
    .fetch_one(&db)
    .await
    .unwrap();

    let accept = call(
        &purged.router,
        Method::POST,
        &format!("/groups/invitations/{token}/accept"),
        Some(&purged.old_session),
        None,
    )
    .await;
    assert_status(&accept, StatusCode::UNAUTHORIZED);
    let memberships = "SELECT count(*) FROM group_members WHERE user_id = $1";
    assert_eq!(count(&db, memberships, purged.id).await, 0);
}

/// Support deactivation sets `deactivated_at` and keeps the tokens, which the
/// hourly retention purge only takes later: here a token genuinely exists
/// for an account that must stay locked out.
#[sqlx::test]
async fn the_token_routes_refuse_a_deactivated_account_too(db: PgPool) {
    let router = common::test_router(db.clone());
    let email = "locked@example.test";
    register_verify_login(&router, &db, email).await;
    let id = user_id(&db, email).await;
    let reset_token = insert_token(&db, "password_reset_tokens", id).await;
    let verify_token = insert_token(&db, "email_verification_tokens", id).await;
    sqlx::query("UPDATE users SET deactivated_at = now(), email_verified = false WHERE id = $1")
        .bind(id)
        .execute(&db)
        .await
        .unwrap();

    let reset = call(
        &router,
        Method::POST,
        "/auth/password/reset",
        None,
        Some(serde_json::json!({"token": reset_token, "new_password": "back-in-pass-1"})),
    )
    .await;
    assert_status(&reset, StatusCode::NOT_FOUND);
    let verify = call(
        &router,
        Method::GET,
        &format!("/auth/verify-email?token={verify_token}"),
        None,
        None,
    )
    .await;
    assert_status(&verify, StatusCode::NOT_FOUND);
    assert_status(
        &login(&router, email, PASSWORD).await,
        StatusCode::UNAUTHORIZED,
    );
    assert_status(
        &login(&router, email, "back-in-pass-1").await,
        StatusCode::UNAUTHORIZED,
    );
    let verified = "SELECT count(*) FROM users WHERE id = $1 AND email_verified";
    assert_eq!(count(&db, verified, id).await, 0);
}
