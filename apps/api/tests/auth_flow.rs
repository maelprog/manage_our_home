mod common;

use axum::http::{Method, StatusCode};
use common::{assert_status, call, set_cookie, test_router};
use sqlx::PgPool;
use uuid::Uuid;

/// AC #1, #2: register, then a duplicate email is rejected generically.
#[sqlx::test]
async fn register_then_duplicate_email_conflicts(db: PgPool) {
    let router = test_router(db.clone());

    let res = call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({
            "email": "alice@example.test",
            "password": "correct horse battery staple",
            "display_name": "Alice",
        })),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);

    let dup = call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({
            "email": "alice@example.test",
            "password": "another password",
            "display_name": "Alice 2",
        })),
    )
    .await;
    assert_status(&dup, StatusCode::CONFLICT);

    let user = sqlx::query!("SELECT email_verified FROM users WHERE email = 'alice@example.test'")
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(!user.email_verified);
}

/// AC #1: login is refused until the verification link is consumed;
/// consuming it flips `email_verified` and unlocks password login.
#[sqlx::test]
async fn verify_email_unlocks_login(db: PgPool) {
    let router = test_router(db.clone());

    call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({
            "email": "bob@example.test",
            "password": "hunter2hunter2",
            "display_name": "Bob",
        })),
    )
    .await;

    let login_before = call(
        &router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "bob@example.test", "password": "hunter2hunter2"})),
    )
    .await;
    assert_status(&login_before, StatusCode::UNAUTHORIZED);

    let token = sqlx::query_scalar!(
        r#"
        SELECT t.token FROM email_verification_tokens t
        JOIN users u ON u.id = t.user_id
        WHERE u.email = 'bob@example.test'
        "#
    )
    .fetch_one(&db)
    .await
    .unwrap();

    let verify = call(
        &router,
        Method::GET,
        &format!("/auth/verify-email?token={token}"),
        None,
        None,
    )
    .await;
    assert_status(&verify, StatusCode::OK);

    let login_after = call(
        &router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "bob@example.test", "password": "hunter2hunter2"})),
    )
    .await;
    assert_status(&login_after, StatusCode::OK);
    assert!(set_cookie(&login_after).is_some());
}

/// AC #4: forgot-password gives an identical response for existing and
/// non-existing accounts, and resetting revokes all active sessions.
#[sqlx::test]
async fn forgot_password_is_anti_enumeration_and_reset_revokes_sessions(db: PgPool) {
    let router = test_router(db.clone());

    call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({
            "email": "carol@example.test",
            "password": "initial-password",
            "display_name": "Carol",
        })),
    )
    .await;
    let token = sqlx::query_scalar!(
        "SELECT token FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = 'carol@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    call(
        &router,
        Method::GET,
        &format!("/auth/verify-email?token={token}"),
        None,
        None,
    )
    .await;

    let login = call(
        &router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "carol@example.test", "password": "initial-password"})),
    )
    .await;
    let cookie = set_cookie(&login).unwrap();

    let known = call(
        &router,
        Method::POST,
        "/auth/password/forgot",
        None,
        Some(serde_json::json!({"email": "carol@example.test"})),
    )
    .await;
    let unknown = call(
        &router,
        Method::POST,
        "/auth/password/forgot",
        None,
        Some(serde_json::json!({"email": "no-such-user@example.test"})),
    )
    .await;
    assert_eq!(known.status(), unknown.status());

    let reset_token = sqlx::query_scalar!(
        "SELECT token FROM password_reset_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = 'carol@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();

    let reset = call(
        &router,
        Method::POST,
        "/auth/password/reset",
        None,
        Some(serde_json::json!({"token": reset_token, "new_password": "brand-new-password"})),
    )
    .await;
    assert_status(&reset, StatusCode::OK);

    let logout_attempt = call(&router, Method::POST, "/auth/logout", Some(&cookie), None).await;
    assert_status(&logout_attempt, StatusCode::UNAUTHORIZED);
}

