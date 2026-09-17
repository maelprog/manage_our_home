use lettre::{AsyncSmtpTransport, Tokio1Executor};
use manage_our_home::email::EmailSender;
use manage_our_home::{build_router, AppState};
use oauth2::basic::BasicClient;
use oauth2::{AuthUrl, ClientId, ClientSecret, RedirectUrl, TokenUrl};
use sqlx::PgPool;

/// Builds an `AppState`/router for tests. Email sending targets an
/// unreachable local relay on purpose — handlers log and swallow send
/// failures rather than failing the request, so tests exercise the DB
/// behavior without needing a real mail server.
// TODO: remove #[allow(dead_code)] once every integration test binary uses
// this helper (clippy sees each test binary's copy separately, and flags
// helpers unused by that particular binary as dead code).
#[allow(dead_code)]
pub fn test_state(db: PgPool) -> AppState {
    let google_oauth = BasicClient::new(ClientId::new("test-client-id".into()))
        .set_client_secret(ClientSecret::new("test-client-secret".into()))
        .set_auth_uri(AuthUrl::new("https://accounts.google.com/o/oauth2/v2/auth".into()).unwrap())
        .set_token_uri(TokenUrl::new("https://oauth2.googleapis.com/token".into()).unwrap())
        .set_redirect_uri(
            RedirectUrl::new("http://localhost:8080/auth/google/callback".into()).unwrap(),
        );

    let smtp = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous("127.0.0.1")
        .port(1)
        .build();
    let email = EmailSender::new(smtp, "noreply@example.test".parse().unwrap());

    AppState {
        db: runtime_pool(&db),
        admin_db: db,
        google_oauth,
        google_userinfo_url: manage_our_home::auth::oauth_google::GOOGLE_USERINFO_URL.into(),
        email,
        public_base_url: "http://localhost:8080".into(),
        frontend_base_url: "http://localhost:5173".into(),
        oauth_encryption_key: "test-encryption-key".into(),
        message_encryption_key: "test-message-encryption-key".into(),
        calendar_feed_encryption_key: "test-calendar-feed-encryption-key".into(),
        message_hubs: manage_our_home::messagerie::MessageHub::new(),
        // Production default; tests that exercise the WS membership recheck
        // (AC #7) override this on their own state to keep the bound short.
        message_ws_recheck_interval: std::time::Duration::from_secs(30),
        secure_cookies: false,
        storage: test_storage(),
        // No socket behind these requests (the router is driven directly),
        // so there is no peer to trust and `X-Forwarded-For` is ignored:
        // every test client resolves to the unspecified address. Tests
        // that need several distinct clients build their own state.
        trusted_proxies: std::sync::Arc::new(manage_our_home::client_ip::TrustedProxies::none()),
        login_throttle: std::sync::Arc::new(manage_our_home::auth::throttle::LoginThrottle::new()),
        login_branches: std::sync::Arc::new(
            manage_our_home::auth::timing::BranchCounters::default(),
        ),
    }
}

