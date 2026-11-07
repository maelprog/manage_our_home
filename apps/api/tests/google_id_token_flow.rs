//! The native Google sign-in by ID token (#669): `POST /auth/google/nonce`
//! then `POST /auth/google/id-token`, end to end against a real database.
//!
//! Google is never called: the key set is served from a local listener,
//! with the test keys of `tests/fixtures/google_id_token` (made for these
//! tests only), and the tokens are signed here with the matching private
//! keys.

mod common;

use axum::http::{header, Method, StatusCode};
use common::{assert_status, call, json_body};
use sqlx::PgPool;
use uuid::Uuid;

const AUDIENCE: &str = "test-client-id";
const KEY_A: &[u8] = include_bytes!("fixtures/google_id_token/key_a.der");
const KEY_B: &[u8] = include_bytes!("fixtures/google_id_token/key_b.der");
const JWK_A: &str = include_str!("fixtures/google_id_token/jwk_a.json");

/// A router whose ID tokens are checked against a key set holding key A
/// only, served from a local listener. `secure` as `SECURE_COOKIES`.
async fn router(db: PgPool, secure: bool) -> axum::Router {
    let jwks: serde_json::Value =
        serde_json::from_str(&format!(r#"{{"keys":[{JWK_A}]}}"#)).unwrap();
    let google = axum::Router::new().route(
        "/certs",
        axum::routing::get(move || {
            let jwks = jwks.clone();
            async move { axum::Json(jwks) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, google).await.unwrap() });

    let mut state = common::test_state(db);
    state.secure_cookies = secure;
    state.google_id_token = std::sync::Arc::new(
        manage_our_home::auth::google_id_token::IdTokenVerifier::new(
            AUDIENCE,
            format!("{base}/certs"),
        ),
    );
    manage_our_home::build_router(state)
}

fn set_cookies(response: &axum::response::Response) -> Vec<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect()
}

fn cookie_line<'a>(lines: &'a [String], name: &str) -> Option<&'a String> {
    lines
        .iter()
        .find(|line| line.starts_with(&format!("{name}=")))
}

fn cookie_value(lines: &[String], name: &str) -> Option<String> {
    cookie_line(lines, name)
        .map(|line| line.split(';').next().unwrap()[name.len() + 1..].to_string())
}

/// A browser's first step: the nonce, and the cookie pair to send back.
async fn nonce(router: &axum::Router) -> (String, String) {
    let res = call(router, Method::POST, "/auth/google/nonce", None, None).await;
    assert_status(&res, StatusCode::OK);
    let cookie = cookie_value(&set_cookies(&res), "google_id_token_nonce").expect("nonce cookie");
    let body = json_body(res).await;
    let nonce = body["nonce"].as_str().unwrap().to_string();
    assert_eq!(nonce, cookie, "the cookie holds the nonce handed out");
    (nonce, format!("google_id_token_nonce={cookie}"))
}

struct Claims {
    sub: &'static str,
    email: &'static str,
    aud: &'static str,
    nonce: String,
}

