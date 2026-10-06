mod common;

use axum::http::{Method, StatusCode};
use common::{
    assert_status, call, drop_prescribed_role, json_body, prescribed_role_pool, session_id_of,
    set_cookie, test_router,
};
use manage_our_home::auth::terms_acceptance::terms_in_force_now;
use manage_our_home::auth::token::{new_token, token_hash};
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
            serde_json::json!({"email": email, "password": password, "display_name": "Test User", "declares_minimum_age": true, "accepts_terms": true}),
        ),
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

async fn set_last_seen_ago(db: &PgPool, session_id: Uuid, ago: &str) {
    sqlx::query("UPDATE sessions SET last_seen_at = now() - $2::interval WHERE id = $1")
        .bind(session_id)
        .bind(ago)
        .execute(db)
        .await
        .unwrap();
}

async fn last_seen_older_than(db: &PgPool, session_id: Uuid, ago: &str) -> bool {
    sqlx::query_scalar("SELECT last_seen_at < now() - $2::interval FROM sessions WHERE id = $1")
        .bind(session_id)
        .bind(ago)
        .fetch_one(db)
        .await
        .unwrap()
}

/// #195: a session unused for more than 7 days is refused, although its
/// absolute 30-day lifetime has not run out. The row is not revoked: the
/// refusal leaves `revoked_at` and `last_seen_at` as they were.
#[sqlx::test]
async fn a_session_idle_for_more_than_seven_days_is_refused(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "idle@example.test", "idle-password1").await;
    let session_id = session_id_of(&db, &cookie).await;

    set_last_seen_ago(&db, session_id, "7 days 1 minute").await;
    let res = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
    assert_eq!(json_body(res).await["error"], "unauthorized");

    let (revoked, still_idle): (bool, bool) = sqlx::query_as(
        "SELECT revoked_at IS NOT NULL, last_seen_at < now() - interval '7 days'
         FROM sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(!revoked, "an idle session expires, it is not revoked");
    assert!(still_idle, "a refused request must not revive the session");

    // A later request is refused too: the refusal did not reset the clock.
    let res = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
}

/// #195: a session used within the last 7 days is accepted, and the request
/// pushes its inactivity deadline back by refreshing `last_seen_at`.
#[sqlx::test]
async fn a_session_used_within_seven_days_is_accepted_and_refreshed(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "active@example.test", "active-password1").await;
    let session_id = session_id_of(&db, &cookie).await;

    set_last_seen_ago(&db, session_id, "6 days 23 hours").await;
    let res = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&res, StatusCode::OK);
    assert!(
        !last_seen_older_than(&db, session_id, "1 minute").await,
        "an accepted request must refresh a stale last_seen_at"
    );
}

/// #195: `last_seen_at` is rewritten at most once an hour, not on every
/// request — a request within the hour leaves it as it was.
#[sqlx::test]
async fn last_seen_at_is_not_rewritten_within_the_hour(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "hourly@example.test", "hourly-password1").await;
    let session_id = session_id_of(&db, &cookie).await;

    set_last_seen_ago(&db, session_id, "50 minutes").await;
    let res = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&res, StatusCode::OK);
    assert!(
        last_seen_older_than(&db, session_id, "49 minutes").await,
        "a request within the hour must not rewrite last_seen_at"
    );
}

/// #222: what the `sessions` table holds opens nothing. The cookie carries
/// a token the table only keeps the SHA-256 of; a cookie made of the row's
/// `id` (the former cookie) or of its `token_hash`, spelt as hex or as
/// base64url, is refused. The control: the real cookie opens the session.
#[sqlx::test]
async fn a_cookie_made_of_what_the_sessions_table_holds_is_refused(db: PgPool) {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "hash@example.test", "hash-password1").await;
    let res = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&res, StatusCode::OK);

    let session_id = session_id_of(&db, &cookie).await;
    let (row, token_hash): (String, Vec<u8>) =
        sqlx::query_as("SELECT s::text, token_hash FROM sessions s WHERE id = $1")
            .bind(session_id)
            .fetch_one(&db)
            .await
            .unwrap();
    let (_, token) = cookie.split_once('=').unwrap();
    assert!(!row.contains(token), "the raw token is stored: {row}");
    assert_eq!(token_hash.len(), 32);

    let hex: String = token_hash.iter().map(|b| format!("{b:02x}")).collect();
    for forged in [
        session_id.to_string(),
        hex,
        URL_SAFE_NO_PAD.encode(&token_hash),
    ] {
        let forged = format!("session_id={forged}");
        let res = call(&router, Method::GET, "/auth/me", Some(&forged), None).await;
        assert_status(&res, StatusCode::UNAUTHORIZED);
    }

    let res = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&res, StatusCode::OK);
}

