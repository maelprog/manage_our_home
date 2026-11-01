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

async fn create_recipe(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    body: serde_json::Value,
) -> String {
    let res = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/recipes"),
        Some(cookie),
        Some(body),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

/// Plans a recipe for the next shopping (#469), for `servings` people.
async fn select(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    recipe_id: &str,
    servings: Option<i32>,
) {
    let body = match servings {
        Some(n) => serde_json::json!({ "servings": n }),
        None => serde_json::json!({}),
    };
    let res = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/recipes/{recipe_id}/selection"),
        Some(cookie),
        Some(body),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
}

async fn generate(router: &axum::Router, cookie: &str, group_id: &str) -> Vec<serde_json::Value> {
    let res = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items/generate"),
        Some(cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["items"].as_array().unwrap().clone()
}

/// #458: a quantity entered without a unit gets "unité", on create and on
/// update; without a quantity the unit stays null; a typed unit is kept.
#[sqlx::test]
async fn a_quantity_without_unit_gets_the_default_unit(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "grocery-unit@example.test", "owner-password1").await;
    let group_id = create_group(&router, &cookie, "Foyer").await;

    let create = |body: serde_json::Value| {
        let router = router.clone();
        let cookie = cookie.clone();
        let group_id = group_id.clone();
        async move {
            let res = call(
                &router,
                Method::POST,
                &format!("/groups/{group_id}/grocery-items"),
                Some(&cookie),
                Some(body),
            )
            .await;
            assert_status(&res, StatusCode::CREATED);
            json_body(res).await
        }
    };

    let eggs = create(serde_json::json!({"name": "Oeufs", "quantity": 6.0, "unit": ""})).await;
    assert_eq!(eggs["unit"], "unité");
    let lemons = create(serde_json::json!({"name": "Citrons", "quantity": 2.0})).await;
    assert_eq!(lemons["unit"], "unité");
    let flour =
        create(serde_json::json!({"name": "Farine", "quantity": 1.0, "unit": " kg "})).await;
    assert_eq!(flour["unit"], "kg");
    let salt = create(serde_json::json!({"name": "Sel", "unit": "  "})).await;
    assert_eq!(salt["unit"], serde_json::Value::Null);
    assert_eq!(salt["quantity"], serde_json::Value::Null);

    let patch = |id: String, body: serde_json::Value| {
        let router = router.clone();
        let cookie = cookie.clone();
        let group_id = group_id.clone();
        async move {
            let res = call(
                &router,
                Method::PATCH,
                &format!("/groups/{group_id}/grocery-items/{id}"),
                Some(&cookie),
                Some(body),
            )
            .await;
            assert_status(&res, StatusCode::OK);
            json_body(res).await
        }
    };

    // A quantity added to an item without unit: the unit follows.
    let salt_id = salt["id"].as_str().unwrap().to_string();
    let salt = patch(salt_id.clone(), serde_json::json!({"quantity": 1.0})).await;
    assert_eq!(salt["unit"], "unité");
    // Clearing the unit while the quantity stays: back to the default.
    let flour_id = flour["id"].as_str().unwrap().to_string();
    let flour = patch(flour_id, serde_json::json!({"unit": null})).await;
    assert_eq!(flour["unit"], "unité");
    // Clearing both: the unit stays null.
    let salt = patch(salt_id, serde_json::json!({"quantity": null, "unit": null})).await;
    assert_eq!(salt["unit"], serde_json::Value::Null);
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
    // in stock, so it should be pulled in as a missing ingredient), planned
    // for the next shopping (#470).
    let recipe = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes"),
        Some(&owner_cookie),
        Some(serde_json::json!({
            "name": "Crepes",
            "ingredients": [
                {"name": "Farine", "is_optional": false},
                {"name": "Oeufs", "quantity": 3.0, "is_optional": false}
            ]
        })),
    )
    .await;
    let recipe_id = json_body(recipe).await["id"].as_str().unwrap().to_string();
    select(&router, &owner_cookie, &group_id, &recipe_id, None).await;

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
    // #458: a recipe quantity without a unit gets the default one, as on a
    // manual add.
    let eggs = items.iter().find(|i| i["name"] == "Oeufs").unwrap();
    assert_eq!(eggs["quantity"], 3.0);
    assert_eq!(eggs["unit"], "unité");

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

/// #470: with three recipes of which one is planned, the generation adds
/// that recipe's ingredients only, each naming it in `source_recipe_id`.
#[sqlx::test]
async fn generate_takes_only_the_planned_recipes(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "grocery-planned@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;

    let crepes = create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({
            "name": "Crepes",
            "ingredients": [
                {"name": "Farine", "quantity": 250.0, "unit": "g"},
                {"name": "Oeufs", "quantity": 4.0},
                {"name": "Rhum", "quantity": 1.0, "unit": "cl", "is_optional": true}
            ]
        }),
    )
    .await;
    create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({"name": "Soupe", "ingredients": [{"name": "Poireaux"}]}),
    )
    .await;
    create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({"name": "Gratin", "ingredients": [{"name": "Pommes de terre"}]}),
    )
    .await;
    select(&router, &cookie, &group_id, &crepes, None).await;

    let items = generate(&router, &cookie, &group_id).await;
    let mut names: Vec<&str> = items.iter().map(|i| i["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(names, ["Farine", "Oeufs"], "{items:?}");
    for item in &items {
        assert_eq!(item["source"], "recipe");
        assert_eq!(item["source_recipe_id"], crepes.as_str(), "{item}");
    }
}

/// #470: without any planned recipe, no recipe ingredient is added; the
/// stock items under their threshold still are.
#[sqlx::test]
async fn generate_without_selection_adds_low_stock_only(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "grocery-unplanned@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;

    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&cookie),
        Some(serde_json::json!({"name": "Lait", "quantity": 0.0, "reorder_threshold": 1.0})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({"name": "Crepes", "ingredients": [{"name": "Oeufs"}]}),
    )
    .await;

    let items = generate(&router, &cookie, &group_id).await;
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["name"], "Lait");
    assert_eq!(items[0]["source"], "low_stock");
    assert_eq!(items[0]["source_recipe_id"], serde_json::Value::Null);
}