fn claims(sub: &'static str, email: &'static str, nonce: &str) -> Claims {
    Claims {
        sub,
        email,
        aud: AUDIENCE,
        nonce: nonce.to_string(),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// An ID token as Google signs it, with key `key` under the `kid` of key A.
fn id_token_signed_with(key: &[u8], claims: &Claims) -> String {
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some("test-key-a".into());
    jsonwebtoken::encode(
        &header,
        &serde_json::json!({
            "iss": "https://accounts.google.com",
            "azp": "android-client.apps.googleusercontent.com",
            "aud": claims.aud,
            "sub": claims.sub,
            "email": claims.email,
            "email_verified": true,
            "name": "Google User",
            "nonce": claims.nonce,
            "iat": now(),
            "exp": now() + 3600,
        }),
        &jsonwebtoken::EncodingKey::from_rsa_der(key),
    )
    .unwrap()
}

fn id_token(claims: &Claims) -> String {
    id_token_signed_with(KEY_A, claims)
}

async fn post_id_token(
    router: &axum::Router,
    cookie: Option<&str>,
    token: &str,
) -> axum::response::Response {
    call(
        router,
        Method::POST,
        "/auth/google/id-token",
        cookie,
        Some(serde_json::json!({ "id_token": token })),
    )
    .await
}

fn session_cookie(response: &axum::response::Response) -> Option<String> {
    let lines = set_cookies(response);
    ["__Host-session_id", "session_id"]
        .into_iter()
        .find_map(|name| {
            cookie_value(&lines, name)
                .filter(|v| !v.is_empty())
                .map(|v| format!("{name}={v}"))
        })
}

async fn user_id(db: &PgPool, email: &str) -> Option<Uuid> {
    sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_optional(db)
        .await
        .unwrap()
}

async fn identities(db: &PgPool, user_id: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT provider_user_id FROM oauth_identities
          WHERE user_id = $1 AND provider = 'google'",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
    .unwrap()
}

async fn sessions(db: &PgPool, user_id: Uuid) -> Vec<bool> {
    sqlx::query_scalar("SELECT restricted FROM sessions WHERE user_id = $1 ORDER BY created_at")
        .bind(user_id)
        .fetch_all(db)
        .await
        .unwrap()
}

/// A password account, registered and verified through the API.
async fn password_account(router: &axum::Router, db: &PgPool, email: &str) -> Uuid {
    let res = call(
        router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({
            "email": email,
            "password": "password-669-a",
            "display_name": "Password User",
            "declares_minimum_age": true,
            "accepts_terms": true,
        })),
    )
    .await;
    assert!(res.status().is_success(), "{}", res.status());
    let token = common::verification_token(db, email).await;
    let res = call(
        router,
        Method::GET,
        &format!("/auth/verify-email?token={token}"),
        None,
        None,
    )
    .await;
    assert!(
        res.status().is_success() || res.status().is_redirection(),
        "{}",
        res.status()
    );
    user_id(db, email).await.unwrap()
}

/// The whole sign-in: a new account is created, the Google identity bound
/// to it, and the session cookie set with its `__Host-` prefix behind
/// `SECURE_COOKIES`. The nonce cookie is cleared, and the nonce spent.
#[sqlx::test]
async fn a_valid_token_opens_a_session_with_the_host_prefixed_cookie(db: PgPool) {
    let router = router(db.clone(), true).await;
    let (nonce, cookie) = nonce(&router).await;
    let token = id_token(&claims("google-sub-669", "new-669@example.test", &nonce));

    let res = post_id_token(&router, Some(&cookie), &token).await;
    assert_status(&res, StatusCode::OK);
    let lines = set_cookies(&res);
    let session = cookie_line(&lines, "__Host-session_id").expect("a __Host- session cookie");
    for attribute in ["HttpOnly", "Secure", "Path=/", "SameSite=Lax"] {
        assert!(session.contains(attribute), "{session}");
    }
    let cleared = cookie_line(&lines, "google_id_token_nonce").expect("nonce cookie cleared");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
    assert!(cleared.contains("Path=/"), "{cleared}");
    let session = session_cookie(&res).unwrap();
    assert_eq!(json_body(res).await, serde_json::json!({ "redirect": "/" }));

    let user = user_id(&db, "new-669@example.test").await.expect("created");
    assert_eq!(identities(&db, user).await, vec!["google-sub-669"]);
    assert_eq!(sessions(&db, user).await, vec![false]);
    let nonces: i64 = sqlx::query_scalar("SELECT count(*) FROM google_sign_in_nonces")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(nonces, 0, "the nonce is spent");

    // As a Google sign-up through `callback`: held at the age declaration
    // (#318), a session all the same.
    let me = call(&router, Method::GET, "/auth/me", Some(&session), None).await;
    assert_status(&me, StatusCode::FORBIDDEN);
    assert_eq!(json_body(me).await["error"], "age_not_declared");
}

/// The nonce route: an HttpOnly flow cookie that lasts as long as the
/// nonce, and only the nonce's hash in the database.
#[sqlx::test]
async fn the_nonce_lives_in_an_http_only_cookie_and_as_a_hash(db: PgPool) {
    let router = router(db.clone(), true).await;
    let res = call(&router, Method::POST, "/auth/google/nonce", None, None).await;
    assert_status(&res, StatusCode::OK);
    let lines = set_cookies(&res);
    let line = cookie_line(&lines, "google_id_token_nonce")
        .unwrap()
        .clone();
    for attribute in [
        "HttpOnly",
        "Secure",
        "Path=/",
        "SameSite=Lax",
        "Max-Age=300",
    ] {
        assert!(line.contains(attribute), "{line}");
    }
    let nonce = json_body(res).await["nonce"].as_str().unwrap().to_string();
    let stored: Vec<Vec<u8>> = sqlx::query_scalar("SELECT nonce_hash FROM google_sign_in_nonces")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(
        stored,
        vec![manage_our_home::auth::token::token_hash(&nonce)
            .unwrap()
            .to_vec()]
    );
}

#[sqlx::test]
async fn a_token_for_another_audience_is_refused(db: PgPool) {
    let router = router(db.clone(), false).await;
    let (nonce, cookie) = nonce(&router).await;
    let token = id_token(&Claims {
        aud: "another-client.apps.googleusercontent.com",
        ..claims("google-sub-aud", "aud-669@example.test", &nonce)
    });

    let res = post_id_token(&router, Some(&cookie), &token).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
    assert!(session_cookie(&res).is_none());
    assert_eq!(json_body(res).await["error"], "unauthorized");
    assert_eq!(user_id(&db, "aud-669@example.test").await, None);
}

#[sqlx::test]
async fn a_token_signed_by_another_key_is_refused(db: PgPool) {
    let router = router(db.clone(), false).await;
    let (nonce, cookie) = nonce(&router).await;
    let token = id_token_signed_with(
        KEY_B,
        &claims("google-sub-forged", "forged-669@example.test", &nonce),
    );

    let res = post_id_token(&router, Some(&cookie), &token).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
    assert_eq!(user_id(&db, "forged-669@example.test").await, None);
}

/// The same token and the same cookie, presented again: the nonce is
/// spent, whatever the browser sends.
#[sqlx::test]
async fn a_replayed_nonce_is_refused(db: PgPool) {
    let router = router(db.clone(), false).await;
    let (nonce, cookie) = nonce(&router).await;
    let token = id_token(&claims(
        "google-sub-replay",
        "replay-669@example.test",
        &nonce,
    ));

    let first = post_id_token(&router, Some(&cookie), &token).await;
    assert_status(&first, StatusCode::OK);
    let user = user_id(&db, "replay-669@example.test").await.unwrap();

    let replay = post_id_token(&router, Some(&cookie), &token).await;
    assert_status(&replay, StatusCode::UNAUTHORIZED);
    assert!(session_cookie(&replay).is_none());
    assert_eq!(sessions(&db, user).await.len(), 1);
}

/// A token carrying another browser's nonce — lifted from it, say — does
/// not sign in this one, and leaves the other's nonce unspent.
#[sqlx::test]
async fn a_nonce_issued_to_another_browser_is_refused(db: PgPool) {
    let router = router(db.clone(), false).await;
    let (their_nonce, their_cookie) = nonce(&router).await;
    let (_, my_cookie) = nonce(&router).await;
    let token = id_token(&claims(
        "google-sub-other",
        "other-669@example.test",
        &their_nonce,
    ));

    let res = post_id_token(&router, Some(&my_cookie), &token).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
    assert_eq!(user_id(&db, "other-669@example.test").await, None);

    let theirs = post_id_token(&router, Some(&their_cookie), &token).await;
    assert_status(&theirs, StatusCode::OK);
}

#[sqlx::test]
async fn without_the_nonce_cookie_the_token_is_refused(db: PgPool) {
    let router = router(db.clone(), false).await;
    let (nonce, _) = nonce(&router).await;
    let token = id_token(&claims("google-sub-none", "none-669@example.test", &nonce));

    let res = post_id_token(&router, None, &token).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
    assert_eq!(user_id(&db, "none-669@example.test").await, None);
}

/// A nonce past its five minutes is refused, cookie or not.
#[sqlx::test]
async fn an_expired_nonce_is_refused(db: PgPool) {
    let router = router(db.clone(), false).await;
    let (nonce, cookie) = nonce(&router).await;
    sqlx::query("UPDATE google_sign_in_nonces SET expires_at = now() - interval '1 second'")
        .execute(&db)
        .await
        .unwrap();
    let token = id_token(&claims("google-sub-late", "late-669@example.test", &nonce));

    let res = post_id_token(&router, Some(&cookie), &token).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
    assert_eq!(user_id(&db, "late-669@example.test").await, None);
}

/// As `callback` does: the verified profile signs in to the password
/// account with the same email, and the Google identity is bound to it.
#[sqlx::test]
async fn the_email_of_an_existing_password_account_signs_in_to_it(db: PgPool) {
    let router = router(db.clone(), false).await;
    let user = password_account(&router, &db, "dana-669@example.test").await;
    let (nonce, cookie) = nonce(&router).await;
    let token = id_token(&claims("google-sub-dana", "Dana-669@Example.test", &nonce));

    let res = post_id_token(&router, Some(&cookie), &token).await;
    assert_status(&res, StatusCode::OK);
    let session = session_cookie(&res).expect("a session");
    assert_eq!(identities(&db, user).await, vec!["google-sub-dana"]);
    assert_eq!(sessions(&db, user).await, vec![false]);
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(users, 1, "no second account");
    let me = call(&router, Method::GET, "/auth/me", Some(&session), None).await;
    assert_status(&me, StatusCode::OK);
    assert_eq!(json_body(me).await["user_id"], user.to_string());
}

/// As `callback` does (#194, #289): an account deactivated by support gets
/// a restricted session, and no identity is bound to it.
#[sqlx::test]
async fn a_deactivated_account_gets_only_a_restricted_session(db: PgPool) {
    let router = router(db.clone(), false).await;
    let user = password_account(&router, &db, "locked-669@example.test").await;
    sqlx::query("UPDATE users SET deactivated_at = now() WHERE id = $1")
        .bind(user)
        .execute(&db)
        .await
        .unwrap();
    let (nonce, cookie) = nonce(&router).await;
    let token = id_token(&claims(
        "google-sub-locked",
        "locked-669@example.test",
        &nonce,
    ));

    let res = post_id_token(&router, Some(&cookie), &token).await;
    assert_status(&res, StatusCode::OK);
    let session = session_cookie(&res).expect("a session");
    assert_eq!(json_body(res).await, serde_json::json!({ "redirect": "/" }));
    assert_eq!(sessions(&db, user).await, vec![true]);
    assert!(identities(&db, user).await.is_empty());
    let me = call(&router, Method::GET, "/auth/me", Some(&session), None).await;
    assert_status(&me, StatusCode::FORBIDDEN);
    assert_eq!(json_body(me).await["error"], "account_deactivated");
}

/// As `callback` does (#139): a purged account, found by its Google
/// identity, is refused with a bare 401.
#[sqlx::test]
async fn a_purged_account_is_refused(db: PgPool) {
    let router = router(db.clone(), false).await;
    let user: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, email_verified, display_name, deleted_at)
         VALUES ('purged-669@example.test', true, 'Purged', now()) RETURNING id",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO oauth_identities (user_id, provider, provider_user_id)
         VALUES ($1, 'google', 'google-sub-purged')",
    )
    .bind(user)
    .execute(&db)
    .await
    .unwrap();
    let (nonce, cookie) = nonce(&router).await;
    let token = id_token(&claims(
        "google-sub-purged",
        "purged-669@example.test",
        &nonce,
    ));

    let res = post_id_token(&router, Some(&cookie), &token).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
    assert!(session_cookie(&res).is_none());
    assert!(sessions(&db, user).await.is_empty());
}