/// #224: behind `SECURE_COOKIES`, the session cookie is `__Host-session_id`,
/// `Secure`, `Path=/`, without `Domain` — what a browser requires before it
/// accepts the prefix. The api reads it under that name only: the same
/// token under the bare `session_id` is refused, so a cookie planted from a
/// sibling subdomain (which cannot carry the prefix) is never read. Logging
/// out removes it under the same name, `Secure` too, or the browser would
/// drop the removal.
#[sqlx::test]
async fn behind_secure_cookies_the_session_cookie_carries_the_host_prefix(db: PgPool) {
    let mut state = common::test_state(db.clone());
    state.secure_cookies = true;
    let router = manage_our_home::build_router(state);
    call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({
            "email": "host@example.test", "password": "host-password1",
            "display_name": "Host", "declares_minimum_age": true, "accepts_terms": true
        })),
    )
    .await;
    sqlx::query("UPDATE users SET email_verified = true WHERE email = $1")
        .bind("host@example.test")
        .execute(&db)
        .await
        .unwrap();
    let login = call(
        &router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "host@example.test", "password": "host-password1"})),
    )
    .await;
    assert_status(&login, StatusCode::OK);
    let lines = set_cookies(&login);
    let line = lines
        .iter()
        .find(|l| l.starts_with("__Host-session_id="))
        .unwrap_or_else(|| panic!("no __Host- session cookie in {lines:?}"));
    let attributes: Vec<String> = line
        .split(';')
        .skip(1)
        .map(|a| a.trim().to_ascii_lowercase())
        .collect();
    assert!(attributes.iter().any(|a| a == "secure"), "{line}");
    assert!(attributes.iter().any(|a| a == "path=/"), "{line}");
    assert!(
        !attributes.iter().any(|a| a.starts_with("domain")),
        "{line}"
    );

    let token = cookie_value(&lines, "__Host-session_id").unwrap();
    let cookie = format!("__Host-session_id={token}");
    let res = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&res, StatusCode::OK);
    let bare = format!("session_id={token}");
    let res = call(&router, Method::GET, "/auth/me", Some(&bare), None).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);

    let logout = call(&router, Method::POST, "/auth/logout", Some(&cookie), None).await;
    assert_status(&logout, StatusCode::NO_CONTENT);
    let lines = set_cookies(&logout);
    let removal = lines
        .iter()
        .find(|l| l.starts_with("__Host-session_id="))
        .unwrap_or_else(|| panic!("no __Host- removal in {lines:?}"));
    let attributes: Vec<String> = removal
        .split(';')
        .skip(1)
        .map(|a| a.trim().to_ascii_lowercase())
        .collect();
    assert!(attributes.iter().any(|a| a == "secure"), "{removal}");
    assert!(attributes.iter().any(|a| a == "path=/"), "{removal}");
    assert!(attributes.iter().any(|a| a == "max-age=0"), "{removal}");
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
        Some(serde_json::json!({"email": "v@example.test", "password": "short", "display_name": "V", "declares_minimum_age": true, "accepts_terms": true})),
    )
    .await;
    assert_status(&short_pw, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json_body(short_pw).await["error"], "password_too_short");

    let bad_email = call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": "not-an-email", "password": "long-enough-1", "display_name": "V", "declares_minimum_age": true, "accepts_terms": true})),
    )
    .await;
    assert_status(&bad_email, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json_body(bad_email).await["error"], "invalid_email");

    let empty_name = call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": "v@example.test", "password": "long-enough-1", "display_name": "   ", "declares_minimum_age": true, "accepts_terms": true})),
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
        Some(serde_json::json!({"email": "v@example.test", "password": "long-enough-1", "display_name": "Valid", "declares_minimum_age": true, "accepts_terms": true})),
    )
    .await;
    assert_status(&ok, StatusCode::CREATED);
}

/// #137, art. 8 GDPR: the service is not open under 15, so a registration
/// that does not carry the age declaration is refused — and one that does
/// leaves the declaration on file, dated. A body that simply omits the field
/// declares nothing: it gets the same 422 code as an explicit `false`, not a
/// deserialization error, so a caller is told which rule it broke.
#[sqlx::test]
async fn register_requires_the_age_declaration_and_records_it(db: PgPool) {
    let router = test_router(db.clone());

    for body in [
        serde_json::json!({"email": "young@example.test", "password": "long-enough-1", "display_name": "Young", "declares_minimum_age": false}),
        serde_json::json!({"email": "young@example.test", "password": "long-enough-1", "display_name": "Young"}),
    ] {
        let refused = call(&router, Method::POST, "/auth/register", None, Some(body)).await;
        assert_status(&refused, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            json_body(refused).await["error"],
            "age_declaration_required"
        );
    }

    let none_created =
        sqlx::query_scalar!("SELECT count(*) FROM users WHERE email = 'young@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(
        none_created,
        Some(0),
        "a refused registration created a user"
    );

    let created = call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": "old-enough@example.test", "password": "long-enough-1", "display_name": "Old Enough", "declares_minimum_age": true, "accepts_terms": true})),
    )
    .await;
    assert_status(&created, StatusCode::CREATED);

    let declared_at = sqlx::query_scalar!(
        "SELECT age_declared_at FROM users WHERE email = 'old-enough@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(
        declared_at.is_some(),
        "the age declaration was not recorded on the account"
    );
}

/// #318: an account with no age declaration on file — opened through
/// Google, or before #137 — still logs in, but its session opens nothing
/// until the declaration is made: every `AuthUser` route answers 403
/// `age_not_declared`. Declaring nothing gets the registration's 422; the
/// declaration, once made, dates the account and opens the app to the same
/// session, and is never made twice.
#[sqlx::test]
async fn an_account_without_age_declaration_is_held_at_the_declaration(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "legacy@example.test", "long-enough-1").await;
    sqlx::query!("UPDATE users SET age_declared_at = NULL WHERE email = 'legacy@example.test'")
        .execute(&db)
        .await
        .unwrap();

    for path in ["/auth/me", "/auth/sessions", "/groups", "/account/export"] {
        let held = call(&router, Method::GET, path, Some(&cookie), None).await;
        assert_status(&held, StatusCode::FORBIDDEN);
        assert_eq!(json_body(held).await["error"], "age_not_declared", "{path}");
    }

    for body in [
        serde_json::json!({"declares_minimum_age": false}),
        serde_json::json!({}),
    ] {
        let refused = call(
            &router,
            Method::POST,
            "/auth/age-declaration",
            Some(&cookie),
            Some(body),
        )
        .await;
        assert_status(&refused, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            json_body(refused).await["error"],
            "age_declaration_required"
        );
    }
    let still_none = sqlx::query_scalar!(
        "SELECT age_declared_at FROM users WHERE email = 'legacy@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(still_none, None, "declaring nothing recorded a declaration");

    let declared = call(
        &router,
        Method::POST,
        "/auth/age-declaration",
        Some(&cookie),
        Some(serde_json::json!({"declares_minimum_age": true})),
    )
    .await;
    assert_status(&declared, StatusCode::NO_CONTENT);
    let declared_at = sqlx::query_scalar!(
        "SELECT age_declared_at FROM users WHERE email = 'legacy@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(declared_at.is_some(), "the declaration was not recorded");

    let me = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&me, StatusCode::OK);

    let again = call(
        &router,
        Method::POST,
        "/auth/age-declaration",
        Some(&cookie),
        Some(serde_json::json!({"declares_minimum_age": true})),
    )
    .await;
    assert_status(&again, StatusCode::UNAUTHORIZED);
    let unchanged = sqlx::query_scalar!(
        "SELECT age_declared_at FROM users WHERE email = 'legacy@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(unchanged, declared_at, "the declaration was rewritten");
}

/// #318: the holder of an account without age declaration — one who does
/// not declare, under 15 included — can still end the session.
#[sqlx::test]
async fn an_account_without_age_declaration_can_log_out(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "leaving@example.test", "long-enough-1").await;
    sqlx::query!("UPDATE users SET age_declared_at = NULL WHERE email = 'leaving@example.test'")
        .execute(&db)
        .await
        .unwrap();

    let out = call(&router, Method::POST, "/auth/logout", Some(&cookie), None).await;
    assert_status(&out, StatusCode::NO_CONTENT);
    let after = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&after, StatusCode::UNAUTHORIZED);
}

