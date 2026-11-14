//! What the tests of #817 share: a family of two, and a client that
//! returns the status and the JSON of an answer.

use axum::http::{Method, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::common::{assert_status, call, json_body, set_cookie};

pub const TAG: &str = "en:peanuts";

pub async fn register_verify_login(router: &axum::Router, db: &PgPool, email: &str) -> String {
    call(
        router,
        Method::POST,
        "/auth/register",
        None,
        Some(
            json!({"email": email, "password": "member-password1", "display_name": email,
                    "declares_minimum_age": true, "accepts_terms": true}),
        ),
    )
    .await;
    let token = crate::common::verification_token(db, email).await;
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
        Some(json!({"email": email, "password": "member-password1"})),
    )
    .await;
    set_cookie(&login).unwrap()
}

pub async fn user_id(db: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(db)
        .await
        .unwrap()
}

pub async fn create_group(router: &axum::Router, cookie: &str, name: &str) -> String {
    let res = call(
        router,
        Method::POST,
        "/groups",
        Some(cookie),
        Some(json!({"name": name})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

pub async fn join(router: &axum::Router, owner: &str, member: &str, group: &str) {
    let invite = call(
        router,
        Method::POST,
        &format!("/groups/{group}/invitations"),
        Some(owner),
        Some(json!({})),
    )
    .await;
    let token = json_body(invite).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let accept = call(
        router,
        Method::POST,
        &format!("/groups/invitations/{token}/accept"),
        Some(member),
        None,
    )
    .await;
    assert_status(&accept, StatusCode::OK);
}

/// A family of two: `owner` and `holder`, the member whose allergies the
/// tests are about.
pub struct Family {
    pub group: String,
    pub owner: String,
    pub holder: String,
    // Read by `dietary_constraints_flow` only, not by the logs binary.
    #[allow(dead_code)]
    pub holder_id: Uuid,
}

pub async fn family(router: &axum::Router, db: &PgPool, prefix: &str) -> Family {
    let owner = register_verify_login(router, db, &format!("{prefix}-owner@example.test")).await;
    let holder_email = format!("{prefix}-holder@example.test");
    let holder = register_verify_login(router, db, &holder_email).await;
    let group = create_group(router, &owner, "Maison").await;
    join(router, &owner, &holder, &group).await;
    Family {
        group,
        owner,
        holder,
        holder_id: user_id(db, &holder_email).await,
    }
}

pub async fn send(
    router: &axum::Router,
    method: Method,
    uri: &str,
    cookie: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let res = call(router, method, uri, Some(cookie), body).await;
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}
