//! `POST /csp-report` on the real router (#325): the endpoint
//! infra/Caddyfile's Content-Security-Policy reports to. What a report is
//! read into is covered by `csp_report`'s own tests; these check the route
//! is mounted, answers both report formats, is left out of the origin guard,
//! and bounds its body.

mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use common::test_router;
use sqlx::PgPool;
use tower::ServiceExt;

async fn post(
    router: &axum::Router,
    content_type: &str,
    body: Vec<u8>,
    browser: &[(&str, &str)],
) -> StatusCode {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/csp-report")
        .header(header::HOST, "localhost:8080")
        .header(header::CONTENT_TYPE, content_type);
    for (k, v) in browser {
        builder = builder.header(*k, *v);
    }
    router
        .clone()
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap()
        .status()
}

const LEGACY: &str = r#"{"csp-report": {"document-uri": "https://maison.example.org/login",
    "effective-directive": "script-src-elem", "blocked-uri": "inline", "disposition": "enforce"}}"#;

const BATCH: &str = r#"[{"type": "csp-violation", "body": {"documentURL": "https://maison.example.org/",
    "effectiveDirective": "script-src-attr", "blockedURL": "inline", "disposition": "enforce"}}]"#;

#[sqlx::test]
async fn both_report_formats_are_accepted(db: PgPool) {
    let router = test_router(db);
    assert_eq!(
        post(&router, "application/csp-report", LEGACY.into(), &[]).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        post(&router, "application/reports+json", BATCH.into(), &[]).await,
        StatusCode::NO_CONTENT
    );
    // Unreadable: nothing logged, and nothing to tell the sender.
    assert_eq!(
        post(&router, "application/json", b"not json".to_vec(), &[]).await,
        StatusCode::NO_CONTENT
    );
}

#[sqlx::test]
async fn reports_are_not_held_to_the_origin_guard(db: PgPool) {
    let router = test_router(db);
    let cross_site = [
        ("sec-fetch-site", "cross-site"),
        ("origin", "https://evil.test"),
    ];
    assert_eq!(
        post(
            &router,
            "application/csp-report",
            LEGACY.into(),
            &cross_site
        )
        .await,
        StatusCode::NO_CONTENT
    );
}

#[sqlx::test]
async fn an_oversized_report_is_refused(db: PgPool) {
    let router = test_router(db);
    let padding = "a".repeat(manage_our_home::csp_report::MAX_REPORT_BODY_BYTES);
    let body = format!(
        r#"{{"csp-report": {{"document-uri": "https://h/{padding}", "blocked-uri": "inline"}}}}"#
    );
    assert_eq!(
        post(&router, "application/csp-report", body.into(), &[]).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[sqlx::test]
async fn only_post_is_routed(db: PgPool) {
    let res = test_router(db)
        .oneshot(
            Request::builder()
                .uri("/csp-report")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
}