/// AC #5: changing password authenticated requires the current password
/// and keeps the calling session while revoking the rest.
#[sqlx::test]
async fn change_password_keeps_current_session_revokes_others(db: PgPool) {
    let router = test_router(db.clone());
    call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": "dave@example.test", "password": "old-password-1", "display_name": "Dave"})),
    )
    .await;
    let token = sqlx::query_scalar!(
        "SELECT token FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = 'dave@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    call(
        &router,
        Method::GET,
        &format!("/auth/verify-email?token={token}"),
        None,
        None,
    )
    .await;

    let login1 = call(
        &router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "dave@example.test", "password": "old-password-1"})),
    )
    .await;
    let cookie1 = set_cookie(&login1).unwrap();
    let login2 = call(
        &router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "dave@example.test", "password": "old-password-1"})),
    )
    .await;
    let cookie2 = set_cookie(&login2).unwrap();

    let bad_change = call(
        &router,
        Method::POST,
        "/settings/password/change",
        Some(&cookie1),
        Some(serde_json::json!({"current_password": "wrong", "new_password": "new-password-1"})),
    )
    .await;
    assert_status(&bad_change, StatusCode::UNAUTHORIZED);

    let change = call(
        &router,
        Method::POST,
        "/settings/password/change",
        Some(&cookie1),
        Some(serde_json::json!({"current_password": "old-password-1", "new_password": "new-password-1"})),
    )
    .await;
    assert_status(&change, StatusCode::OK);

    let still_works = call(&router, Method::POST, "/auth/logout", Some(&cookie1), None).await;
    assert_status(&still_works, StatusCode::NO_CONTENT);

    let other_session_dead =
        call(&router, Method::POST, "/auth/logout", Some(&cookie2), None).await;
    assert_status(&other_session_dead, StatusCode::UNAUTHORIZED);
}

/// AC #6: account deletion is blocked while owning a group, and can be
/// cancelled within the grace window once unblocked.
#[sqlx::test]
async fn delete_account_blocked_while_owner_then_cancellable(db: PgPool) {
    let router = test_router(db.clone());
    call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": "erin@example.test", "password": "erins-password1", "display_name": "Erin"})),
    )
    .await;
    let token = sqlx::query_scalar!(
        "SELECT token FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = 'erin@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    call(
        &router,
        Method::GET,
        &format!("/auth/verify-email?token={token}"),
        None,
        None,
    )
    .await;
    let login = call(
        &router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "erin@example.test", "password": "erins-password1"})),
    )
    .await;
    let cookie = set_cookie(&login).unwrap();

    let create_group = call(
        &router,
        Method::POST,
        "/groups",
        Some(&cookie),
        Some(serde_json::json!({"name": "Famille Erin"})),
    )
    .await;
    assert_status(&create_group, StatusCode::CREATED);

    let blocked = call(
        &router,
        Method::POST,
        "/account/delete",
        Some(&cookie),
        Some(serde_json::json!({"current_password": "erins-password1"})),
    )
    .await;
    assert_status(&blocked, StatusCode::CONFLICT);

    let group: Uuid = sqlx::query_scalar!(
        "SELECT g.id FROM groups g JOIN users u ON u.id = g.created_by WHERE u.email = 'erin@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let delete_group = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group}"),
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&delete_group, StatusCode::NO_CONTENT);

    let allowed = call(
        &router,
        Method::POST,
        "/account/delete",
        Some(&cookie),
        Some(serde_json::json!({"current_password": "erins-password1"})),
    )
    .await;
    assert_status(&allowed, StatusCode::OK);

    let cancel = call(
        &router,
        Method::POST,
        "/account/delete/cancel",
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&cancel, StatusCode::OK);

    let user_row =
        sqlx::query!("SELECT deletion_requested_at FROM users WHERE email = 'erin@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(user_row.deletion_requested_at.is_none());
}