/// #318: the declaration route is open to nobody else — no session, or the
/// session of an account that already declared, is a 401.
#[sqlx::test]
async fn the_age_declaration_needs_a_session_awaiting_it(db: PgPool) {
    let router = test_router(db.clone());
    let body = serde_json::json!({"declares_minimum_age": true});

    let anonymous = call(
        &router,
        Method::POST,
        "/auth/age-declaration",
        None,
        Some(body.clone()),
    )
    .await;
    assert_status(&anonymous, StatusCode::UNAUTHORIZED);

    let cookie =
        register_verify_login(&router, &db, "declared@example.test", "long-enough-1").await;
    let declared = call(
        &router,
        Method::POST,
        "/auth/age-declaration",
        Some(&cookie),
        Some(body),
    )
    .await;
    assert_status(&declared, StatusCode::UNAUTHORIZED);
}

/// The CGU acceptance on file for `email`: version and date (#319).
async fn terms_on_file(
    db: &PgPool,
    email: &str,
) -> (Option<String>, Option<chrono::DateTime<chrono::Utc>>) {
    let row = sqlx::query!(
        "SELECT terms_accepted_version, terms_accepted_at FROM users WHERE email = $1",
        email
    )
    .fetch_one(db)
    .await
    .unwrap();
    (row.terms_accepted_version, row.terms_accepted_at)
}

/// #319: a registration that does not accept the CGU is refused — after the
/// age declaration, which keeps its code — and one that does records the
/// version in force, dated. Omitting the field accepts nothing.
#[sqlx::test]
async fn register_requires_the_terms_acceptance_and_records_its_version(db: PgPool) {
    let router = test_router(db.clone());

    for body in [
        serde_json::json!({"email": "no-terms@example.test", "password": "long-enough-1", "display_name": "No Terms", "declares_minimum_age": true, "accepts_terms": false}),
        serde_json::json!({"email": "no-terms@example.test", "password": "long-enough-1", "display_name": "No Terms", "declares_minimum_age": true}),
    ] {
        let refused = call(&router, Method::POST, "/auth/register", None, Some(body)).await;
        assert_status(&refused, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            json_body(refused).await["error"],
            "terms_acceptance_required"
        );
    }
    let none_created =
        sqlx::query_scalar!("SELECT count(*) FROM users WHERE email = 'no-terms@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(
        none_created,
        Some(0),
        "a refused registration created a user"
    );

    let created = call(
        &router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": "terms@example.test", "password": "long-enough-1", "display_name": "Terms", "declares_minimum_age": true, "accepts_terms": true})),
    )
    .await;
    assert_status(&created, StatusCode::CREATED);
    let (version, at) = terms_on_file(&db, "terms@example.test").await;
    assert_eq!(version.as_deref(), Some(terms_in_force_now()));
    assert!(at.is_some(), "the acceptance was not dated");
}

/// #319: an account with no acceptance of the CGU on file — opened through
/// Google, or before #319 — still logs in, but its session opens nothing
/// until it accepts: every `AuthUser` route answers 403
/// `terms_not_accepted`. Accepting nothing gets the registration's 422; the
/// acceptance records the version in force and opens the app to the same
/// session, which `/auth/me` then reports.
#[sqlx::test]
async fn an_account_without_terms_acceptance_is_held_at_the_acceptance(db: PgPool) {
    let router = test_router(db.clone());
    let email = "legacy-terms@example.test";
    let cookie = register_verify_login(&router, &db, email, "long-enough-1").await;
    sqlx::query!(
        "UPDATE users SET terms_accepted_version = NULL, terms_accepted_at = NULL WHERE email = $1",
        email
    )
    .execute(&db)
    .await
    .unwrap();

    for path in ["/auth/me", "/auth/sessions", "/groups", "/account/export"] {
        let held = call(&router, Method::GET, path, Some(&cookie), None).await;
        assert_status(&held, StatusCode::FORBIDDEN);
        assert_eq!(
            json_body(held).await["error"],
            "terms_not_accepted",
            "{path}"
        );
    }

    for body in [
        serde_json::json!({"accepts_terms": false}),
        serde_json::json!({}),
    ] {
        let refused = call(
            &router,
            Method::POST,
            "/auth/terms-acceptance",
            Some(&cookie),
            Some(body),
        )
        .await;
        assert_status(&refused, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            json_body(refused).await["error"],
            "terms_acceptance_required"
        );
    }
    assert_eq!(
        terms_on_file(&db, email).await,
        (None, None),
        "accepting nothing recorded an acceptance"
    );

    let accepted = call(
        &router,
        Method::POST,
        "/auth/terms-acceptance",
        Some(&cookie),
        Some(serde_json::json!({"accepts_terms": true})),
    )
    .await;
    assert_status(&accepted, StatusCode::NO_CONTENT);
    let (version, at) = terms_on_file(&db, email).await;
    assert_eq!(version.as_deref(), Some(terms_in_force_now()));
    assert!(at.is_some(), "the acceptance was not dated");

    let me = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&me, StatusCode::OK);
    assert_eq!(
        json_body(me).await["terms_accepted_version"],
        terms_in_force_now()
    );

    // The holder can log out as any other.
    let out = call(&router, Method::POST, "/auth/logout", Some(&cookie), None).await;
    assert_status(&out, StatusCode::NO_CONTENT);
}