/// #470, decisions of 2026-10-24: each quantity is scaled to the
/// selection's number of people against the recipe's base, and an
/// ingredient two planned recipes share makes one line, summed when the
/// units match and attributed to the recipe planned first. A selection
/// without a number of people, or a recipe without a base, keeps the
/// quantities as written.
#[sqlx::test]
async fn generate_scales_and_sums_the_planned_quantities(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "grocery-scale@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;

    // Written for 4, cooked for 6: ×1.5.
    let crepes = create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({
            "name": "Crepes",
            "servings": 4,
            "ingredients": [
                {"name": "Farine", "quantity": 200.0, "unit": "g"},
                {"name": "Lait", "quantity": 0.5, "unit": "L"}
            ]
        }),
    )
    .await;
    // No base: taken as written whatever the selection says.
    let gateau = create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({
            "name": "Gateau",
            "ingredients": [
                {"name": "farine", "quantity": 400.0, "unit": "g"},
                {"name": "Lait", "quantity": 20.0, "unit": "cl"}
            ]
        }),
    )
    .await;
    // A base but no number of people on the selection: as written too.
    let pain = create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({
            "name": "Pain",
            "servings": 2,
            "ingredients": [{"name": "Levure", "quantity": 10.0, "unit": "g"}]
        }),
    )
    .await;
    select(&router, &cookie, &group_id, &crepes, Some(6)).await;
    select(&router, &cookie, &group_id, &gateau, Some(8)).await;
    select(&router, &cookie, &group_id, &pain, None).await;

    let items = generate(&router, &cookie, &group_id).await;
    let item = |name: &str| {
        items
            .iter()
            .find(|i| i["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing: {items:?}"))
            .clone()
    };
    assert_eq!(items.len(), 3, "one line per ingredient name: {items:?}");
    // 200 g × 6/4 + 400 g.
    let flour = item("Farine");
    assert_eq!(flour["quantity"], 700.0);
    assert_eq!(flour["unit"], "g");
    assert_eq!(flour["source_recipe_id"], crepes.as_str());
    // 0.5 L × 6/4 = 0.75 L; the 20 cl are in another unit, not added.
    let milk = item("Lait");
    assert_eq!(milk["quantity"], 0.75);
    assert_eq!(milk["unit"], "L");
    assert_eq!(milk["source_recipe_id"], crepes.as_str());
    let yeast = item("Levure");
    assert_eq!(yeast["quantity"], 10.0);
    assert_eq!(yeast["source_recipe_id"], pain.as_str());

    // A deselected recipe stops counting.
    let res = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/recipes/{pain}/selection"),
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    for item in &items {
        let id = item["id"].as_str().unwrap();
        let res = call(
            &router,
            Method::DELETE,
            &format!("/groups/{group_id}/grocery-items/{id}"),
            Some(&cookie),
            None,
        )
        .await;
        assert_status(&res, StatusCode::NO_CONTENT);
    }
    let items = generate(&router, &cookie, &group_id).await;
    let mut names: Vec<&str> = items.iter().map(|i| i["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(names, ["Farine", "Lait"], "{items:?}");
}

/// Issue #559: adding a planned recipe's missing ingredients to the list
/// creates one item per element of `missing`, with the quantity lacking and
/// the recipe in `source_recipe_id`; replaying adds nothing while the items
/// stay unchecked.
#[sqlx::test]
async fn adding_the_missing_ingredients_is_idempotent(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "grocery-missing@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Oeufs", "quantity": 1.0})),
    )
    .await;
    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Farine", "quantity": 500.0, "unit": "g"})),
    )
    .await;
    let recipe_id = create_recipe(
        &router,
        &owner_cookie,
        &group_id,
        serde_json::json!({
            "name": "Crêpes",
            "servings": 2,
            "ingredients": [
                {"name": "Oeufs", "quantity": 3.0, "is_optional": false},
                {"name": "Farine", "quantity": 200.0, "unit": "g", "is_optional": false},
                {"name": "Lait", "quantity": 0.5, "unit": "L", "is_optional": false},
                {"name": "Rhum", "quantity": 1.0, "unit": "cl", "is_optional": true}
            ]
        }),
    )
    .await;
    // Cooked for 4: 6 eggs, 400 g of flour, 1 L of milk.
    select(&router, &owner_cookie, &group_id, &recipe_id, Some(4)).await;

    let path = format!("/groups/{group_id}/recipes/{recipe_id}/selection/missing");
    let res = call(&router, Method::POST, &path, Some(&owner_cookie), None).await;
    assert_status(&res, StatusCode::CREATED);
    let created = json_body(res).await["items"].as_array().unwrap().clone();
    let summary: Vec<_> = created
        .iter()
        .map(|i| {
            (
                i["name"].as_str().unwrap().to_string(),
                i["quantity"].as_f64().unwrap(),
                i["unit"].as_str().unwrap().to_string(),
                i["source"].as_str().unwrap().to_string(),
                i["source_recipe_id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                "Lait".into(),
                1.0,
                "L".into(),
                "recipe".into(),
                recipe_id.clone()
            ),
            (
                "Oeufs".into(),
                5.0,
                "unité".into(),
                "recipe".into(),
                recipe_id.clone()
            ),
        ]
    );

    // Replayed: one item per unchecked name.
    let res = call(&router, Method::POST, &path, Some(&owner_cookie), None).await;
    assert_status(&res, StatusCode::CREATED);
    assert!(json_body(res).await["items"].as_array().unwrap().is_empty());
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM grocery_items")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(rows, 2);

    // Once an item is checked, its name can be added again.
    let milk = created[0]["id"].as_str().unwrap();
    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items/{milk}/check"),
        Some(&owner_cookie),
        Some(serde_json::json!({"checked": true})),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let res = call(&router, Method::POST, &path, Some(&owner_cookie), None).await;
    assert_status(&res, StatusCode::CREATED);
    let again = json_body(res).await["items"].as_array().unwrap().clone();
    assert_eq!(again.len(), 1);
    assert_eq!(again[0]["name"], "Lait");
}

