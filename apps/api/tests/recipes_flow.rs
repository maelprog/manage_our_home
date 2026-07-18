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

/// AC: full CRUD lifecycle for a recipe with a structured ingredient list,
/// scoped to a group.
#[sqlx::test]
async fn full_recipe_lifecycle(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "recipe-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "name": "Pates au beurre",
            "instructions": "Cuire les pates, ajouter le beurre.",
            "ingredients": [
                {"name": "Pates", "quantity": 200.0, "unit": "g"},
                {"name": "Beurre", "quantity": 20.0, "unit": "g"},
                {"name": "Sel", "is_optional": true}
            ]
        })),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let recipe = json_body(create).await;
    let recipe_id = recipe["id"].as_str().unwrap().to_string();
    assert_eq!(recipe["name"], "Pates au beurre");
    assert_eq!(recipe["ingredients"].as_array().unwrap().len(), 3);

    let get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/recipes/{recipe_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&get, StatusCode::OK);

    let update = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/recipes/{recipe_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "name": "Pates au beurre et parmesan",
            "ingredients": [
                {"name": "Pates", "quantity": 200.0, "unit": "g"},
                {"name": "Beurre", "quantity": 20.0, "unit": "g"},
                {"name": "Parmesan", "quantity": 30.0, "unit": "g"}
            ]
        })),
    )
    .await;
    assert_status(&update, StatusCode::OK);
    let updated = json_body(update).await;
    assert_eq!(updated["name"], "Pates au beurre et parmesan");
    assert_eq!(
        updated["ingredients"].as_array().unwrap().len(),
        3,
        "ingredient list must be fully replaced, not appended"
    );

    let list = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/recipes"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&list, StatusCode::OK);
    assert_eq!(
        json_body(list).await["recipes"].as_array().unwrap().len(),
        1
    );

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/recipes/{recipe_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::NO_CONTENT);

    let get_after_delete = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/recipes/{recipe_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&get_after_delete, StatusCode::NOT_FOUND);
}

/// AC: a non-member of the group cannot read or write its recipes, even
/// with a valid session for another account.
#[sqlx::test]
async fn non_member_cannot_access_group_recipes(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "recipe-owner2@example.test",
        "owner-password1",
    )
    .await;
    let outsider_cookie = register_verify_login(
        &router,
        &db,
        "recipe-outsider@example.test",
        "outsider-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Riz cantonais", "ingredients": []})),
    )
    .await;
    let recipe_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let outsider_get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/recipes/{recipe_id}"),
        Some(&outsider_cookie),
        None,
    )
    .await;
    assert_status(&outsider_get, StatusCode::FORBIDDEN);

    let outsider_create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes"),
        Some(&outsider_cookie),
        Some(serde_json::json!({"name": "Intrusion", "ingredients": []})),
    )
    .await;
    assert_status(&outsider_create, StatusCode::FORBIDDEN);
}

/// AC: a regular member may create/read recipes, but only the recipe's
/// creator or a group admin/owner may delete it.
#[sqlx::test]
async fn only_creator_or_admin_can_delete_recipe(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "recipe-owner3@example.test",
        "owner-password1",
    )
    .await;
    let member_cookie = register_verify_login(
        &router,
        &db,
        "recipe-member@example.test",
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
        &format!("/groups/{group_id}/recipes"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Tarte", "ingredients": []})),
    )
    .await;
    let recipe_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let member_delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/recipes/{recipe_id}"),
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&member_delete, StatusCode::FORBIDDEN);

    let owner_delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/recipes/{recipe_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&owner_delete, StatusCode::NO_CONTENT);
}

/// AC: an ingredient's quantity cannot go negative, mirroring stocks'
/// validation.
#[sqlx::test]
async fn negative_ingredient_quantity_is_rejected(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "recipe-neg@example.test", "test-password-1234").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "name": "Soupe",
            "ingredients": [{"name": "Carotte", "quantity": -1.0}]
        })),
    )
    .await;
    assert_status(&create, StatusCode::BAD_REQUEST);
}

/// AC: logging a meal records it in meal_history, retrievable via the
/// group's meal-history listing.
#[sqlx::test]
async fn log_meal_records_history(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "recipe-meal@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Omelette", "ingredients": []})),
    )
    .await;
    let recipe_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let log = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes/{recipe_id}/meal-history"),
        Some(&owner_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&log, StatusCode::CREATED);

    let history = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/recipes/meal-history"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&history, StatusCode::OK);
    let entries = json_body(history).await;
    let entries = entries["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["recipe_id"], recipe_id);
}

/// AC: suggestions rank a fully-in-stock recipe above one missing
/// ingredients, list the missing ingredients (for the future Grocery-list
/// epic), and penalize a recipe recently logged in meal_history (variety).
#[sqlx::test]
async fn suggestions_rank_by_stock_match_and_variety(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "recipe-suggest@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Pates", "quantity": 500.0, "unit": "g"})),
    )
    .await;
    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Beurre", "quantity": 100.0, "unit": "g"})),
    )
    .await;

    let full_match = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "name": "Pates au beurre",
            "ingredients": [
                {"name": "Pates", "quantity": 200.0, "unit": "g"},
                {"name": "Beurre", "quantity": 20.0, "unit": "g"}
            ]
        })),
    )
    .await;
    let full_match_id = json_body(full_match).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let partial_match = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "name": "Poulet au riz",
            "ingredients": [
                {"name": "Poulet", "quantity": 300.0, "unit": "g"},
                {"name": "Riz", "quantity": 150.0, "unit": "g"}
            ]
        })),
    )
    .await;
    let partial_match_id = json_body(partial_match).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let suggestions = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/recipes/suggestions"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&suggestions, StatusCode::OK);
    let body = json_body(suggestions).await;
    let list = body["suggestions"].as_array().unwrap();
    assert_eq!(list[0]["recipe_id"], full_match_id);
    assert_eq!(list[0]["score"], 100.0);
    assert_eq!(list[1]["recipe_id"], partial_match_id);
    assert_eq!(
        list[1]["missing_ingredients"].as_array().unwrap().len(),
        2,
        "both ingredients are missing from stock"
    );

    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes/{full_match_id}/meal-history"),
        Some(&owner_cookie),
        Some(serde_json::json!({})),
    )
    .await;

    let suggestions_after_meal = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/recipes/suggestions"),
        Some(&owner_cookie),
        None,
    )
    .await;
    let body_after = json_body(suggestions_after_meal).await;
    let list_after = body_after["suggestions"].as_array().unwrap();
    let full_match_after = list_after
        .iter()
        .find(|s| s["recipe_id"] == full_match_id)
        .unwrap();
    assert_eq!(
        full_match_after["recently_eaten"], true,
        "recipe logged today must be flagged recently_eaten"
    );
    assert_eq!(
        full_match_after["score"], 70.0,
        "recently-eaten recipe must be penalized by RECENCY_PENALTY"
    );
}
