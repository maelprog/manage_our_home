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
        Some(
            serde_json::json!({"email": email, "password": password, "display_name": "Test User"}),
        ),
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

/// `GET /auth/me` returns the caller's identity when authenticated, and 401
/// with `{"error":"unauthorized"}` when there is no valid session.
#[sqlx::test]
async fn me_returns_identity_when_authed_and_401_otherwise(db: PgPool) {
    let router = test_router(db.clone());

    let no_session = call(&router, Method::GET, "/auth/me", None, None).await;
    assert_status(&no_session, StatusCode::UNAUTHORIZED);
    let body = json_body(no_session).await;
    assert_eq!(body["error"], "unauthorized");

    let cookie = register_verify_login(&router, &db, "me@example.test", "me-password1").await;
    let authed = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&authed, StatusCode::OK);
    let me = json_body(authed).await;
    assert_eq!(me["email"], "me@example.test");
    assert_eq!(me["display_name"], "Test User");
    assert_eq!(me["email_verified"], true);
    assert!(me["user_id"].is_string());
}

/// AC #6: register rejects invalid input with the exact 422 codes, and a
/// valid registration is unaffected.
#[sqlx::test]
async fn register_validates_input(db: PgPool) {
    let router = test_router(db.clone());

    let short_pw = call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": "v@example.test", "password": "short", "display_name": "V"})),
    )
    .await;
    assert_status(&short_pw, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json_body(short_pw).await["error"], "password_too_short");

    let bad_email = call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": "not-an-email", "password": "long-enough-1", "display_name": "V"})),
    )
    .await;
    assert_status(&bad_email, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json_body(bad_email).await["error"], "invalid_email");

    let empty_name = call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": "v@example.test", "password": "long-enough-1", "display_name": "   "})),
    )
    .await;
    assert_status(&empty_name, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json_body(empty_name).await["error"],
        "display_name_required"
    );

    let ok = call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": "v@example.test", "password": "long-enough-1", "display_name": "Valid"})),
    )
    .await;
    assert_status(&ok, StatusCode::CREATED);
}

/// AC #6: the authenticated change-password endpoint rejects a too-short new
/// password with `password_too_short` (422), even with the correct current
/// password.
#[sqlx::test]
async fn change_password_rejects_short_new_password(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "cp@example.test", "old-password-1").await;

    let res = call(
        &router,
        Method::POST,
        "/settings/password/change",
        Some(&cookie),
        Some(serde_json::json!({"current_password": "old-password-1", "new_password": "short"})),
    )
    .await;
    assert_status(&res, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json_body(res).await["error"], "password_too_short");
}

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

// --- #178: the login enumeration oracle, and the lock that bounds the cost
// of closing it ---

/// A router whose state trusts `10.0.0.0/8` as a proxy range, so the tests
/// below can pose as several distinct clients. `common::test_state` trusts
/// nobody, which is right for every other test: without a trust list the
/// peer address is the client and `X-Forwarded-For` is ignored.
fn router_trusting_10_0_0_0_8(db: PgPool) -> axum::Router {
    let mut state = common::test_state(db);
    state.trusted_proxies = std::sync::Arc::new(
        manage_our_home::client_ip::TrustedProxies::parse("10.0.0.0/8").unwrap(),
    );
    manage_our_home::build_router(state)
}

/// `POST /auth/login` from a named peer, optionally carrying an
/// `X-Forwarded-For`. `common::call` cannot do this: it drives the router
/// directly, with no socket behind the request.
async fn login_from(
    router: &axum::Router,
    peer: &str,
    forwarded_for: Option<&str>,
    email: &str,
    password: &str,
) -> axum::http::Response<axum::body::Body> {
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::{header, Request};
    use tower::ServiceExt;

    let peer: std::net::SocketAddr = peer.parse().unwrap();
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .extension(ConnectInfo(peer));
    if let Some(value) = forwarded_for {
        builder = builder.header("x-forwarded-for", value);
    }
    let body = serde_json::json!({"email": email, "password": password});
    let request = builder
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    router.clone().oneshot(request).await.unwrap()
}