/// #319: the age declaration comes first. An account with neither is held
/// at the declaration, and cannot skip it by accepting the CGU.
#[sqlx::test]
async fn the_age_declaration_comes_before_the_terms_acceptance(db: PgPool) {
    let router = test_router(db.clone());
    let email = "neither@example.test";
    let cookie = register_verify_login(&router, &db, email, "long-enough-1").await;
    sqlx::query!(
        "UPDATE users SET age_declared_at = NULL, terms_accepted_version = NULL,
                          terms_accepted_at = NULL
         WHERE email = $1",
        email
    )
    .execute(&db)
    .await
    .unwrap();

    let me = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&me, StatusCode::FORBIDDEN);
    assert_eq!(json_body(me).await["error"], "age_not_declared");

    let early = call(
        &router,
        Method::POST,
        "/auth/terms-acceptance",
        Some(&cookie),
        Some(serde_json::json!({"accepts_terms": true})),
    )
    .await;
    assert_status(&early, StatusCode::UNAUTHORIZED);
    assert_eq!(terms_on_file(&db, email).await, (None, None));
}

/// #319: a member who accepted an earlier version is not held — continued
/// use is acceptance, per the CGU — and `/auth/me` reports the version on
/// file, from which `apps/web` tells them the CGU changed. Acknowledging
/// records the version in force; acknowledging it again keeps the first
/// date.
#[sqlx::test]
async fn a_member_on_an_earlier_version_acknowledges_the_new_one(db: PgPool) {
    let router = test_router(db.clone());
    let email = "earlier@example.test";
    let cookie = register_verify_login(&router, &db, email, "long-enough-1").await;
    sqlx::query!(
        "UPDATE users SET terms_accepted_version = '2000-01-01',
                          terms_accepted_at = now() - interval '1 year'
         WHERE email = $1",
        email
    )
    .execute(&db)
    .await
    .unwrap();

    let me = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&me, StatusCode::OK);
    assert_eq!(json_body(me).await["terms_accepted_version"], "2000-01-01");
    let groups = call(&router, Method::GET, "/groups", Some(&cookie), None).await;
    assert_status(&groups, StatusCode::OK);

    let refused = call(
        &router,
        Method::POST,
        "/auth/terms-acceptance",
        Some(&cookie),
        Some(serde_json::json!({"accepts_terms": false})),
    )
    .await;
    assert_status(&refused, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        terms_on_file(&db, email).await.0.as_deref(),
        Some("2000-01-01")
    );

    let acknowledged = call(
        &router,
        Method::POST,
        "/auth/terms-acceptance",
        Some(&cookie),
        Some(serde_json::json!({"accepts_terms": true})),
    )
    .await;
    assert_status(&acknowledged, StatusCode::NO_CONTENT);
    let (version, first_at) = terms_on_file(&db, email).await;
    assert_eq!(version.as_deref(), Some(terms_in_force_now()));
    let first_at = first_at.unwrap();
    assert!(
        first_at > chrono::Utc::now() - chrono::Duration::hours(1),
        "the acknowledgement kept the earlier date"
    );

    let again = call(
        &router,
        Method::POST,
        "/auth/terms-acceptance",
        Some(&cookie),
        Some(serde_json::json!({"accepts_terms": true})),
    )
    .await;
    assert_status(&again, StatusCode::NO_CONTENT);
    assert_eq!(
        terms_on_file(&db, email).await.1,
        Some(first_at),
        "acknowledging the same version again rewrote its date"
    );
}

/// #367, rollback: a member accepted a version later than the one this
/// binary holds in force — a release that announced it, its date passed,
/// then this earlier release put back. Their acceptance covers the earlier
/// text: the session is full, `/auth/me` reports the later version, and
/// acknowledging again does not replace it with the earlier one.
#[sqlx::test]
async fn an_acceptance_of_a_later_version_covers_the_one_in_force(db: PgPool) {
    let router = test_router(db.clone());
    let email = "later@example.test";
    let cookie = register_verify_login(&router, &db, email, "long-enough-1").await;
    let in_force = chrono::NaiveDate::parse_from_str(terms_in_force_now(), "%Y-%m-%d").unwrap();
    let later = (in_force + chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    sqlx::query!(
        "UPDATE users SET terms_accepted_version = $2,
                          terms_accepted_at = now() - interval '1 day'
         WHERE email = $1",
        email,
        later
    )
    .execute(&db)
    .await
    .unwrap();
    let (_, accepted_at) = terms_on_file(&db, email).await;

    let me = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&me, StatusCode::OK);
    assert_eq!(
        json_body(me).await["terms_accepted_version"],
        later.as_str()
    );
    let groups = call(&router, Method::GET, "/groups", Some(&cookie), None).await;
    assert_status(&groups, StatusCode::OK);

    let acknowledged = call(
        &router,
        Method::POST,
        "/auth/terms-acceptance",
        Some(&cookie),
        Some(serde_json::json!({"accepts_terms": true})),
    )
    .await;
    assert_status(&acknowledged, StatusCode::NO_CONTENT);
    assert_eq!(
        terms_on_file(&db, email).await,
        (Some(later), accepted_at),
        "an earlier version replaced the later one accepted"
    );
}