/// AC (#27) case 1: for an unverified account, resend invalidates the
/// outstanding verification token and issues a fresh one that verifies the
/// email end-to-end. The cooldown is stepped past by ageing the token that
/// registration just created.
#[sqlx::test]
async fn resend_verification_invalidates_old_token_and_new_one_works(db: PgPool) {
    let router = test_router(db.clone());

    call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({
            "email": "fred@example.test",
            "password": "initial-password",
            "display_name": "Fred",
        })),
    )
    .await;

    let old_token = sqlx::query_scalar!(
        "SELECT token FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = 'fred@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();

    // Age the registration token past the cooldown window.
    sqlx::query!(
        "UPDATE email_verification_tokens SET created_at = now() - interval '10 minutes' WHERE token = $1",
        old_token
    )
    .execute(&db)
    .await
    .unwrap();

    let resend = call(
        &router,
        Method::POST,
        "/auth/verify-email/resend",
        None,
        Some(serde_json::json!({"email": "fred@example.test"})),
    )
    .await;
    assert_status(&resend, StatusCode::OK);

    // Old token is now consumed and can no longer verify the email.
    let old_verify = call(
        &router,
        Method::GET,
        &format!("/auth/verify-email?token={old_token}"),
        None,
        None,
    )
    .await;
    assert_status(&old_verify, StatusCode::GONE);

    // A fresh, unconsumed token was issued; it verifies the email.
    let new_token = sqlx::query_scalar!(
        "SELECT token FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = 'fred@example.test' AND t.consumed_at IS NULL"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_ne!(old_token, new_token);

    let new_verify = call(
        &router,
        Method::GET,
        &format!("/auth/verify-email?token={new_token}"),
        None,
        None,
    )
    .await;
    assert_status(&new_verify, StatusCode::OK);

    let login = call(
        &router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "fred@example.test", "password": "initial-password"})),
    )
    .await;
    assert_status(&login, StatusCode::OK);
}

/// AC (#27) case 2: unknown email and already-verified account both return
/// 200 with no token created and no email sent (anti-enumeration).
#[sqlx::test]
async fn resend_verification_noops_for_unknown_and_verified(db: PgPool) {
    let router = test_router(db.clone());

    // Unknown email: 200, and no token row exists for it.
    let unknown = call(
        &router,
        Method::POST,
        "/auth/verify-email/resend",
        None,
        Some(serde_json::json!({"email": "ghost@example.test"})),
    )
    .await;
    assert_status(&unknown, StatusCode::OK);

    // Register and fully verify an account.
    call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({
            "email": "grace@example.test",
            "password": "initial-password",
            "display_name": "Grace",
        })),
    )
    .await;
    let token = sqlx::query_scalar!(
        "SELECT token FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = 'grace@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    call(
        &router,
        Method::GET,
        &format!("/auth/verify-email?token={token}"),
        None,
        None,
    )
    .await;

    let tokens_before = sqlx::query_scalar!(
        "SELECT count(*) FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = 'grace@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();

    // Already verified: 200, no new token issued.
    let verified = call(
        &router,
        Method::POST,
        "/auth/verify-email/resend",
        None,
        Some(serde_json::json!({"email": "grace@example.test"})),
    )
    .await;
    assert_status(&verified, StatusCode::OK);

    let tokens_after = sqlx::query_scalar!(
        "SELECT count(*) FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = 'grace@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(tokens_before, tokens_after);
}

/// AC (#27) case 3: a second resend inside the 5-minute window is a silent
/// no-op — no new token is created.
#[sqlx::test]
async fn resend_verification_cooldown_is_silent_noop(db: PgPool) {
    let router = test_router(db.clone());

    call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({
            "email": "heidi@example.test",
            "password": "initial-password",
            "display_name": "Heidi",
        })),
    )
    .await;

    let old_token = sqlx::query_scalar!(
        "SELECT token FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = 'heidi@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();

    // Age the registration token so the first resend actually issues one.
    sqlx::query!(
        "UPDATE email_verification_tokens SET created_at = now() - interval '10 minutes' WHERE token = $1",
        old_token
    )
    .execute(&db)
    .await
    .unwrap();

    let first = call(
        &router,
        Method::POST,
        "/auth/verify-email/resend",
        None,
        Some(serde_json::json!({"email": "heidi@example.test"})),
    )
    .await;
    assert_status(&first, StatusCode::OK);

    let count_after_first = sqlx::query_scalar!(
        "SELECT count(*) FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = 'heidi@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();

    // Second resend within the cooldown window: no-op.
    let second = call(
        &router,
        Method::POST,
        "/auth/verify-email/resend",
        None,
        Some(serde_json::json!({"email": "heidi@example.test"})),
    )
    .await;
    assert_status(&second, StatusCode::OK);

    let count_after_second = sqlx::query_scalar!(
        "SELECT count(*) FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = 'heidi@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(count_after_first, count_after_second);
}