/// The fix for #178: the three regimes of `login_inner` must be
/// indistinguishable, in the response *and* on a stopwatch.
///
/// Before this, an unknown email answered in ~0,22 ms and a known one in
/// ~256 ms because only the second reached argon2id — three orders of
/// magnitude, readable with no credentials at all. The Google-only account
/// (`password_hash IS NULL`) was the third regime and the finer leak: it
/// short-circuited like an unknown email while meaning "this account exists
/// and has no password".
///
/// The bound below is deliberately loose (a quarter of the slowest branch)
/// because this measures wall time on a shared runner. It is three orders
/// of magnitude away from what the bug produced, so it separates "pays for
/// argon2id" from "does not" without pretending to measure the difference
/// between two hashes.
#[sqlx::test]
async fn the_three_login_regimes_answer_alike_and_take_comparable_time(db: PgPool) {
    let router = test_router(db.clone());

    // Regime (c): a real account with a password hash.
    register_verify_login(&router, &db, "known@example.test", "known-password1").await;
    // Regime (b): Google-only — a row with no password hash at all. The
    // oauth identity goes in the same transaction because `users` refuses
    // a row with no auth method at all (deferred trigger, migration 0001).
    let mut tx = db.begin().await.unwrap();
    let google_only = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ($1, NULL, 'Google Only', true) RETURNING id",
        "google-only@example.test"
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO oauth_identities (user_id, provider, provider_user_id) VALUES ($1, 'google', $2)",
        google_only,
        "google-subject-178"
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut elapsed = Vec::new();
    let mut bodies = Vec::new();
    for email in [
        "unknown@example.test",
        "google-only@example.test",
        "known@example.test",
    ] {
        let started = std::time::Instant::now();
        let response = call(
            &router,
            Method::POST,
            "/auth/login",
            None,
            Some(serde_json::json!({"email": email, "password": "wrong-password1"})),
        )
        .await;
        elapsed.push(started.elapsed());
        assert_status(&response, StatusCode::UNAUTHORIZED);
        bodies.push(json_body(response).await);
    }

    assert_eq!(bodies[0], bodies[1], "unknown vs Google-only");
    assert_eq!(bodies[1], bodies[2], "Google-only vs known");

    let slowest = *elapsed.iter().max().unwrap();
    let fastest = *elapsed.iter().min().unwrap();
    assert!(
        fastest >= slowest / 4,
        "one branch skipped the hashing: {elapsed:?}"
    );
}

/// The lock of #178 (piste 3), consulted before the argon2 work the decoy
/// hash added to every invalid attempt.
#[sqlx::test]
async fn repeated_failures_from_one_address_are_locked_out(db: PgPool) {
    let router = router_trusting_10_0_0_0_8(db.clone());
    let peer = "10.0.0.2:40000";

    for _ in 0..manage_our_home::auth::throttle::MAX_FAILURES {
        let response = login_from(
            &router,
            peer,
            Some("192.168.1.42"),
            "nobody@example.test",
            "wrong-password1",
        )
        .await;
        assert_status(&response, StatusCode::UNAUTHORIZED);
    }

    let locked = login_from(
        &router,
        peer,
        Some("192.168.1.42"),
        "nobody@example.test",
        "wrong-password1",
    )
    .await;
    assert_status(&locked, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json_body(locked).await["error"], "too_many_attempts");
}