/// Issue #559: adding the missing ingredients needs a `planned` selection of
/// the recipe in the group named by the path: another group's selection, a
/// recipe not planned, or a selection already done is 404, and nothing is
/// written; a non-member is refused.
#[sqlx::test]
async fn adding_the_missing_ingredients_of_another_group_is_404(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "grocery-missing-a@example.test",
        "owner-password1",
    )
    .await;
    let outsider_cookie = register_verify_login(
        &router,
        &db,
        "grocery-missing-b@example.test",
        "outsider-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let outsider_group = create_group(&router, &outsider_cookie, "Autre foyer").await;
    let recipe_id = create_recipe(
        &router,
        &owner_cookie,
        &group_id,
        serde_json::json!({
            "name": "Omelette",
            "ingredients": [{"name": "Oeufs", "quantity": 3.0, "is_optional": false}]
        }),
    )
    .await;
    let not_planned = create_recipe(
        &router,
        &owner_cookie,
        &group_id,
        serde_json::json!({
            "name": "Quiche",
            "ingredients": [{"name": "Oeufs", "quantity": 3.0, "is_optional": false}]
        }),
    )
    .await;
    select(&router, &owner_cookie, &group_id, &recipe_id, None).await;

    let smuggled = format!("/groups/{outsider_group}/recipes/{recipe_id}/selection/missing");
    let res = call(
        &router,
        Method::POST,
        &smuggled,
        Some(&outsider_cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);
    let foreign = format!("/groups/{group_id}/recipes/{recipe_id}/selection/missing");
    let res = call(
        &router,
        Method::POST,
        &foreign,
        Some(&outsider_cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::FORBIDDEN);
    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/recipes/{not_planned}/selection/missing"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);
    sqlx::query("UPDATE recipe_selections SET status = 'done'")
        .execute(&db)
        .await
        .unwrap();
    let res = call(&router, Method::POST, &foreign, Some(&owner_cookie), None).await;
    assert_status(&res, StatusCode::NOT_FOUND);

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM grocery_items")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}