/// #319: the acceptance needs a session — anonymous is a 401 — and not a
/// restricted one: the holder of a deactivated account accepts nothing.
#[sqlx::test]
async fn the_terms_acceptance_needs_a_full_or_held_session(db: PgPool) {
    let router = test_router(db.clone());
    let anonymous = call(
        &router,
        Method::POST,
        "/auth/terms-acceptance",
        None,
        Some(serde_json::json!({"accepts_terms": true})),
    )
    .await;
    assert_status(&anonymous, StatusCode::UNAUTHORIZED);

    // A correct login on a deactivated account opens a restricted session
    // (#289), whatever the acceptance on file.
    let email = "deactivated-terms@example.test";
    register_verify_login(&router, &db, email, "long-enough-1").await;
    sqlx::query!(
        "UPDATE users SET deactivated_at = now(), terms_accepted_version = NULL,
                          terms_accepted_at = NULL
         WHERE email = $1",
        email
    )
    .execute(&db)
    .await
    .unwrap();
    let login = call(
        &router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": email, "password": "long-enough-1"})),
    )
    .await;
    let restricted = set_cookie(&login).expect("a restricted session");
    let refused = call(
        &router,
        Method::POST,
        "/auth/terms-acceptance",
        Some(&restricted),
        Some(serde_json::json!({"accepts_terms": true})),
    )
    .await;
    assert_status(&refused, StatusCode::UNAUTHORIZED);
    assert_eq!(terms_on_file(&db, email).await, (None, None));
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
            "declares_minimum_age": true, "accepts_terms": true
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
            "declares_minimum_age": true, "accepts_terms": true
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
            "declares_minimum_age": true, "accepts_terms": true
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

    let token = common::verification_token(&db, "bob@example.test").await;

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
            "declares_minimum_age": true, "accepts_terms": true
        })),
    )
    .await;
    let token = common::verification_token(&db, "carol@example.test").await;
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

    let reset_token = common::reset_token(&db, "carol@example.test").await;

    let reset = call(
        &router,
        Method::POST,
        "/auth/password/reset",
        None,
        Some(serde_json::json!({"token": reset_token, "new_password": "brand-new-password"})),
    )
    .await;
    assert_status(&reset, StatusCode::OK);

    // #138: the token is deleted at use, so a second use finds nothing.
    let left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM password_reset_tokens WHERE token_hash = $1")
            .bind(&token_hash(&reset_token).unwrap()[..])
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(left, 0);
    let reuse = call(
        &router,
        Method::POST,
        "/auth/password/reset",
        None,
        Some(serde_json::json!({"token": reset_token, "new_password": "another-password-2"})),
    )
    .await;
    assert_status(&reuse, StatusCode::NOT_FOUND);

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
        Some(serde_json::json!({"email": "dave@example.test", "password": "old-password-1", "display_name": "Dave", "declares_minimum_age": true, "accepts_terms": true})),
    )
    .await;
    let token = common::verification_token(&db, "dave@example.test").await;
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
        Some(serde_json::json!({"email": "erin@example.test", "password": "erins-password1", "display_name": "Erin", "declares_minimum_age": true, "accepts_terms": true})),
    )
    .await;
    let token = common::verification_token(&db, "erin@example.test").await;
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

/// Issue #207: the `owner_of_groups` guard of `POST /account/delete` read
/// the owned groups on the bare pool, outside any RLS scope. Under the
/// `NOSUPERUSER NOBYPASSRLS` role apps/api/README.md prescribes, that read
/// came back empty and the owner of a group was scheduled for deletion
/// (200) instead of being blocked (409). The test above drives the handler
/// through the `#[sqlx::test]` pool, which bypasses RLS, so it cannot see
/// this. Here the guard must block the owner and list the group, and must
/// still let a caller who owns nothing through.
#[sqlx::test]
async fn account_deletion_guard_sees_owned_groups_under_the_prescribed_role(db: PgPool) {
    let (role, app_db) = prescribed_role_pool(&db).await;
    let router = test_router(app_db.clone());
    let owner = register_verify_login(&router, &db, "gwen@example.test", "gwens-password1").await;
    let loner = register_verify_login(&router, &db, "hugo@example.test", "hugos-password1").await;

    let create_group = call(
        &router,
        Method::POST,
        "/groups",
        Some(&owner),
        Some(serde_json::json!({"name": "Famille Gwen"})),
    )
    .await;
    assert_status(&create_group, StatusCode::CREATED);
    let group_id = json_body(create_group).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let blocked = call(
        &router,
        Method::POST,
        "/account/delete",
        Some(&owner),
        Some(serde_json::json!({"current_password": "gwens-password1"})),
    )
    .await;
    assert_status(&blocked, StatusCode::CONFLICT);
    let body = json_body(blocked).await;
    assert_eq!(body["error"], "owner_of_groups");
    let listed = body["groups"].as_array().unwrap();
    assert_eq!(listed.len(), 1, "the blocking group must be listed: {body}");
    assert_eq!(listed[0]["id"], group_id.as_str());
    assert_eq!(listed[0]["name"], "Famille Gwen");

    // Nothing was scheduled: the 409 is a refusal, not a partial success.
    // Runtime queries rather than `query!`: no `.sqlx` entry to add for a
    // test-only lookup (CI builds with `SQLX_OFFLINE=true`).
    let deletion_requested = |email: &'static str| {
        sqlx::query_scalar::<_, Option<chrono::DateTime<chrono::Utc>>>(
            "SELECT deletion_requested_at FROM users WHERE email = $1",
        )
        .bind(email)
        .fetch_one(&db)
    };
    assert!(deletion_requested("gwen@example.test")
        .await
        .unwrap()
        .is_none());

    // The guard must still let through a caller who owns no group, while
    // another user of the database owns one.
    let allowed = call(
        &router,
        Method::POST,
        "/account/delete",
        Some(&loner),
        Some(serde_json::json!({"current_password": "hugos-password1"})),
    )
    .await;
    assert_status(&allowed, StatusCode::OK);
    assert!(deletion_requested("hugo@example.test")
        .await
        .unwrap()
        .is_some());

    drop(router);
    drop_prescribed_role(&db, app_db, &role).await;
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
            "declares_minimum_age": true, "accepts_terms": true
        })),
    )
    .await;

    let old_token = common::verification_token(&db, "fred@example.test").await;

    // Age the registration token past the cooldown window.
    sqlx::query(
        "UPDATE email_verification_tokens SET created_at = now() - interval '10 minutes' WHERE token_hash = $1",
    )
    .bind(&token_hash(&old_token).unwrap()[..])
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
    let new_token = common::verification_token(&db, "fred@example.test").await;
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
            "declares_minimum_age": true, "accepts_terms": true
        })),
    )
    .await;
    let token = common::verification_token(&db, "grace@example.test").await;
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
            "declares_minimum_age": true, "accepts_terms": true
        })),
    )
    .await;

    let old_token = common::verification_token(&db, "heidi@example.test").await;

    // Age the registration token so the first resend actually issues one.
    sqlx::query(
        "UPDATE email_verification_tokens SET created_at = now() - interval '10 minutes' WHERE token_hash = $1",
    )
    .bind(&token_hash(&old_token).unwrap()[..])
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
    #[allow(clippy::disallowed_methods)]
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

