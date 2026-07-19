mod common;

use axum::http::{Method, StatusCode};
use common::{assert_status, call, json_body, set_cookie, test_router};
use sqlx::PgPool;

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
        Some(serde_json::json!({"email": email, "password": password, "display_name": email})),
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

async fn create_group(router: &axum::Router, cookie: &str, name: &str) -> String {
    let res = call(
        router,
        Method::POST,
        "/groups",
        Some(cookie),
        Some(serde_json::json!({"name": name})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

/// AC: full CRUD lifecycle for a manually-entered stock item, scoped to a group.
#[sqlx::test]
async fn full_stock_item_lifecycle(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "stock-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Farine", "quantity": 2.0, "unit": "kg", "reorder_threshold": 0.5})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let item = json_body(create).await;
    let item_id = item["id"].as_str().unwrap().to_string();
    assert_eq!(item["name"], "Farine");
    assert_eq!(item["low_stock"], false);

    let get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&get, StatusCode::OK);

    let update = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"quantity": 0.3})),
    )
    .await;
    assert_status(&update, StatusCode::OK);
    let updated = json_body(update).await;
    assert_eq!(updated["quantity"], 0.3);
    assert_eq!(
        updated["low_stock"], true,
        "quantity at/below threshold must report low_stock"
    );

    let list = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&list, StatusCode::OK);
    assert_eq!(json_body(list).await["items"].as_array().unwrap().len(), 1);

    let low_stock_list = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/stock-items?low_stock=true"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&low_stock_list, StatusCode::OK);
    assert_eq!(
        json_body(low_stock_list).await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::NO_CONTENT);

    let get_after_delete = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&get_after_delete, StatusCode::NOT_FOUND);
}

/// AC: a non-member of the group cannot read or write its stock items, even
/// with a valid session for another account.
#[sqlx::test]
async fn non_member_cannot_access_group_stock_items(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "stock-owner2@example.test", "owner-password1").await;
    let outsider_cookie = register_verify_login(
        &router,
        &db,
        "stock-outsider@example.test",
        "outsider-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Riz", "quantity": 1.0})),
    )
    .await;
    let item_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let outsider_get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&outsider_cookie),
        None,
    )
    .await;
    assert_status(&outsider_get, StatusCode::FORBIDDEN);

    let outsider_create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&outsider_cookie),
        Some(serde_json::json!({"name": "Intrusion", "quantity": 1.0})),
    )
    .await;
    assert_status(&outsider_create, StatusCode::FORBIDDEN);
}

/// AC: quantity and reorder_threshold cannot go negative.
#[sqlx::test]
async fn negative_quantity_is_rejected(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "stock-neg@example.test", "test-password-1234").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Lait", "quantity": -1.0})),
    )
    .await;
    assert_status(&create, StatusCode::BAD_REQUEST);
}

/// AC: `name` is trimmed on write, `category` can be explicitly cleared via
/// `{"category": null}` (distinct from omitting the field), and a blank
/// `unit` is rejected the same way a blank `name` is.
#[sqlx::test]
async fn update_can_clear_category_and_rejects_blank_unit(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "stock-clear@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "  Riz  ", "category": "Cereales", "quantity": 1.0})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let item = json_body(create).await;
    assert_eq!(
        item["name"], "Riz",
        "leading/trailing whitespace must be trimmed on create"
    );
    let item_id = item["id"].as_str().unwrap().to_string();

    let clear_category = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"category": null})),
    )
    .await;
    assert_status(&clear_category, StatusCode::OK);
    assert_eq!(
        json_body(clear_category).await["category"],
        serde_json::Value::Null,
        "explicit null must clear the category, not leave it unchanged"
    );

    let blank_unit = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"unit": "  "})),
    )
    .await;
    assert_status(&blank_unit, StatusCode::BAD_REQUEST);
}

/// AC: `reorder_threshold` can be explicitly cleared via
/// `{"reorder_threshold": null}` (distinct from omitting the field, which
/// must leave it untouched).
#[sqlx::test]
async fn update_can_clear_reorder_threshold(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "stock-clear-threshold@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Sucre", "quantity": 1.0, "reorder_threshold": 0.5})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let item_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let untouched = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"quantity": 2.0})),
    )
    .await;
    assert_status(&untouched, StatusCode::OK);
    assert_eq!(
        json_body(untouched).await["reorder_threshold"],
        0.5,
        "omitting the field must leave reorder_threshold untouched"
    );

    let cleared = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"reorder_threshold": null})),
    )
    .await;
    assert_status(&cleared, StatusCode::OK);
    assert_eq!(
        json_body(cleared).await["reorder_threshold"],
        serde_json::Value::Null,
        "explicit null must clear reorder_threshold, not leave it unchanged"
    );
}

/// AC: a regular member may create/read/adjust stock, but only the item's
/// creator or a group admin/owner may delete it.
#[sqlx::test]
async fn only_creator_or_admin_can_delete_stock_item(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "stock-owner3@example.test", "owner-password1").await;
    let member_cookie = register_verify_login(
        &router,
        &db,
        "stock-member@example.test",
        "member-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let invite = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(&owner_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    let token = json_body(invite).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    call(
        &router,
        Method::POST,
        &format!("/groups/invitations/{token}/accept"),
        Some(&member_cookie),
        None,
    )
    .await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Pâtes", "quantity": 5.0})),
    )
    .await;
    let item_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let member_delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&member_delete, StatusCode::FORBIDDEN);

    let owner_delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&owner_delete, StatusCode::NO_CONTENT);
}
