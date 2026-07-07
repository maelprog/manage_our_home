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
pub fn test_state(db: PgPool) -> AppState {
    let google_oauth = BasicClient::new(
        ClientId::new("test-client-id".into()),
        Some(ClientSecret::new("test-client-secret".into())),
        AuthUrl::new("https://accounts.google.com/o/oauth2/v2/auth".into()).unwrap(),
        Some(TokenUrl::new("https://oauth2.googleapis.com/token".into()).unwrap()),
    )
    .set_redirect_uri(RedirectUrl::new("http://localhost:8080/auth/google/callback".into()).unwrap());

    let smtp = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous("127.0.0.1")
        .port(1)
        .build();
    let email = EmailSender::new(smtp, "noreply@example.test".parse().unwrap());

    AppState {
        db,
        google_oauth,
        email,
        public_base_url: "http://localhost:8080".into(),
        frontend_base_url: "http://localhost:5173".into(),
        oauth_encryption_key: "test-encryption-key".into(),
        secure_cookies: false,
    }
}

pub fn test_router(db: PgPool) -> axum::Router {
    build_router(test_state(db))
}

use axum::body::Body;
use axum::http::{header, Method, Request, Response, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

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

pub fn set_cookie(response: &Response<Body>) -> Option<String> {
    response
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string())
}

pub async fn json_body(response: Response<Body>) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

pub fn assert_status(response: &Response<Body>, expected: StatusCode) {
    assert_eq!(response.status(), expected, "unexpected status code");
}