/// #198: an IPv6 client is delegated a whole /64, so the source address it
/// puts on a request is its own choice. Keyed on the /128, ten wrong
/// passwords then cost the attacker one line of a shell loop; keyed on the
/// /64, the eleventh attempt is refused whatever address it arrives from.
#[sqlx::test]
async fn an_ipv6_client_cannot_lift_the_lock_by_rotating_inside_its_block(db: PgPool) {
    let router = router_trusting_10_0_0_0_8(db.clone());
    let proxy = "10.0.0.2:40000";

    // Every attempt from a different address, all inside one /64.
    for i in 0..manage_our_home::auth::throttle::MAX_FAILURES {
        let response = login_from(
            &router,
            proxy,
            Some(&format!("2001:db8:1:2::{i:x}")),
            "nobody@example.test",
            "wrong-password1",
        )
        .await;
        assert_status(&response, StatusCode::UNAUTHORIZED);
    }

    let next = login_from(
        &router,
        proxy,
        Some("2001:db8:1:2:ffff:ffff:ffff:ffff"),
        "nobody@example.test",
        "wrong-password1",
    )
    .await;
    assert_status(&next, StatusCode::TOO_MANY_REQUESTS);

    // And the grouping stops at the /64: the block next door is a
    // different subscriber and keeps its own budget.
    let neighbour = login_from(
        &router,
        proxy,
        Some("2001:db8:1:3::1"),
        "nobody@example.test",
        "wrong-password1",
    )
    .await;
    assert_status(&neighbour, StatusCode::UNAUTHORIZED);
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

/// The `restricted` flag of every session row of `user_id`, oldest first.
async fn session_restrictions(db: &PgPool, user_id: Uuid) -> Vec<bool> {
    sqlx::query_scalar("SELECT restricted FROM sessions WHERE user_id = $1 ORDER BY created_at")
        .bind(user_id)
        .fetch_all(db)
        .await
        .unwrap()
}

/// The `session_id=...` pair a response sets, ready to send back.
fn session_cookie_of(response: &axum::response::Response) -> String {
    let value = cookie_value(&set_cookies(response), "session_id").expect("a session cookie");
    format!("session_id={value}")
}

/// #194, #289: an account support deactivated keeps its email and its
/// Google identity, so the identity branch of `callback` still finds it.
/// Signing in with Google again opens a restricted session — the one a
/// correct password opens — and no full session: the app answers it 403
/// `account_deactivated`, and only the deactivated-account page opens.
/// The first sign-in, before the lock, is the control — it shows the stub
/// carries `callback` all the way to `create_session`.
#[sqlx::test]
async fn google_callback_opens_only_a_restricted_session_for_an_account_deactivated_by_support(
    db: PgPool,
) {
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
    assert!(
        after_lock.status().is_redirection(),
        "{}",
        after_lock.status()
    );
    assert!(opens_a_session(&after_lock));
    assert_eq!(session_rows(&db, user_id).await, sessions_at_lock + 1);
    assert_eq!(session_restrictions(&db, user_id).await.last(), Some(&true));
    let cookie = session_cookie_of(&after_lock);
    let me = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&me, StatusCode::FORBIDDEN);
    assert_eq!(json_body(me).await["error"], "account_deactivated");
    let page = call(
        &router,
        Method::GET,
        "/account/deactivated",
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&page, StatusCode::OK);
    assert_eq!(google_identity_rows(&db, user_id).await, 1);
}

/// #194, #289: a deactivated account with no Google identity yet is
/// reached by the email branch of `callback`. The verified profile opens a
/// restricted session, but nothing is written to the account: no identity
/// is bound, so a later attempt still comes through the email branch.
#[sqlx::test]
async fn google_callback_binds_no_identity_to_an_account_deactivated_by_support(db: PgPool) {
    let email = "locked-no-google-194@example.test";
    let router = router_with_google_stub(db.clone(), "google-sub-194-new", email).await;
    register_verify_login(&router, &db, email, "locked-pass-1").await;
    let user_id = user_id_by_email(&db, email).await;

    deactivate_through_support(&router, &db, user_id).await;
    let sessions_at_lock = session_rows(&db, user_id).await;

    let response = google_callback(&router).await;
    assert!(response.status().is_redirection(), "{}", response.status());
    assert_eq!(session_rows(&db, user_id).await, sessions_at_lock + 1);
    assert_eq!(session_restrictions(&db, user_id).await.last(), Some(&true));
    assert_eq!(google_identity_rows(&db, user_id).await, 0);
    let cookie = session_cookie_of(&response);
    let groups = call(&router, Method::GET, "/groups", Some(&cookie), None).await;
    assert_status(&groups, StatusCode::FORBIDDEN);
}

/// #318: an account opened through Google never saw the registration form,
/// so it has no age declaration on file. Its first session is held at the
/// declaration, which then opens the app to that same session.
#[sqlx::test]
async fn a_google_sign_up_is_held_at_the_age_declaration(db: PgPool) {
    let email = "google-new-318@example.test";
    let router = router_with_google_stub(db.clone(), "google-sub-318", email).await;

    let response = google_callback(&router).await;
    assert!(response.status().is_redirection(), "{}", response.status());
    let user_id = user_id_by_email(&db, email).await;
    let declared_at =
        sqlx::query_scalar!("SELECT age_declared_at FROM users WHERE id = $1", user_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(declared_at, None);

    let cookie = session_cookie_of(&response);
    let me = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&me, StatusCode::FORBIDDEN);
    assert_eq!(json_body(me).await["error"], "age_not_declared");

    let declared = call(
        &router,
        Method::POST,
        "/auth/age-declaration",
        Some(&cookie),
        Some(serde_json::json!({"declares_minimum_age": true})),
    )
    .await;
    assert_status(&declared, StatusCode::NO_CONTENT);

    // #319: nor did it accept the CGU — the same session is held at the
    // acceptance next, which then opens the app.
    let me = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&me, StatusCode::FORBIDDEN);
    assert_eq!(json_body(me).await["error"], "terms_not_accepted");
    let accepted = call(
        &router,
        Method::POST,
        "/auth/terms-acceptance",
        Some(&cookie),
        Some(serde_json::json!({"accepts_terms": true})),
    )
    .await;
    assert_status(&accepted, StatusCode::NO_CONTENT);
    let me = call(&router, Method::GET, "/auth/me", Some(&cookie), None).await;
    assert_status(&me, StatusCode::OK);
}