/// The key is the pair, never the email alone: locking one pair must not
/// lock the account for the rest of the household, nor the address for the
/// rest of the accounts.
#[sqlx::test]
async fn the_lock_is_scoped_to_one_address_and_email_pair(db: PgPool) {
    let router = router_trusting_10_0_0_0_8(db.clone());
    let proxy = "10.0.0.2:40000";
    register_verify_login(&router, &db, "victim@example.test", "victim-password1").await;

    for _ in 0..manage_our_home::auth::throttle::MAX_FAILURES {
        login_from(
            &router,
            proxy,
            Some("203.0.113.9"),
            "victim@example.test",
            "wrong-password1",
        )
        .await;
    }
    let attacker_again = login_from(
        &router,
        proxy,
        Some("203.0.113.9"),
        "victim@example.test",
        "wrong-password1",
    )
    .await;
    assert_status(&attacker_again, StatusCode::TOO_MANY_REQUESTS);

    // The owner, from their own address, is untouched — with the right
    // password *and* with a wrong one.
    let owner_wrong = login_from(
        &router,
        proxy,
        Some("192.168.1.42"),
        "victim@example.test",
        "wrong-password1",
    )
    .await;
    assert_status(&owner_wrong, StatusCode::UNAUTHORIZED);
    let owner_right = login_from(
        &router,
        proxy,
        Some("192.168.1.42"),
        "victim@example.test",
        "victim-password1",
    )
    .await;
    assert_status(&owner_right, StatusCode::OK);

    // And the attacker's address can still reach another account.
    let other_account = login_from(
        &router,
        proxy,
        Some("203.0.113.9"),
        "someone-else@example.test",
        "wrong-password1",
    )
    .await;
    assert_status(&other_account, StatusCode::UNAUTHORIZED);
}

/// The trap the arbitration on #178 called non-negotiable: an
/// `X-Forwarded-For` from a peer that is not a trusted proxy is worth
/// nothing. If it were honoured, rotating the header would buy an
/// unlimited number of attempts — and naming a neighbour's address would
/// lock *them* out.
#[sqlx::test]
async fn a_forged_forwarded_for_from_an_untrusted_peer_buys_no_extra_attempts(db: PgPool) {
    let router = router_trusting_10_0_0_0_8(db.clone());

    for i in 0..manage_our_home::auth::throttle::MAX_FAILURES {
        let response = login_from(
            &router,
            // 203.0.113.x is outside the trusted 10.0.0.0/8, so this peer
            // is a client, not a proxy.
            "203.0.113.9:40000",
            Some(&format!("198.51.100.{i}")),
            "nobody@example.test",
            "wrong-password1",
        )
        .await;
        assert_status(&response, StatusCode::UNAUTHORIZED);
    }

    let next = login_from(
        &router,
        "203.0.113.9:40000",
        Some("198.51.100.200"),
        "nobody@example.test",
        "wrong-password1",
    )
    .await;
    assert_status(&next, StatusCode::TOO_MANY_REQUESTS);
}

/// A login that succeeds clears what came before it, so a household member
/// who mistypes their way to the edge of the lock and then gets it right
/// starts from a clean slate.
#[sqlx::test]
async fn a_successful_login_clears_the_failures_before_it(db: PgPool) {
    let router = router_trusting_10_0_0_0_8(db.clone());
    let proxy = "10.0.0.2:40000";
    register_verify_login(&router, &db, "clumsy@example.test", "clumsy-password1").await;

    for _ in 0..(manage_our_home::auth::throttle::MAX_FAILURES - 1) {
        login_from(
            &router,
            proxy,
            Some("192.168.1.42"),
            "clumsy@example.test",
            "wrong-password1",
        )
        .await;
    }
    let right = login_from(
        &router,
        proxy,
        Some("192.168.1.42"),
        "clumsy@example.test",
        "clumsy-password1",
    )
    .await;
    assert_status(&right, StatusCode::OK);

    for _ in 0..(manage_our_home::auth::throttle::MAX_FAILURES - 1) {
        let response = login_from(
            &router,
            proxy,
            Some("192.168.1.42"),
            "clumsy@example.test",
            "wrong-password1",
        )
        .await;
        assert_status(&response, StatusCode::UNAUTHORIZED);
    }
}

