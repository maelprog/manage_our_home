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

/// AC: full CRUD lifecycle for a manually-entered grocery item, scoped to a group.
#[sqlx::test]
async fn full_grocery_item_lifecycle(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "grocery-owner@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Farine", "quantity": 1.0, "unit": "kg"})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let item = json_body(create).await;
    let item_id = item["id"].as_str().unwrap().to_string();
    assert_eq!(item["name"], "Farine");
    assert_eq!(item["checked"], false);
    assert_eq!(item["source"], "manual");

    let get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/grocery-items/{item_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&get, StatusCode::OK);

    let update = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/grocery-items/{item_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"quantity": 2.0})),
    )
    .await;
    assert_status(&update, StatusCode::OK);
    assert_eq!(json_body(update).await["quantity"], 2.0);

    let list = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/grocery-items"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&list, StatusCode::OK);
    assert_eq!(json_body(list).await["items"].as_array().unwrap().len(), 1);

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/grocery-items/{item_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::NO_CONTENT);

    let get_after_delete = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/grocery-items/{item_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&get_after_delete, StatusCode::NOT_FOUND);
}

/// AC: any member (not just the creator) may check/uncheck an item off the
/// shared list, even though only the creator or an admin/owner may edit or
/// delete it.
#[sqlx::test]
async fn any_member_can_check_item(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "grocery-owner2@example.test",
        "owner-password1",
    )
    .await;
    let member_cookie = register_verify_login(
        &router,
        &db,
        "grocery-member@example.test",
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
        &format!("/groups/{group_id}/grocery-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Lait"})),
    )
    .await;
    let item_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let member_check = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items/{item_id}/check"),
        Some(&member_cookie),
        Some(serde_json::json!({"checked": true})),
    )
    .await;
    assert_status(&member_check, StatusCode::OK);
    assert_eq!(json_body(member_check).await["checked"], true);

    let member_edit = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/grocery-items/{item_id}"),
        Some(&member_cookie),
        Some(serde_json::json!({"name": "Lait entier"})),
    )
    .await;
    assert_status(&member_edit, StatusCode::FORBIDDEN);

    let member_delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/grocery-items/{item_id}"),
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&member_delete, StatusCode::FORBIDDEN);

    let owner_delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/grocery-items/{item_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&owner_delete, StatusCode::NO_CONTENT);
}

/// AC: a non-member of the group cannot read or write its grocery items.
#[sqlx::test]
async fn non_member_cannot_access_group_grocery_items(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "grocery-owner3@example.test",
        "owner-password1",
    )
    .await;
    let outsider_cookie = register_verify_login(
        &router,
        &db,
        "grocery-outsider@example.test",
        "outsider-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Riz"})),
    )
    .await;
    let item_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let outsider_get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/grocery-items/{item_id}"),
        Some(&outsider_cookie),
        None,
    )
    .await;
    assert_status(&outsider_get, StatusCode::FORBIDDEN);

    let outsider_create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items"),
        Some(&outsider_cookie),
        Some(serde_json::json!({"name": "Intrusion"})),
    )
    .await;
    assert_status(&outsider_create, StatusCode::FORBIDDEN);
}

/// AC: `name` is required (rejects blank) and `quantity` cannot go negative.
#[sqlx::test]
async fn validation_rejects_blank_name_and_negative_quantity(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "grocery-valid@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let blank_name = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "   "})),
    )
    .await;
    assert_status(&blank_name, StatusCode::BAD_REQUEST);

    let negative_qty = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Beurre", "quantity": -1.0})),
    )
    .await;
    assert_status(&negative_qty, StatusCode::BAD_REQUEST);
}

/// AC: generating grocery items pulls in a recipe's missing (required,
/// not-in-stock) ingredients and stock items at/below their reorder
/// threshold, without duplicating an item that's already on the unchecked
/// list.
#[sqlx::test]
async fn generate_pulls_missing_ingredients_and_low_stock(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "grocery-gen@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    // Stock: "Farine" is in stock (so it won't be "missing"), "Lait" is
    // below its reorder threshold (so it should be pulled in as low_stock).
    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Farine", "quantity": 2.0})),
    )
    .await;
    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Lait", "quantity": 0.1, "reorder_threshold": 0.5})),
    )
    .await;

    // Recipe requiring "Farine" (in stock, not missing) and "Oeufs" (not
    // in stock, so it should be pulled in as a missing ingredient).
    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "name": "Crepes",
            "ingredients": [
                {"name": "Farine", "is_optional": false},
                {"name": "Oeufs", "is_optional": false}
            ]
        })),
    )
    .await;

    let generate = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items/generate"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&generate, StatusCode::CREATED);
    let created = json_body(generate).await;
    let items = created["items"].as_array().unwrap();
    let names: Vec<&str> = items.iter().map(|i| i["name"].as_str().unwrap()).collect();

    assert!(
        names.contains(&"Oeufs"),
        "missing recipe ingredient must be pulled in: {names:?}"
    );
    assert!(
        names.contains(&"Lait"),
        "low stock item must be pulled in: {names:?}"
    );
    assert!(
        !names.contains(&"Farine"),
        "in-stock ingredient must not be pulled in: {names:?}"
    );

    // Running generate again must not duplicate the already-present items.
    let regenerate = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items/generate"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&regenerate, StatusCode::CREATED);
    assert_eq!(
        json_body(regenerate).await["items"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "re-running generate must not duplicate existing unchecked items"
    );
}