/// The pool the handlers serve requests from (`AppState::db`).
///
/// By default it is the `#[sqlx::test]` pool itself, whose role is
/// `DATABASE_URL`'s: a superuser in CI, which bypasses RLS. A handler that
/// reads through the bare pool, without `app.user_id` set, sees the whole
/// database there and nothing in production (#113, #207, #208).
///
/// When `FLOW_TEST_RUNTIME_ROLE` is set (CI job `test-nobypassrls`, #213),
/// the handlers reach the same throwaway database as that role instead, as
/// apps/api/README.md prescribes for `DATABASE_URL`. The test body keeps the
/// harness pool for its fixtures and assertions, and so does `admin_db`,
/// whose production role bypasses RLS too. The role's table grants come from
/// default privileges the job declares in `template1`, since every test
/// database is created after the role.
///
/// Each connection checks its own role and refuses to open if it bypasses
/// RLS, so a misconfigured job fails every request instead of passing on a
/// superuser connection.
fn runtime_pool(db: &PgPool) -> PgPool {
    let Ok(role) = std::env::var("FLOW_TEST_RUNTIME_ROLE") else {
        return db.clone();
    };
    let password = std::env::var("FLOW_TEST_RUNTIME_ROLE_PASSWORD")
        .expect("FLOW_TEST_RUNTIME_ROLE is set without FLOW_TEST_RUNTIME_ROLE_PASSWORD");
    let options = (*db.connect_options())
        .clone()
        .username(&role)
        .password(&password);
    sqlx::postgres::PgPoolOptions::new()
        // The bounds `#[sqlx::test]` gives its own pool: this one comes on
        // top of it, against the server's connection limit.
        .max_connections(5)
        .idle_timeout(Some(std::time::Duration::from_secs(1)))
        .after_connect(|conn, _| {
            Box::pin(async move {
                let bypasses: bool = sqlx::query_scalar(
                    "SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname = current_user",
                )
                .fetch_one(&mut *conn)
                .await?;
                if bypasses {
                    return Err(sqlx::Error::Configuration(
                        "FLOW_TEST_RUNTIME_ROLE bypasses RLS".into(),
                    ));
                }
                Ok(())
            })
        })
        .connect_lazy_with(options)
}

/// Points at an unreachable local MinIO endpoint on purpose — most tests
/// never touch object storage, and the ones that assert on a *failing*
/// storage (the delete-ordering guards) want exactly this.
// TODO: remove #[allow(dead_code)] once every integration test binary uses
// this helper (see note on test_state above).
#[allow(dead_code)]
fn test_storage() -> manage_our_home::storage::Storage {
    manage_our_home::storage::Storage::new(
        minio_client("http://127.0.0.1:1", "test", "test"),
        "test-bucket".into(),
    )
}

fn minio_client(endpoint: &str, access_key: &str, secret_key: &str) -> aws_sdk_s3::Client {
    use aws_credential_types::Credentials;
    use aws_sdk_s3::config::{BehaviorVersion, Region};

    let credentials = Credentials::new(access_key, secret_key, None, None, "minio-static");
    let config = aws_sdk_s3::config::Builder::new()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-east-1"))
        .endpoint_url(endpoint)
        .credentials_provider(credentials)
        .force_path_style(true)
        .build();
    aws_sdk_s3::Client::from_conf(config)
}

/// A real MinIO client, when the suite runs against one (CI's `test` job
/// starts one; see `.github/workflows/ci.yml`). Tests that need to assert
/// on the actual stored bytes — not just on the metadata rows — skip
/// themselves when this returns `None`, so `cargo test` still passes on a
/// checkout with nothing but Postgres.
// TODO: remove #[allow(dead_code)] once every integration test binary uses
// this helper (see note on test_state above).
#[allow(dead_code)]
pub fn real_minio_from_env() -> Option<(aws_sdk_s3::Client, String)> {
    let endpoint = std::env::var("MINIO_ENDPOINT").ok()?;
    let access_key = std::env::var("MINIO_ACCESS_KEY").ok()?;
    let secret_key = std::env::var("MINIO_SECRET_KEY").ok()?;
    let bucket = std::env::var("MINIO_BUCKET").ok()?;
    Some((minio_client(&endpoint, &access_key, &secret_key), bucket))
}

/// Router whose `AppState` talks to the given object storage, for the few
/// tests that upload real bytes (see `real_minio_from_env`).
// TODO: remove #[allow(dead_code)] once every integration test binary uses
// this helper (see note on test_state above).
#[allow(dead_code)]
pub fn test_router_with_storage(
    db: PgPool,
    storage: manage_our_home::storage::Storage,
) -> axum::Router {
    let mut state = test_state(db);
    state.storage = storage;
    build_router(state)
}