/// A burst on one pair gets exactly [`MAX_FAILURES`] argon2id runs, however
/// many requests arrive at once. Counting only once a refusal was known let
/// every request of a concurrent burst read "not locked" before the first
/// one finished hashing: 40 at once came back as 40 × 401 and 0 × 429.
#[sqlx::test]
async fn a_concurrent_burst_on_one_pair_is_bounded_by_the_threshold(db: PgPool) {
    let router = router_trusting_10_0_0_0_8(db.clone());
    let burst = 40;

    let responses = futures::future::join_all((0..burst).map(|_| {
        login_from(
            &router,
            "10.0.0.2:40000",
            Some("192.168.1.42"),
            "nobody@example.test",
            "wrong-password1",
        )
    }))
    .await;

    let unauthorized = responses
        .iter()
        .filter(|r| r.status() == StatusCode::UNAUTHORIZED)
        .count();
    let locked = responses
        .iter()
        .filter(|r| r.status() == StatusCode::TOO_MANY_REQUESTS)
        .count();
    let max = manage_our_home::auth::throttle::MAX_FAILURES as usize;
    assert_eq!(unauthorized, max, "hashes paid by the burst");
    assert_eq!(locked, burst - max);
}

/// The lock is consulted before argon2id, not after: a locked attempt
/// answers without paying for a hash. If the check moved behind
/// `verify_password`, the 429 would cost what a 401 costs.
///
/// Compared against a refused attempt measured in the same test, with a
/// loose factor, because this is wall time on a shared runner: on the
/// debug profile a hash is a couple of hundred milliseconds and a locked
/// answer a few.
#[sqlx::test]
async fn a_locked_attempt_answers_without_hashing(db: PgPool) {
    let router = router_trusting_10_0_0_0_8(db.clone());
    let proxy = "10.0.0.2:40000";

    let mut refused = std::time::Duration::MAX;
    for _ in 0..manage_our_home::auth::throttle::MAX_FAILURES {
        let started = std::time::Instant::now();
        let response = login_from(
            &router,
            proxy,
            Some("192.168.1.42"),
            "nobody@example.test",
            "wrong-password1",
        )
        .await;
        refused = refused.min(started.elapsed());
        assert_status(&response, StatusCode::UNAUTHORIZED);
    }

    let started = std::time::Instant::now();
    let locked = login_from(
        &router,
        proxy,
        Some("192.168.1.42"),
        "nobody@example.test",
        "wrong-password1",
    )
    .await;
    let locked_elapsed = started.elapsed();
    assert_status(&locked, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        locked_elapsed * 4 < refused,
        "a locked attempt took {locked_elapsed:?}, the fastest refusal {refused:?}"
    );
}

/// Every `Set-Cookie` of a response, as `name=value; attributes` lines.
fn set_cookies(response: &axum::response::Response) -> Vec<String> {
    response
        .headers()
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect()
}

fn cookie_value(set_cookies: &[String], name: &str) -> Option<String> {
    set_cookies.iter().find_map(|line| {
        line.split(';')
            .next()
            .and_then(|pair| pair.strip_prefix(&format!("{name}=")))
            .map(str::to_string)
    })
}

/// #193: `start` sends Google an S256 PKCE challenge and keeps the matching
/// verifier in an HttpOnly cookie beside the CSRF `state`, never in the URL.
#[sqlx::test]
async fn google_start_sends_an_s256_challenge_and_keeps_the_verifier_in_a_cookie(db: PgPool) {
    let router = test_router(db);

    let response = call(&router, Method::GET, "/auth/google/start", None, None).await;
    assert!(response.status().is_redirection());
    let location = response
        .headers()
        .get(axum::http::header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let url = oauth2::url::Url::parse(&location).unwrap();
    let param = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };

    let cookies = set_cookies(&response);
    let verifier = cookie_value(&cookies, "google_oauth_pkce_verifier").expect("verifier cookie");
    let state = cookie_value(&cookies, "google_oauth_state").expect("state cookie");

    assert_eq!(param("code_challenge_method").as_deref(), Some("S256"));
    let expected = oauth2::PkceCodeChallenge::from_code_verifier_sha256(
        &oauth2::PkceCodeVerifier::new(verifier.clone()),
    );
    assert_eq!(param("code_challenge").as_deref(), Some(expected.as_str()));
    assert_eq!(param("state").as_deref(), Some(state.as_str()));
    assert!(
        !location.contains(&verifier),
        "verifier leaked into the URL"
    );

    let verifier_line = cookies
        .iter()
        .find(|l| l.starts_with("google_oauth_pkce_verifier="))
        .unwrap();
    assert!(verifier_line.contains("HttpOnly"));
    assert!(verifier_line.contains("Path=/"));
    assert!(verifier_line.contains("SameSite=Lax"));
}