/// As `callback` does (#572), as JSON: nothing signed in to nor created,
/// and the code the login page has copy for.
#[sqlx::test]
async fn an_address_outside_ascii_is_refused_with_its_reason(db: PgPool) {
    let router = router(db.clone(), false).await;
    let (nonce, cookie) = nonce(&router).await;
    let token = id_token(&claims("google-sub-zoe", "Zoë-669@Example.test", &nonce));

    let res = post_id_token(&router, Some(&cookie), &token).await;
    assert_status(&res, StatusCode::OK);
    assert!(session_cookie(&res).is_none());
    assert_eq!(
        json_body(res).await,
        serde_json::json!({
            "redirect": "/login?google=email_not_ascii",
            "refusal": "email_not_ascii",
        })
    );
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(users, 0);
}

/// Both routes spend one budget per address: past it, 429, and the work is
/// not done.
#[sqlx::test]
async fn both_routes_share_a_rate_limit_per_address(db: PgPool) {
    use manage_our_home::auth::google_id_token::RATE;
    let router = router(db.clone(), false).await;
    let (nonce, cookie) = nonce(&router).await;
    for _ in 1..RATE.limit - 1 {
        let res = call(&router, Method::POST, "/auth/google/nonce", None, None).await;
        assert_status(&res, StatusCode::OK);
    }
    let token = id_token(&claims("google-sub-rate", "rate-669@example.test", &nonce));
    let last = post_id_token(&router, Some(&cookie), "not-a-token").await;
    assert_status(&last, StatusCode::UNAUTHORIZED);

    let refused = post_id_token(&router, Some(&cookie), &token).await;
    assert_status(&refused, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json_body(refused).await["error"], "too_many_attempts");
    let refused = call(&router, Method::POST, "/auth/google/nonce", None, None).await;
    assert_status(&refused, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(user_id(&db, "rate-669@example.test").await, None);
}