// TODO: remove #[allow(dead_code)] once every integration test binary uses
// this helper (see note on test_state above).
#[allow(dead_code)]
pub fn test_router(db: PgPool) -> axum::Router {
    build_router(test_state(db))
}

use axum::body::Body;
use axum::http::{header, Method, Request, Response, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

// TODO: remove #[allow(dead_code)] once every integration test binary uses
// this helper (see note on test_state above).
#[allow(dead_code)]
pub async fn call(
    router: &axum::Router,
    method: Method,
    uri: &str,
    cookie: Option<&str>,
    body: Option<serde_json::Value>,
) -> Response<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(c) = cookie {
        builder = builder.header(header::COOKIE, c);
    }
    let body = match body {
        Some(v) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&v).unwrap())
        }
        None => Body::empty(),
    };
    let request = builder.body(body).unwrap();
    router.clone().oneshot(request).await.unwrap()
}

/// Posts a single `file` field as `multipart/form-data`, the shape
/// `upload_attachment` reads. Hand-rolled because the suite has no
/// multipart client and one field needs no more than this.
// TODO: remove #[allow(dead_code)] once every integration test binary uses
// this helper (see note on test_state above).
#[allow(dead_code)]
pub async fn call_upload(
    router: &axum::Router,
    uri: &str,
    cookie: &str,
    filename: &str,
    bytes: &[u8],
) -> Response<Body> {
    const BOUNDARY: &str = "----manageourhometestboundary";

    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());

    let request = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::COOKIE, cookie)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .unwrap();
    router.clone().oneshot(request).await.unwrap()
}

// TODO: remove #[allow(dead_code)] once every integration test binary uses
// this helper (see note on test_state above).
#[allow(dead_code)]
pub fn set_cookie(response: &Response<Body>) -> Option<String> {
    response
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string())
}

// TODO: remove #[allow(dead_code)] once every integration test binary uses
// this helper (see note on test_state above).
#[allow(dead_code)]
pub async fn json_body(response: Response<Body>) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

// TODO: remove #[allow(dead_code)] once every integration test binary uses
// this helper (see note on test_state above).
#[allow(dead_code)]
pub fn assert_status(response: &Response<Body>, expected: StatusCode) {
    assert_eq!(response.status(), expected, "unexpected status code");
}

/// A pool connected as a fresh `NOSUPERUSER NOBYPASSRLS` login role with
/// the grants apps/api/README.md prescribes for `DATABASE_URL`. The
/// `#[sqlx::test]` pool is the harness role, which bypasses RLS: a handler
/// driven through it cannot tell a policy that filters correctly from one
/// that filters nothing, or everything (issue #113).
// TODO: remove #[allow(dead_code)] once every integration test binary uses
// this helper (see note on test_state above).
#[allow(dead_code)]
pub async fn prescribed_role_pool(db: &PgPool) -> (String, PgPool) {
    let role = format!("app_flow_role_{}", uuid::Uuid::new_v4().simple());
    for statement in [
        format!("CREATE ROLE {role} LOGIN PASSWORD 'flow-test-password' NOSUPERUSER NOBYPASSRLS"),
        format!("GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO {role}"),
        format!("GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO {role}"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(db)
            .await
            .unwrap();
    }
    let options = (*db.connect_options())
        .clone()
        .username(&role)
        .password("flow-test-password");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .unwrap();
    (role, pool)
}

/// Closes the pool `prescribed_role_pool` opened and drops the role it
/// created, which the `#[sqlx::test]` teardown does not do on its own:
/// roles are cluster-wide, the throwaway database is not.
// TODO: remove #[allow(dead_code)] once every integration test binary uses
// this helper (see note on test_state above).
#[allow(dead_code)]
pub async fn drop_prescribed_role(db: &PgPool, pool: PgPool, role: &str) {
    pool.close().await;
    for statement in [format!("DROP OWNED BY {role}"), format!("DROP ROLE {role}")] {
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(db)
            .await
            .unwrap();
    }
}
