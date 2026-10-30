mod common;

use axum::http::{Method, StatusCode};
use chrono::{Days, NaiveDate, Utc};
use common::{assert_status, call, json_body, set_cookie, test_router};
use manage_our_home_shared::validation::auth::paris_day;
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
        Some(serde_json::json!({"email": email, "password": password, "display_name": email, "declares_minimum_age": true, "accepts_terms": true})),
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
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "recipe-neg@example.test",
        "test-password-1234",
    )
    .await;
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
/// group's meal-history listing. Without a date it is dated today in
/// Paris, not on the database's UTC day (#608).
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

    let before = paris_day(Utc::now());
    let log = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes/{recipe_id}/meal-history"),
        Some(&owner_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    let after = paris_day(Utc::now());
    assert_status(&log, StatusCode::CREATED);
    let eaten_on: NaiveDate = json_body(log).await["eaten_on"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        eaten_on == before || eaten_on == after,
        "an undated meal is dated today in Paris, got {eaten_on}"
    );

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

/// AC: the history's default 14-day window counts back from today in
/// Paris (#608): a meal 14 days ago is in it, one 15 days ago is not.
#[sqlx::test]
async fn meal_history_window_counts_back_from_the_paris_day(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "recipe-window@example.test",
        "owner-password1",
    )
    .await;
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

    let today = paris_day(Utc::now());
    for days in [14, 15] {
        let eaten_on = today.checked_sub_days(Days::new(days)).unwrap();
        let log = call(
            &router,
            Method::POST,
            &format!("/groups/{group_id}/recipes/{recipe_id}/meal-history"),
            Some(&owner_cookie),
            Some(serde_json::json!({"eaten_on": eaten_on})),
        )
        .await;
        assert_status(&log, StatusCode::CREATED);
    }

    let history = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/recipes/meal-history"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&history, StatusCode::OK);
    if paris_day(Utc::now()) != today {
        // Midnight in Paris passed during the test: the window moved.
        return;
    }
    let entries = json_body(history).await;
    let days: Vec<&str> = entries["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["eaten_on"].as_str().unwrap())
        .collect();
    let fourteen = today.checked_sub_days(Days::new(14)).unwrap().to_string();
    assert_eq!(days, vec![fourteen.as_str()]);
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

/// #406: an article covers a recipe's ingredient through Open Food Facts'
/// categories. A scanned article takes the categories of its product (here
/// "Lait demi-écrémé Lactel", under en:semi-skimmed-milks, covers "Lait");
/// an article typed by hand takes them from its name ("Oeufs" covers
/// "œuf"). A descendant never covers its ancestor's sibling: the milk does
/// not cover "Lait de coco".
#[sqlx::test]
async fn suggestions_match_stock_through_the_category_taxonomy(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "recipe-taxonomy@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;

    // What a scan would have cached (#402), written directly: the test
    // must not reach Open Food Facts.
    sqlx::query("INSERT INTO off_products (code, name, categories_tags) VALUES ($1, $2, $3)")
        .bind("4006381333931")
        .bind("Lait demi-écrémé Lactel")
        .bind(vec!["en:dairies", "en:milks", "en:semi-skimmed-milks"])
        .execute(&db)
        .await
        .unwrap();

    for item in [
        serde_json::json!({"name": "Lait demi-écrémé Lactel", "quantity": 1.0, "barcode": "4006381333931"}),
        serde_json::json!({"name": "Oeufs", "quantity": 6.0}),
    ] {
        let res = call(
            &router,
            Method::POST,
            &format!("/groups/{group_id}/stock-items"),
            Some(&cookie),
            Some(item),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
    }

    let recipe = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes"),
        Some(&cookie),
        Some(serde_json::json!({
            "name": "Crêpes coco",
            "ingredients": [
                {"name": "Lait", "quantity": 500.0, "unit": "ml"},
                {"name": "œuf", "quantity": 3.0},
                {"name": "Lait de coco", "quantity": 200.0, "unit": "ml"}
            ]
        })),
    )
    .await;
    assert_status(&recipe, StatusCode::CREATED);

    let res = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/recipes/suggestions"),
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let body = json_body(res).await;
    let suggestion = &body["suggestions"][0];
    assert_eq!(suggestion["matched_ingredients"], 2);
    assert_eq!(suggestion["total_required_ingredients"], 3);
    let missing: Vec<&str> = suggestion["missing_ingredients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(missing, ["Lait de coco"]);
}

/// #539: `?possible=true` keeps the recipes whose every required
/// ingredient is in stock, by the same matching as the suggestions. A
/// missing optional ingredient does not exclude a recipe; a missing
/// required one does; a recipe without ingredients is not « possible ».
/// The filter applies before `limit`.
#[sqlx::test]
async fn the_possible_filter_keeps_the_recipes_the_stock_covers(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "recipe-possible@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;

    for item in [
        serde_json::json!({"name": "Farine", "quantity": 1000.0, "unit": "g"}),
        serde_json::json!({"name": "Oeufs", "quantity": 6.0}),
    ] {
        let res = call(
            &router,
            Method::POST,
            &format!("/groups/{group_id}/stock-items"),
            Some(&cookie),
            Some(item),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
    }

    let mut ids = Vec::new();
    for (name, ingredients) in [
        (
            "Pâte à pain",
            serde_json::json!([
                {"name": "Farine", "quantity": 500.0, "unit": "g"},
                {"name": "œuf", "quantity": 1.0},
                {"name": "Graines de sésame", "is_optional": true}
            ]),
        ),
        (
            "Crêpes",
            serde_json::json!([
                {"name": "Farine", "quantity": 250.0, "unit": "g"},
                {"name": "Lait", "quantity": 500.0, "unit": "ml"}
            ]),
        ),
        ("Recette sans ingrédient", serde_json::json!([])),
    ] {
        let res = call(
            &router,
            Method::POST,
            &format!("/groups/{group_id}/recipes"),
            Some(&cookie),
            Some(serde_json::json!({"name": name, "ingredients": ingredients})),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
        ids.push(json_body(res).await["id"].as_str().unwrap().to_string());
    }

    let names = |body: serde_json::Value| -> Vec<String> {
        body["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_string())
            .collect()
    };

    // Unfiltered, all three are suggested.
    let res = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/recipes/suggestions"),
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(names(json_body(res).await).len(), 3);

    for path in ["?possible=true", "?possible=true&limit=1"] {
        let res = call(
            &router,
            Method::GET,
            &format!("/groups/{group_id}/recipes/suggestions{path}"),
            Some(&cookie),
            None,
        )
        .await;
        assert_status(&res, StatusCode::OK);
        let body = json_body(res).await;
        assert_eq!(names(body.clone()), ["Pâte à pain"], "{path}");
        assert_eq!(body["suggestions"][0]["recipe_id"], ids[0]);
        assert_eq!(body["suggestions"][0]["matched_ingredients"], 2);
        assert_eq!(body["suggestions"][0]["total_required_ingredients"], 2);
    }

    // Once its milk is in stock, the crêpes are possible too.
    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&cookie),
        Some(serde_json::json!({"name": "Lait", "quantity": 1.0, "unit": "L"})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let res = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/recipes/suggestions?possible=true"),
        Some(&cookie),
        None,
    )
    .await;
    let mut possible = names(json_body(res).await);
    possible.sort();
    assert_eq!(possible, ["Crêpes", "Pâte à pain"]);
}

/// #539: the possible filter drops the recipes before `limit` cuts the
/// list. Two recipes with only an optional, always-in-season ingredient
/// are created first and score above the possible one (no required
/// ingredient counts as fully matched, plus the seasonal bonus), so a
/// filter applied after the cut would return nothing.
#[sqlx::test]
async fn the_possible_filter_applies_before_the_limit(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "recipe-possible-limit@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;

    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&cookie),
        Some(serde_json::json!({"name": "Farine", "quantity": 1000.0, "unit": "g"})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);

    let all_months: Vec<i32> = (1..=12).collect();
    for (name, ingredients) in [
        (
            "Salade d'herbes",
            serde_json::json!([
                {"name": "Ciboulette", "is_optional": true, "seasonal_months": all_months}
            ]),
        ),
        (
            "Bouquet garni",
            serde_json::json!([
                {"name": "Persil", "is_optional": true, "seasonal_months": all_months}
            ]),
        ),
        (
            "Pain",
            serde_json::json!([{"name": "Farine", "quantity": 500.0, "unit": "g"}]),
        ),
    ] {
        let res = call(
            &router,
            Method::POST,
            &format!("/groups/{group_id}/recipes"),
            Some(&cookie),
            Some(serde_json::json!({"name": name, "ingredients": ingredients})),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
    }

    // Unfiltered, the two recipes without a required ingredient lead.
    let res = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/recipes/suggestions?limit=1"),
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let body = json_body(res).await;
    assert_ne!(body["suggestions"][0]["name"], "Pain");

    let res = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/recipes/suggestions?possible=true&limit=1"),
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let body = json_body(res).await;
    let suggestions = body["suggestions"].as_array().unwrap();
    assert_eq!(suggestions.len(), 1, "{body}");
    assert_eq!(suggestions[0]["name"], "Pain");
}

/// Owner and member of one group, joined through an invitation.
async fn group_with_member(
    router: &axum::Router,
    db: &PgPool,
    prefix: &str,
) -> (String, String, String) {
    let owner_cookie = register_verify_login(
        router,
        db,
        &format!("{prefix}-owner@example.test"),
        "owner-password1",
    )
    .await;
    let member_cookie = register_verify_login(
        router,
        db,
        &format!("{prefix}-member@example.test"),
        "member-password1",
    )
    .await;
    let group_id = create_group(router, &owner_cookie, "Foyer").await;
    let invite = call(
        router,
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
    let accept = call(
        router,
        Method::POST,
        &format!("/groups/invitations/{token}/accept"),
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&accept, StatusCode::OK);
    (owner_cookie, member_cookie, group_id)
}

async fn create_recipe(router: &axum::Router, cookie: &str, group_id: &str, name: &str) -> String {
    let res = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/recipes"),
        Some(cookie),
        Some(serde_json::json!({"name": name, "ingredients": []})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

async fn planned_selections(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
) -> Vec<serde_json::Value> {
    let res = call(
        router,
        Method::GET,
        &format!("/groups/{group_id}/recipes/selections"),
        Some(cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    json_body(res).await["selections"]
        .as_array()
        .unwrap()
        .clone()
}

/// Issue #469: selecting a recipe creates one `planned` selection; selecting
/// it again — from another member too — returns that same selection with
/// 200, and changes its number of people only when the request carries one.
#[sqlx::test]
async fn selecting_a_recipe_twice_keeps_one_planned_selection(db: PgPool) {
    let router = test_router(db.clone());
    let (owner_cookie, member_cookie, group_id) =
        group_with_member(&router, &db, "sel-twice").await;
    let recipe_id = create_recipe(&router, &owner_cookie, &group_id, "Lasagnes").await;
    let other_id = create_recipe(&router, &owner_cookie, &group_id, "Salade").await;
    let path = format!("/groups/{group_id}/recipes/{recipe_id}/selection");

    let first = call(
        &router,
        Method::POST,
        &path,
        Some(&owner_cookie),
        Some(serde_json::json!({"servings": 4})),
    )
    .await;
    assert_status(&first, StatusCode::CREATED);
    let first = json_body(first).await;
    assert_eq!(first["status"], "planned");
    assert_eq!(first["servings"], 4);
    assert_eq!(first["recipe_name"], "Lasagnes");

    let again = call(
        &router,
        Method::POST,
        &path,
        Some(&member_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&again, StatusCode::OK);
    let again = json_body(again).await;
    assert_eq!(again["id"], first["id"]);
    assert_eq!(
        again["servings"], 4,
        "no servings in the request: unchanged"
    );
    assert_eq!(again["created_by"], first["created_by"]);
    assert_eq!(again["updated_at"], first["updated_at"]);

    let resized = call(
        &router,
        Method::POST,
        &path,
        Some(&member_cookie),
        Some(serde_json::json!({"servings": 6})),
    )
    .await;
    assert_status(&resized, StatusCode::OK);
    let resized = json_body(resized).await;
    assert_eq!(resized["id"], first["id"]);
    assert_eq!(resized["servings"], 6);

    let unsized_new = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes/{other_id}/selection"),
        Some(&member_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&unsized_new, StatusCode::CREATED);
    assert!(json_body(unsized_new).await["servings"].is_null());

    let listed = planned_selections(&router, &member_cookie, &group_id).await;
    let listed: Vec<(&str, Option<i64>)> = listed
        .iter()
        .map(|s| (s["recipe_id"].as_str().unwrap(), s["servings"].as_i64()))
        .collect();
    assert_eq!(
        listed,
        [(recipe_id.as_str(), Some(6)), (other_id.as_str(), None)]
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM recipe_selections")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(rows, 2);
}

/// Issue #469: any member changes the number of people of a selection or
/// deselects it, whoever selected it (controller's decision of
/// 2026-10-24). Deselecting deletes the row; a recipe without a planned
/// selection answers 404 to both.
#[sqlx::test]
async fn any_member_resizes_or_deselects_a_planned_selection(db: PgPool) {
    let router = test_router(db.clone());
    let (owner_cookie, member_cookie, group_id) = group_with_member(&router, &db, "sel-any").await;
    let recipe_id = create_recipe(&router, &owner_cookie, &group_id, "Curry").await;
    let path = format!("/groups/{group_id}/recipes/{recipe_id}/selection");

    for (method, body) in [
        (Method::PATCH, Some(serde_json::json!({"servings": 2}))),
        (Method::DELETE, None),
    ] {
        let res = call(&router, method, &path, Some(&member_cookie), body).await;
        assert_status(&res, StatusCode::NOT_FOUND);
    }

    let select = call(
        &router,
        Method::POST,
        &path,
        Some(&owner_cookie),
        Some(serde_json::json!({"servings": 2})),
    )
    .await;
    assert_status(&select, StatusCode::CREATED);

    let resize = call(
        &router,
        Method::PATCH,
        &path,
        Some(&member_cookie),
        Some(serde_json::json!({"servings": 8})),
    )
    .await;
    assert_status(&resize, StatusCode::OK);
    assert_eq!(json_body(resize).await["servings"], 8);

    let deselect = call(&router, Method::DELETE, &path, Some(&member_cookie), None).await;
    assert_status(&deselect, StatusCode::NO_CONTENT);
    assert!(planned_selections(&router, &owner_cookie, &group_id)
        .await
        .is_empty());
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM recipe_selections")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(rows, 0);

    let again = call(&router, Method::DELETE, &path, Some(&member_cookie), None).await;
    assert_status(&again, StatusCode::NOT_FOUND);

    // Selectable again once deselected.
    let reselect = call(
        &router,
        Method::POST,
        &path,
        Some(&member_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&reselect, StatusCode::CREATED);
}

/// Issue #469: deselecting only deletes a `planned` selection. A `done` or
/// `abandoned` one (#473) is history: it is neither listed, nor deleted, nor
/// resized, and it does not stop the recipe from being planned again.
#[sqlx::test]
async fn only_the_planned_selection_is_listed_resized_or_deleted(db: PgPool) {
    let router = test_router(db.clone());
    let (owner_cookie, _member_cookie, group_id) =
        group_with_member(&router, &db, "sel-done").await;
    let recipe_id = create_recipe(&router, &owner_cookie, &group_id, "Tajine").await;
    let path = format!("/groups/{group_id}/recipes/{recipe_id}/selection");
    let select = call(
        &router,
        Method::POST,
        &path,
        Some(&owner_cookie),
        Some(serde_json::json!({"servings": 3})),
    )
    .await;
    assert_status(&select, StatusCode::CREATED);
    sqlx::query("UPDATE recipe_selections SET status = 'done'")
        .execute(&db)
        .await
        .unwrap();

    assert!(planned_selections(&router, &owner_cookie, &group_id)
        .await
        .is_empty());
    let resize = call(
        &router,
        Method::PATCH,
        &path,
        Some(&owner_cookie),
        Some(serde_json::json!({"servings": 5})),
    )
    .await;
    assert_status(&resize, StatusCode::NOT_FOUND);
    let deselect = call(&router, Method::DELETE, &path, Some(&owner_cookie), None).await;
    assert_status(&deselect, StatusCode::NOT_FOUND);

    let plan_again = call(
        &router,
        Method::POST,
        &path,
        Some(&owner_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&plan_again, StatusCode::CREATED);
    let statuses: Vec<(String, Option<i32>)> =
        sqlx::query_as("SELECT status, servings FROM recipe_selections ORDER BY created_at")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(
        statuses,
        [("done".to_string(), Some(3)), ("planned".to_string(), None)]
    );
}

/// Issue #469: the number of people is bounded, on selection and on resize;
/// a refused request changes nothing.
#[sqlx::test]
async fn selection_servings_out_of_bounds_are_refused(db: PgPool) {
    let router = test_router(db.clone());
    let (owner_cookie, _member_cookie, group_id) =
        group_with_member(&router, &db, "sel-bounds").await;
    let recipe_id = create_recipe(&router, &owner_cookie, &group_id, "Paella").await;
    let path = format!("/groups/{group_id}/recipes/{recipe_id}/selection");

    for servings in [0, -2, 51] {
        let res = call(
            &router,
            Method::POST,
            &path,
            Some(&owner_cookie),
            Some(serde_json::json!({"servings": servings})),
        )
        .await;
        assert_status(&res, StatusCode::BAD_REQUEST);
    }
    assert!(planned_selections(&router, &owner_cookie, &group_id)
        .await
        .is_empty());

    let select = call(
        &router,
        Method::POST,
        &path,
        Some(&owner_cookie),
        Some(serde_json::json!({"servings": 50})),
    )
    .await;
    assert_status(&select, StatusCode::CREATED);
    for servings in [0, 51] {
        for method in [Method::PATCH, Method::POST] {
            let res = call(
                &router,
                method,
                &path,
                Some(&owner_cookie),
                Some(serde_json::json!({"servings": servings})),
            )
            .await;
            assert_status(&res, StatusCode::BAD_REQUEST);
        }
    }
    let listed = planned_selections(&router, &owner_cookie, &group_id).await;
    assert_eq!(listed[0]["servings"], 50);
}

/// Issue #469: a member of another group neither sees nor changes anything.
/// Through the other group's path the membership check refuses (403, as for
/// every recipe route); through their own group's path, the other group's
/// recipe does not exist (404) and the other group's selections are not
/// listed.
#[sqlx::test]
async fn selections_are_isolated_between_groups(db: PgPool) {
    let router = test_router(db.clone());
    let (owner_cookie, _member_cookie, group_id) =
        group_with_member(&router, &db, "sel-iso-a").await;
    let outsider_cookie = register_verify_login(
        &router,
        &db,
        "sel-iso-outsider@example.test",
        "outsider-password1",
    )
    .await;
    let outsider_group = create_group(&router, &outsider_cookie, "Autre foyer").await;
    let recipe_id = create_recipe(&router, &owner_cookie, &group_id, "Couscous").await;
    let select = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes/{recipe_id}/selection"),
        Some(&owner_cookie),
        Some(serde_json::json!({"servings": 4})),
    )
    .await;
    assert_status(&select, StatusCode::CREATED);

    let foreign = format!("/groups/{group_id}/recipes/{recipe_id}/selection");
    for (method, body) in [
        (Method::POST, Some(serde_json::json!({"servings": 9}))),
        (Method::PATCH, Some(serde_json::json!({"servings": 9}))),
        (Method::DELETE, None),
    ] {
        let res = call(&router, method, &foreign, Some(&outsider_cookie), body).await;
        assert_status(&res, StatusCode::FORBIDDEN);
    }
    let list = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/recipes/selections"),
        Some(&outsider_cookie),
        None,
    )
    .await;
    assert_status(&list, StatusCode::FORBIDDEN);

    let smuggled = format!("/groups/{outsider_group}/recipes/{recipe_id}/selection");
    for (method, body) in [
        (Method::POST, Some(serde_json::json!({"servings": 9}))),
        (Method::PATCH, Some(serde_json::json!({"servings": 9}))),
        (Method::DELETE, None),
    ] {
        let res = call(&router, method, &smuggled, Some(&outsider_cookie), body).await;
        assert_status(&res, StatusCode::NOT_FOUND);
    }
    assert!(
        planned_selections(&router, &outsider_cookie, &outsider_group)
            .await
            .is_empty()
    );

    let listed = planned_selections(&router, &owner_cookie, &group_id).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["servings"], 4);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM recipe_selections")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(rows, 1);
}

/// Issue #469: deleting a recipe deletes its selections with it
/// (`ON DELETE CASCADE`).
#[sqlx::test]
async fn deleting_a_recipe_drops_its_selection(db: PgPool) {
    let router = test_router(db.clone());
    let (owner_cookie, _member_cookie, group_id) =
        group_with_member(&router, &db, "sel-cascade").await;
    let recipe_id = create_recipe(&router, &owner_cookie, &group_id, "Quiche").await;
    let select = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes/{recipe_id}/selection"),
        Some(&owner_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&select, StatusCode::CREATED);
    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/recipes/{recipe_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::NO_CONTENT);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM recipe_selections")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

/// #470: a recipe carries the number of people it is written for, from 1
/// to 50 or unset; the update changes or clears it, and leaves it alone
/// when the field is absent.
#[sqlx::test]
async fn recipe_servings_are_set_changed_and_cleared(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "recipe-servings@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;

    for servings in [0, 51] {
        let res = call(
            &router,
            Method::POST,
            &format!("/groups/{group_id}/recipes"),
            Some(&cookie),
            Some(serde_json::json!({"name": "Crepes", "servings": servings})),
        )
        .await;
        assert_status(&res, StatusCode::BAD_REQUEST);
        assert_eq!(json_body(res).await["error"], "invalid_servings");
    }

    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes"),
        Some(&cookie),
        Some(serde_json::json!({"name": "Crepes", "servings": 4})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let recipe = json_body(res).await;
    assert_eq!(recipe["servings"], 4);
    let id = recipe["id"].as_str().unwrap().to_string();
    let path = format!("/groups/{group_id}/recipes/{id}");

    let patch = |body: serde_json::Value| {
        let router = router.clone();
        let cookie = cookie.clone();
        let path = path.clone();
        async move { call(&router, Method::PATCH, &path, Some(&cookie), Some(body)).await }
    };

    let res = patch(serde_json::json!({"servings": 51})).await;
    assert_status(&res, StatusCode::BAD_REQUEST);
    assert_eq!(json_body(res).await["error"], "invalid_servings");

    let res = patch(serde_json::json!({"name": "Crêpes"})).await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(json_body(res).await["servings"], 4, "absent: left alone");

    let res = patch(serde_json::json!({"servings": 6})).await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(json_body(res).await["servings"], 6);

    let res = call(&router, Method::GET, &path, Some(&cookie), None).await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(json_body(res).await["servings"], 6);

    let res = patch(serde_json::json!({"servings": null})).await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(json_body(res).await["servings"], serde_json::Value::Null);

    let res = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/recipes"),
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(
        json_body(res).await["recipes"][0]["servings"],
        serde_json::Value::Null
    );
}

/// #539: how long a recipe takes, in minutes, from 1 to a day. Set on
/// create, left alone by a PATCH that omits it, changed, then cleared by
/// `null`; out of bounds is refused with `invalid_duration`.
#[sqlx::test]
async fn recipe_duration_is_set_changed_and_cleared(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "recipe-duration@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;

    for minutes in [0, 1441] {
        let res = call(
            &router,
            Method::POST,
            &format!("/groups/{group_id}/recipes"),
            Some(&cookie),
            Some(serde_json::json!({"name": "Crepes", "duration_minutes": minutes})),
        )
        .await;
        assert_status(&res, StatusCode::BAD_REQUEST);
        assert_eq!(json_body(res).await["error"], "invalid_duration");
    }

    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes"),
        Some(&cookie),
        Some(serde_json::json!({"name": "Crepes", "duration_minutes": 45})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let recipe = json_body(res).await;
    assert_eq!(recipe["duration_minutes"], 45);
    let id = recipe["id"].as_str().unwrap().to_string();
    let path = format!("/groups/{group_id}/recipes/{id}");

    let patch = |body: serde_json::Value| {
        let router = router.clone();
        let cookie = cookie.clone();
        let path = path.clone();
        async move { call(&router, Method::PATCH, &path, Some(&cookie), Some(body)).await }
    };

    let res = patch(serde_json::json!({"duration_minutes": 1441})).await;
    assert_status(&res, StatusCode::BAD_REQUEST);
    assert_eq!(json_body(res).await["error"], "invalid_duration");

    let res = patch(serde_json::json!({"name": "Crêpes"})).await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(
        json_body(res).await["duration_minutes"],
        45,
        "absent: left alone"
    );

    let res = patch(serde_json::json!({"duration_minutes": 1440})).await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(json_body(res).await["duration_minutes"], 1440);

    let res = call(&router, Method::GET, &path, Some(&cookie), None).await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(json_body(res).await["duration_minutes"], 1440);

    let res = patch(serde_json::json!({"duration_minutes": null})).await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(
        json_body(res).await["duration_minutes"],
        serde_json::Value::Null
    );

    let res = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/recipes"),
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(
        json_body(res).await["recipes"][0]["duration_minutes"],
        serde_json::Value::Null
    );
}