/// #279: with the token deleted at use, expiry is the only thing left that
/// answers 410 on a reset. The expired token is left in place (the hourly
/// purge owns it) and the password is not changed.
#[sqlx::test]
async fn expired_reset_token_answers_gone(db: PgPool) {
    let router = test_router(db.clone());
    register_verify_login(&router, &db, "gina@example.test", "initial-password").await;
    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind("gina@example.test")
        .fetch_one(&db)
        .await
        .unwrap();
    let fresh = new_token();
    sqlx::query(
        "INSERT INTO password_reset_tokens (user_id, expires_at, token_hash) \
         VALUES ($1, now() - interval '1 minute', $2)",
    )
    .bind(user_id)
    .bind(&fresh.hash()[..])
    .execute(&db)
    .await
    .unwrap();
    let token = fresh.value();

    let reset = call(
        &router,
        Method::POST,
        "/auth/password/reset",
        None,
        Some(serde_json::json!({"token": token, "new_password": "brand-new-password"})),
    )
    .await;
    assert_status(&reset, StatusCode::GONE);

    let left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM password_reset_tokens WHERE token_hash = $1")
            .bind(&fresh.hash()[..])
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(left, 1);
    let old_login = call(
        &router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "gina@example.test", "password": "initial-password"})),
    )
    .await;
    assert_status(&old_login, StatusCode::OK);
}

// -- active sessions (#225) -----------------------------------------------

/// A second login on the same account, returning its session cookie.
async fn login_again(router: &axum::Router, email: &str, password: &str) -> String {
    let login = call(
        router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": email, "password": password})),
    )
    .await;
    assert_status(&login, StatusCode::OK);
    set_cookie(&login).unwrap()
}

async fn sessions_of(router: &axum::Router, cookie: &str) -> Vec<serde_json::Value> {
    let res = call(router, Method::GET, "/auth/sessions", Some(cookie), None).await;
    assert_status(&res, StatusCode::OK);
    json_body(res).await.as_array().unwrap().clone()
}

async fn is_logged_in(router: &axum::Router, cookie: &str) -> bool {
    let res = call(router, Method::GET, "/auth/me", Some(cookie), None).await;
    res.status() == StatusCode::OK
}