/// Asserts `response` expires `name` for the whole site: an expiry without
/// `start`'s `Path=/` would leave the browser's cookie in place.
fn assert_cleared(response: &axum::response::Response, name: &str) {
    let cookies = set_cookies(response);
    let line = cookies
        .iter()
        .find(|l| l.starts_with(&format!("{name}=")))
        .unwrap_or_else(|| panic!("{name} not cleared"));
    assert!(line.contains("Max-Age=0"), "{line}");
    assert!(line.contains("Path=/"), "{line}");
}

/// #193: a callback whose `state` checks out but whose PKCE verifier cookie
/// is gone is refused before any code exchange — never retried without
/// PKCE. (An attempted exchange would answer 500, not 401: the test client
/// posts a dummy client id to Google's real token endpoint, and `callback`
/// maps any exchange failure to 500.)
#[sqlx::test]
async fn google_callback_without_the_pkce_verifier_is_refused_before_exchange(db: PgPool) {
    let router = test_router(db);

    let response = call(
        &router,
        Method::GET,
        "/auth/google/callback?code=injected-code&state=s",
        Some("google_oauth_state=s"),
        None,
    )
    .await;
    assert_status(&response, StatusCode::UNAUTHORIZED);
    assert_cleared(&response, "google_oauth_state");
}

/// #193: the verifier does not stand in for the CSRF check — a mismatched
/// `state` is still refused with the verifier cookie present, and both
/// single-use flow cookies are cleared.
#[sqlx::test]
async fn google_callback_with_a_verifier_but_a_mismatched_state_is_refused(db: PgPool) {
    let router = test_router(db);

    let verifier = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFG";
    let response = call(
        &router,
        Method::GET,
        "/auth/google/callback?code=c&state=forged",
        Some(&format!(
            "google_oauth_state=s; google_oauth_pkce_verifier={verifier}"
        )),
        None,
    )
    .await;
    assert_status(&response, StatusCode::UNAUTHORIZED);
    assert_cleared(&response, "google_oauth_state");
    assert_cleared(&response, "google_oauth_pkce_verifier");
}

