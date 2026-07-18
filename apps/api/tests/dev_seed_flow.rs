mod common;

use axum::http::{Method, StatusCode};
use common::{assert_status, call, json_body, set_cookie, test_router};
use manage_our_home::dev_seed::{seed_dev_users, DEV_GROUP_NAME, DEV_PASSWORD, DEV_USERS};
use sqlx::PgPool;

/// The dev seed creates pre-verified users that can log in immediately, a
/// shared group both belong to, and running it again is a no-op (it runs on
/// every API boot when DEV_SEED_USERS=true).
#[sqlx::test]
async fn seed_is_idempotent_and_users_can_log_in(db: PgPool) {
    seed_dev_users(&db).await.unwrap();
    seed_dev_users(&db).await.unwrap();

    let user_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM users WHERE email LIKE '%@example.test'",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(user_count, DEV_USERS.len() as i64);

    let router = test_router(db.clone());
    let login = call(
        &router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": DEV_USERS[0].0, "password": DEV_PASSWORD})),
    )
    .await;
    assert_status(&login, StatusCode::OK);
    let cookie = set_cookie(&login).expect("login should set a session cookie");

    // Both users are members of the seeded group, visible through the
    // RLS-scoped listing endpoint.
    let groups = call(&router, Method::GET, "/groups", Some(&cookie), None).await;
    assert_status(&groups, StatusCode::OK);
    let groups = json_body(groups).await;
    let group = groups
        .as_array()
        .and_then(|a| a.first())
        .unwrap_or(&serde_json::Value::Null);
    assert_eq!(group["name"], DEV_GROUP_NAME);

    let member_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM group_members gm JOIN users u ON u.id = gm.user_id \
         WHERE u.email LIKE '%@example.test'",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(member_count, DEV_USERS.len() as i64);
}