/// `GET /auth/sessions` lists the caller's live sessions, the current one
/// marked and first, with their dates and internal id — never the token,
/// and never another account's session. A revoked session drops out.
#[sqlx::test]
async fn sessions_lists_the_callers_live_sessions_only(db: PgPool) {
    let router = test_router(db.clone());
    let first =
        register_verify_login(&router, &db, "sess-list@example.test", "list-password1").await;
    let second = login_again(&router, "sess-list@example.test", "list-password1").await;
    let other =
        register_verify_login(&router, &db, "sess-other@example.test", "other-password1").await;
    let revoked = login_again(&router, "sess-list@example.test", "list-password1").await;
    sqlx::query("UPDATE sessions SET revoked_at = now() WHERE id = $1")
        .bind(session_id_of(&db, &revoked).await)
        .execute(&db)
        .await
        .unwrap();

    let res = call(&router, Method::GET, "/auth/sessions", Some(&second), None).await;
    assert_status(&res, StatusCode::OK);
    let listed = json_body(res).await;
    let raw = listed.to_string();
    for cookie in [&first, &second, &other, &revoked] {
        let token = cookie.split_once('=').unwrap().1;
        assert!(!raw.contains(token), "a token leaked: {raw}");
    }

    let listed = listed.as_array().unwrap();
    let ids: Vec<(String, bool)> = listed
        .iter()
        .map(|s| {
            (
                s["id"].as_str().unwrap().to_string(),
                s["current"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        ids,
        vec![
            (session_id_of(&db, &second).await.to_string(), true),
            (session_id_of(&db, &first).await.to_string(), false),
        ]
    );
    for s in listed {
        assert!(s["created_at"].is_string() && s["last_seen_at"].is_string());
        assert_eq!(s.as_object().unwrap().len(), 4, "{s}");
    }

    let res = call(&router, Method::GET, "/auth/sessions", None, None).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
}

/// `POST /auth/sessions/:id/revoke` ends that session and only that one.
/// Another account's session is a 404 and stays alive, as is a session
/// already revoked and an id that names nothing.
#[sqlx::test]
async fn revoking_one_session_ends_it_and_spares_the_rest(db: PgPool) {
    let router = test_router(db.clone());
    let mine = register_verify_login(&router, &db, "sess-one@example.test", "one-password1").await;
    let forgotten = login_again(&router, "sess-one@example.test", "one-password1").await;
    let theirs =
        register_verify_login(&router, &db, "sess-theirs@example.test", "theirs-password1").await;
    let forgotten_id = session_id_of(&db, &forgotten).await;
    let theirs_id = session_id_of(&db, &theirs).await;

    let res = call(
        &router,
        Method::POST,
        &format!("/auth/sessions/{theirs_id}/revoke"),
        Some(&mine),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);
    assert!(is_logged_in(&router, &theirs).await);

    let res = call(
        &router,
        Method::POST,
        &format!("/auth/sessions/{forgotten_id}/revoke"),
        Some(&mine),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(
        set_cookie(&res),
        None,
        "another session's revocation keeps this cookie"
    );
    assert!(!is_logged_in(&router, &forgotten).await);
    assert!(is_logged_in(&router, &mine).await);
    assert!(is_logged_in(&router, &theirs).await);
    assert_eq!(sessions_of(&router, &mine).await.len(), 1);

    for id in [forgotten_id, Uuid::new_v4()] {
        let res = call(
            &router,
            Method::POST,
            &format!("/auth/sessions/{id}/revoke"),
            Some(&mine),
            None,
        )
        .await;
        assert_status(&res, StatusCode::NOT_FOUND);
    }
}

/// Revoking the current session by its id logs the caller out, cookie
/// cleared, as `POST /auth/logout` would.
#[sqlx::test]
async fn revoking_the_current_session_by_id_logs_out(db: PgPool) {
    let router = test_router(db.clone());
    let mine =
        register_verify_login(&router, &db, "sess-self@example.test", "self-password1").await;
    let id = session_id_of(&db, &mine).await;
    let res = call(
        &router,
        Method::POST,
        &format!("/auth/sessions/{id}/revoke"),
        Some(&mine),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(set_cookie(&res), Some(cookie_name_of(&mine)));
    assert!(!is_logged_in(&router, &mine).await);
}

/// `name=` — what [`set_cookie`] returns for the removal of `cookie`.
fn cookie_name_of(cookie: &str) -> String {
    format!("{}=", cookie.split_once('=').unwrap().0)
}

/// `POST /auth/sessions/revoke-all` ends every session of the caller, the
/// current one included (arbitrated 2026-10-02), clears the cookie, and
/// leaves other accounts alone.
#[sqlx::test]
async fn revoking_all_sessions_ends_the_current_one_too(db: PgPool) {
    let router = test_router(db.clone());
    let here = register_verify_login(&router, &db, "sess-all@example.test", "all-password1").await;
    let elsewhere = login_again(&router, "sess-all@example.test", "all-password1").await;
    let theirs =
        register_verify_login(&router, &db, "sess-bystander@example.test", "by-password1").await;

    let res = call(
        &router,
        Method::POST,
        "/auth/sessions/revoke-all",
        Some(&here),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(set_cookie(&res), Some(cookie_name_of(&here)));
    assert!(!is_logged_in(&router, &here).await);
    assert!(!is_logged_in(&router, &elsewhere).await);
    assert!(is_logged_in(&router, &theirs).await);

    let res = call(
        &router,
        Method::POST,
        "/auth/sessions/revoke-all",
        Some(&here),
        None,
    )
    .await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
}

// -- bearer tokens stored by their hash (#335) ------------------------------

/// The three tables that held a bearer token in clear keep only its hash:
/// no `token` column is left, and every row's `token_hash` is a SHA-256.
#[sqlx::test]
async fn token_tables_keep_only_a_32_byte_hash(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "ivan@example.test", "initial-password").await;
    call(
        &router,
        Method::POST,
        "/auth/password/forgot",
        None,
        Some(serde_json::json!({"email": "ivan@example.test"})),
    )
    .await;
    let group = call(
        &router,
        Method::POST,
        "/groups",
        Some(&cookie),
        Some(serde_json::json!({"name": "Foyer"})),
    )
    .await;
    let group_id = json_body(group).await["id"].as_str().unwrap().to_string();
    let invite = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(&cookie),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&invite, StatusCode::CREATED);

    for table in [
        "email_verification_tokens",
        "password_reset_tokens",
        "invitations",
    ] {
        let columns: Vec<String> = sqlx::query_scalar(
            "SELECT column_name::text FROM information_schema.columns
             WHERE table_schema = current_schema() AND table_name = $1",
        )
        .bind(table)
        .fetch_all(&db)
        .await
        .unwrap();
        assert!(
            !columns.iter().any(|c| c == "token"),
            "{table}: {columns:?}"
        );
        let lengths: Vec<i32> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT octet_length(token_hash) FROM {table}"
        )))
        .fetch_all(&db)
        .await
        .unwrap();
        assert!(!lengths.is_empty(), "{table} has no row");
        assert!(lengths.iter().all(|&n| n == 32), "{table}: {lengths:?}");
    }
}

/// A verification token answers once: used, it is gone (410); past its
/// expiry, too. A token not spelled as the api hands them out — the former
/// UUID format among them — is unknown (404), like one that never existed.
#[sqlx::test]
async fn a_verification_token_is_refused_once_used_expired_or_malformed(db: PgPool) {
    let router = test_router(db.clone());
    let verify = |token: String| {
        let router = router.clone();
        async move {
            call(
                &router,
                Method::GET,
                &format!("/auth/verify-email?token={token}"),
                None,
                None,
            )
            .await
        }
    };
    for email in ["judy@example.test", "kim@example.test"] {
        call(
            &router,
            Method::POST,
            "/auth/register",
            None,
            Some(serde_json::json!({
                "email": email, "password": "initial-password", "display_name": "T",
                "declares_minimum_age": true, "accepts_terms": true
            })),
        )
        .await;
    }

    let used = common::verification_token(&db, "judy@example.test").await;
    assert_status(&verify(used.clone()).await, StatusCode::OK);
    assert_status(&verify(used).await, StatusCode::GONE);

    let expired = common::verification_token(&db, "kim@example.test").await;
    sqlx::query(
        "UPDATE email_verification_tokens SET expires_at = now() - interval '1 minute'
         WHERE token_hash = $1",
    )
    .bind(&token_hash(&expired).unwrap()[..])
    .execute(&db)
    .await
    .unwrap();
    assert_status(&verify(expired).await, StatusCode::GONE);

    for malformed in [
        Uuid::new_v4().to_string(),
        "A".repeat(42),
        format!("{}B", "A".repeat(42)),
    ] {
        assert_status(&verify(malformed).await, StatusCode::NOT_FOUND);
    }
    // Neither is a well-formed token nobody was given.
    assert_status(&verify("A".repeat(43)).await, StatusCode::NOT_FOUND);
}

/// Same for a reset: a token in the former UUID format, or malformed, is
/// unknown (404) and changes nothing.
#[sqlx::test]
async fn a_malformed_reset_token_is_unknown(db: PgPool) {
    let router = test_router(db.clone());
    for token in [Uuid::new_v4().to_string(), "A".repeat(44), "A".repeat(43)] {
        let reset = call(
            &router,
            Method::POST,
            "/auth/password/reset",
            None,
            Some(serde_json::json!({"token": token, "new_password": "brand-new-password"})),
        )
        .await;
        assert_status(&reset, StatusCode::NOT_FOUND);
    }
}