/// #193: the verifier `start` stashed is what `callback` sends with the
/// code. The token endpoint is a local listener recording the exchange's
/// form body, so the assertion is on the request itself, not on a flag.
/// The response status is not asserted: after the exchange, `callback`
/// queries Google's live userinfo endpoint with the fake access token.
#[sqlx::test]
async fn google_callback_sends_the_stored_pkce_verifier_with_the_code(db: PgPool) {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    let recorded: Arc<Mutex<Option<HashMap<String, String>>>> = Arc::default();
    let token_endpoint = axum::Router::new().route(
        "/token",
        axum::routing::post({
            let recorded = recorded.clone();
            move |axum::extract::Form(form): axum::extract::Form<HashMap<String, String>>| async move {
                *recorded.lock().unwrap() = Some(form);
                axum::Json(serde_json::json!({
                    "access_token": "local-access-token",
                    "token_type": "bearer",
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let token_url = format!("http://{}/token", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, token_endpoint).await.unwrap() });

    let mut state = common::test_state(db);
    state.google_oauth = state
        .google_oauth
        .set_token_uri(oauth2::TokenUrl::new(token_url).unwrap());
    let router = manage_our_home::build_router(state);

    let verifier = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFG";
    call(
        &router,
        Method::GET,
        "/auth/google/callback?code=the-code&state=s",
        Some(&format!(
            "google_oauth_state=s; google_oauth_pkce_verifier={verifier}"
        )),
        None,
    )
    .await;

    let form = recorded
        .lock()
        .unwrap()
        .take()
        .expect("callback never reached the token endpoint");
    assert_eq!(form.get("code").map(String::as_str), Some("the-code"));
    assert_eq!(
        form.get("code_verifier").map(String::as_str),
        Some(verifier)
    );
}

/// Stands in for Google on both legs `callback` takes after its state
/// check: the token endpoint answers any code with an access token, the
/// userinfo endpoint with a verified profile for `sub` and `email`. Returns
/// a router whose state points at both.
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

/// A callback whose state and PKCE cookies check out, as the browser sends
/// it on its way back from Google's consent screen.
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

async fn user_id_by_email(db: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar!("SELECT id FROM users WHERE email = $1", email)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn session_rows(db: &PgPool, user_id: Uuid) -> i64 {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM sessions WHERE user_id = $1"#,
        user_id
    )
    .fetch_one(db)
    .await
    .unwrap()
}

async fn google_identity_rows(db: &PgPool, user_id: Uuid) -> i64 {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM oauth_identities WHERE user_id = $1"#,
        user_id
    )
    .fetch_one(db)
    .await
    .unwrap()
}

/// Locks `user_id` the way support does: `POST /admin/users/:id/deactivate`
/// from a superadmin session.
async fn deactivate_through_support(router: &axum::Router, db: &PgPool, user_id: Uuid) {
    let admin =
        register_verify_login(router, db, "support-194@example.test", "support-pass-1").await;
    sqlx::query!(
        "UPDATE users SET is_superadmin = true WHERE email = $1",
        "support-194@example.test"
    )
    .execute(db)
    .await
    .unwrap();
    let res = call(
        router,
        Method::POST,
        &format!("/admin/users/{user_id}/deactivate"),
        Some(&admin),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
}

fn opens_a_session(response: &axum::response::Response) -> bool {
    cookie_value(&set_cookies(response), "session_id").is_some_and(|v| !v.is_empty())
}

/// #194: an account support deactivated keeps its email and its Google
/// identity, so the identity branch of `callback` still finds it. Signing
/// in with Google again is refused and writes no `sessions` row: the lock
/// promises the account has no live session, not merely none that works.
/// The first sign-in, before the lock, is the control — it shows the stub
/// carries `callback` all the way to `create_session`, so the refusal
/// afterwards comes from the lock and not from the stub.
#[sqlx::test]
async fn google_callback_opens_no_session_for_an_account_deactivated_by_support(db: PgPool) {
    let email = "locked-194@example.test";
    let router = router_with_google_stub(db.clone(), "google-sub-194", email).await;
    register_verify_login(&router, &db, email, "locked-pass-1").await;
    let user_id = user_id_by_email(&db, email).await;

    let before_lock = google_callback(&router).await;
    assert!(
        before_lock.status().is_redirection(),
        "{}",
        before_lock.status()
    );
    assert!(opens_a_session(&before_lock));
    assert_eq!(google_identity_rows(&db, user_id).await, 1);

    deactivate_through_support(&router, &db, user_id).await;
    let sessions_at_lock = session_rows(&db, user_id).await;

    let after_lock = google_callback(&router).await;
    assert_status(&after_lock, StatusCode::UNAUTHORIZED);
    assert!(!opens_a_session(&after_lock));
    assert_eq!(session_rows(&db, user_id).await, sessions_at_lock);
}

/// #194: a deactivated account with no Google identity yet is reached by
/// the email branch of `callback`. The refusal leaves nothing behind: no
/// session, and no identity bound for a later attempt to come back in
/// through the identity branch.
#[sqlx::test]
async fn google_callback_binds_no_identity_to_an_account_deactivated_by_support(db: PgPool) {
    let email = "locked-no-google-194@example.test";
    let router = router_with_google_stub(db.clone(), "google-sub-194-new", email).await;
    register_verify_login(&router, &db, email, "locked-pass-1").await;
    let user_id = user_id_by_email(&db, email).await;

    deactivate_through_support(&router, &db, user_id).await;
    let sessions_at_lock = session_rows(&db, user_id).await;

    let response = google_callback(&router).await;
    assert_status(&response, StatusCode::UNAUTHORIZED);
    assert!(!opens_a_session(&response));
    assert_eq!(session_rows(&db, user_id).await, sessions_at_lock);
    assert_eq!(google_identity_rows(&db, user_id).await, 0);
}
