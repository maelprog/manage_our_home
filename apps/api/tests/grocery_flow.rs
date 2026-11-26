mod common;

use axum::http::{Method, StatusCode};
use common::thresholds::{assert_edit_follows_the_threshold, Editable};
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
/// shared list, even though only the creator or a member meeting the
/// family's `grocery_edit` threshold (`admin` by default, as here; #866) may
/// edit or delete it.
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

/// #1023: the generation leaves out what the stock covers by the name
/// normalized when the ingredient or the article has no category
/// (« tomme fermiere » by « Tommes fermières »), and by category alone when
/// both have one (« pâtes », en:pastas, is not « Pâtés », en:pate).
#[sqlx::test]
async fn generate_leaves_out_what_the_stock_covers_by_normalized_name(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "grocery-normalized@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;

    for item in [
        serde_json::json!({"name": "Tommes fermières", "quantity": 1.0}),
        serde_json::json!({"name": "Pâtés", "quantity": 2.0}),
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
    let recipe = create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({
            "name": "Tartiflette",
            "ingredients": [{"name": "tomme fermiere"}, {"name": "pâtes"}]
        }),
    )
    .await;
    select(&router, &cookie, &group_id, &recipe, None).await;

    let items = generate(&router, &cookie, &group_id).await;
    let names: Vec<&str> = items.iter().map(|i| i["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["pâtes"], "{items:?}");
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
/// without a number of people keeps the quantities as written; a recipe
/// without a base counts as written for 1 (#1014, arbitrage of 2026-11-25,
/// replacing the decision of 2026-10-24 that took it as written).
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
    // No base: written for 1, cooked for 8: ×8.
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
    // No base, 250 g cooked for 4: 1000 g.
    let gratin = create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({
            "name": "Gratin",
            "ingredients": [{"name": "Pommes de terre", "quantity": 250.0, "unit": "g"}]
        }),
    )
    .await;
    select(&router, &cookie, &group_id, &pain, None).await;
    select(&router, &cookie, &group_id, &gratin, Some(4)).await;

    let items = generate(&router, &cookie, &group_id).await;
    let item = |name: &str| {
        items
            .iter()
            .find(|i| i["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing: {items:?}"))
            .clone()
    };
    assert_eq!(items.len(), 4, "one line per ingredient name: {items:?}");
    // 200 g × 6/4 + 400 g × 8/1.
    let flour = item("Farine");
    assert_eq!(flour["quantity"], 3500.0);
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
    let potatoes = item("Pommes de terre");
    assert_eq!(potatoes["quantity"], 1000.0);
    assert_eq!(potatoes["unit"], "g");
    assert_eq!(potatoes["source_recipe_id"], gratin.as_str());

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
    assert_eq!(names, ["Farine", "Lait", "Pommes de terre"], "{items:?}");
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

/// A grocery line: name, quantity, unit, source and last update.
type Line = (
    String,
    Option<f64>,
    Option<String>,
    String,
    chrono::DateTime<chrono::Utc>,
);

/// The unchecked grocery lines, by name.
async fn unchecked_lines(db: &PgPool) -> Vec<Line> {
    sqlx::query_as(
        "SELECT name, quantity, unit, source, updated_at FROM grocery_items \
         WHERE checked = false ORDER BY name",
    )
    .fetch_all(db)
    .await
    .unwrap()
}

/// What `unchecked_lines` says, without the last update.
fn counted(lines: &[Line]) -> Vec<(&str, Option<f64>, Option<&str>, &str)> {
    lines
        .iter()
        .map(|(n, q, u, s, _)| (n.as_str(), *q, u.as_deref(), s.as_str()))
        .collect()
}

/// Issue #842: a recipe planned after its ingredient is already on the list
/// raises the recipe line of the same unit to what both recipes call for;
/// a line in another unit and a manual line stay as they are, and
/// generating again changes nothing.
#[sqlx::test]
async fn generate_raises_a_recipe_line_to_what_the_planned_recipes_need(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "grocery-raise@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items"),
        Some(&cookie),
        Some(serde_json::json!({"name": "Sucre", "quantity": 100.0, "unit": "g"})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let a = create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({
            "name": "Crepes",
            "ingredients": [
                {"name": "Farine", "quantity": 200.0, "unit": "g"},
                {"name": "Beurre", "quantity": 1.0, "unit": "kg"}
            ]
        }),
    )
    .await;
    select(&router, &cookie, &group_id, &a, None).await;
    assert_eq!(generate(&router, &cookie, &group_id).await.len(), 2);

    let b = create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({
            "name": "Gateau",
            "ingredients": [
                {"name": "farine", "quantity": 300.0, "unit": "g"},
                {"name": "Beurre", "quantity": 300.0, "unit": "g"},
                {"name": "Sucre", "quantity": 50.0, "unit": "g"}
            ]
        }),
    )
    .await;
    select(&router, &cookie, &group_id, &b, None).await;
    assert!(generate(&router, &cookie, &group_id).await.is_empty());
    let lines = unchecked_lines(&db).await;
    assert_eq!(
        counted(&lines),
        vec![
            ("Beurre", Some(1.0), Some("kg"), "recipe"),
            ("Farine", Some(500.0), Some("g"), "recipe"),
            ("Sucre", Some(100.0), Some("g"), "manual"),
        ]
    );

    // Generated again: no line changes, not even its last update.
    assert!(generate(&router, &cookie, &group_id).await.is_empty());
    assert_eq!(unchecked_lines(&db).await, lines);
}

/// Issue #842: adding a planned recipe's missing ingredients raises the
/// line another planned recipe put on the list by what this one lacks, as
/// the generation does; replaying changes nothing.
#[sqlx::test]
async fn adding_the_missing_ingredients_raises_a_recipe_line(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "grocery-missing-raise@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let a = create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({
            "name": "Crepes",
            "ingredients": [{"name": "Farine", "quantity": 200.0, "unit": "g"}]
        }),
    )
    .await;
    select(&router, &cookie, &group_id, &a, None).await;
    assert_eq!(generate(&router, &cookie, &group_id).await.len(), 1);
    let b = create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({
            "name": "Gateau",
            "ingredients": [
                {"name": "Farine", "quantity": 300.0, "unit": "g"},
                {"name": "Oeufs", "quantity": 2.0}
            ]
        }),
    )
    .await;
    select(&router, &cookie, &group_id, &b, None).await;

    let path = format!("/groups/{group_id}/recipes/{b}/selection/missing");
    let res = call(&router, Method::POST, &path, Some(&cookie), None).await;
    assert_status(&res, StatusCode::CREATED);
    let created = json_body(res).await["items"].as_array().unwrap().clone();
    assert_eq!(created.len(), 1, "{created:?}");
    assert_eq!(created[0]["name"], "Oeufs");
    let lines = unchecked_lines(&db).await;
    assert_eq!(
        counted(&lines),
        vec![
            ("Farine", Some(500.0), Some("g"), "recipe"),
            ("Oeufs", Some(2.0), Some("unité"), "recipe"),
        ]
    );

    // Replayed, for either recipe, or generated: nothing changes.
    for path in [
        path.clone(),
        format!("/groups/{group_id}/recipes/{a}/selection/missing"),
    ] {
        let res = call(&router, Method::POST, &path, Some(&cookie), None).await;
        assert_status(&res, StatusCode::CREATED);
        assert!(json_body(res).await["items"].as_array().unwrap().is_empty());
    }
    assert!(generate(&router, &cookie, &group_id).await.is_empty());
    assert_eq!(unchecked_lines(&db).await, lines);
}

/// Issue #842: what another planned recipe lacks of an ingredient only
/// weighs against a recipe line already on the list. A line created by
/// adding one recipe's missing ingredients carries that recipe's quantity.
#[sqlx::test]
async fn adding_the_missing_ingredients_creates_the_recipe_own_quantity(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "grocery-missing-own@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let a = create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({
            "name": "Crepes",
            "ingredients": [{"name": "Farine", "quantity": 200.0, "unit": "g"}]
        }),
    )
    .await;
    let b = create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({
            "name": "Gateau",
            "ingredients": [{"name": "Farine", "quantity": 300.0, "unit": "g"}]
        }),
    )
    .await;
    select(&router, &cookie, &group_id, &a, None).await;
    select(&router, &cookie, &group_id, &b, None).await;

    let path = format!("/groups/{group_id}/recipes/{a}/selection/missing");
    let res = call(&router, Method::POST, &path, Some(&cookie), None).await;
    assert_status(&res, StatusCode::CREATED);
    let created = json_body(res).await["items"].as_array().unwrap().clone();
    assert_eq!(created.len(), 1, "{created:?}");
    assert_eq!(created[0]["quantity"], 200.0);
    assert_eq!(
        counted(&unchecked_lines(&db).await),
        vec![("Farine", Some(200.0), Some("g"), "recipe")]
    );

    // B's missing ingredients then raise that line by B's share.
    let path = format!("/groups/{group_id}/recipes/{b}/selection/missing");
    let res = call(&router, Method::POST, &path, Some(&cookie), None).await;
    assert_status(&res, StatusCode::CREATED);
    assert!(json_body(res).await["items"].as_array().unwrap().is_empty());
    assert_eq!(
        counted(&unchecked_lines(&db).await),
        vec![("Farine", Some(500.0), Some("g"), "recipe")]
    );
}

/// Issue #902: two names the taxonomy maps to one ingredient make one line
/// when their units are the same, two otherwise; adding a recipe's missing
/// ingredients counts that line by its tag too, and generating again
/// changes nothing.
#[sqlx::test]
async fn generate_merges_the_names_of_one_ingredient(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "grocery-canonical@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let a = create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({
            "name": "Pates",
            "ingredients": [
                {"name": "parmesan", "quantity": 100.0, "unit": "g"},
                {"name": "crème fraîche", "quantity": 20.0, "unit": "cl"}
            ]
        }),
    )
    .await;
    let b = create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({
            "name": "Gratin",
            "ingredients": [
                {"name": "Parmigiano Reggiano", "quantity": 50.0, "unit": "g"},
                {"name": "Crème fraiche", "quantity": 1.0, "unit": "pot"}
            ]
        }),
    )
    .await;
    select(&router, &cookie, &group_id, &a, None).await;
    select(&router, &cookie, &group_id, &b, None).await;

    assert_eq!(generate(&router, &cookie, &group_id).await.len(), 3);
    let lines = unchecked_lines(&db).await;
    // Sorted here, byte-wise: the database's collation orders the accents.
    let mut sorted = counted(&lines);
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    assert_eq!(
        sorted,
        vec![
            ("Crème fraiche", Some(1.0), Some("pot"), "recipe"),
            ("crème fraîche", Some(20.0), Some("cl"), "recipe"),
            ("parmesan", Some(150.0), Some("g"), "recipe"),
        ]
    );

    // Generated again, or B's missing ingredients added: nothing changes,
    // not even a line's last update.
    assert!(generate(&router, &cookie, &group_id).await.is_empty());
    let path = format!("/groups/{group_id}/recipes/{b}/selection/missing");
    let res = call(&router, Method::POST, &path, Some(&cookie), None).await;
    assert_status(&res, StatusCode::CREATED);
    assert!(json_body(res).await["items"].as_array().unwrap().is_empty());
    assert_eq!(unchecked_lines(&db).await, lines);
}

/// Issue #902: whether two names of one ingredient make one line depends on
/// the lines met before them; adding each recipe's missing ingredients
/// after a generation merges them as it did, so it changes nothing, played
/// once or twice.
#[sqlx::test]
async fn adding_the_missing_after_a_generation_changes_nothing(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "grocery-canonical-replay@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let mut recipes = Vec::new();
    for (name, ingredient) in [
        (
            "Pates",
            serde_json::json!({"name": "parmesan", "quantity": 3.0, "unit": "kg"}),
        ),
        (
            "Gratin",
            serde_json::json!({"name": "parmesan", "quantity": 4.0}),
        ),
        (
            "Salade",
            serde_json::json!({"name": "Parmesan râpé", "quantity": 4.0}),
        ),
    ] {
        let id = create_recipe(
            &router,
            &cookie,
            &group_id,
            serde_json::json!({"name": name, "ingredients": [ingredient]}),
        )
        .await;
        select(&router, &cookie, &group_id, &id, None).await;
        recipes.push(id);
    }

    assert_eq!(generate(&router, &cookie, &group_id).await.len(), 2);
    let lines = unchecked_lines(&db).await;
    let mut sorted = counted(&lines);
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    assert_eq!(
        sorted,
        vec![
            ("Parmesan râpé", Some(4.0), Some("unité"), "recipe"),
            ("parmesan", Some(3.0), Some("kg"), "recipe"),
        ]
    );
    for recipe in recipes.iter().chain(recipes.last()) {
        let path = format!("/groups/{group_id}/recipes/{recipe}/selection/missing");
        let res = call(&router, Method::POST, &path, Some(&cookie), None).await;
        assert_status(&res, StatusCode::CREATED);
        assert!(json_body(res).await["items"].as_array().unwrap().is_empty());
    }
    assert_eq!(unchecked_lines(&db).await, lines);
}

/// Issue #902: a manual line under another name of the ingredient is the
/// planned recipe's need already on the list: no line is added.
#[sqlx::test]
async fn generate_sees_a_manual_line_of_another_name(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "grocery-canonical-manual@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items"),
        Some(&cookie),
        Some(serde_json::json!({"name": "parmigiano reggiano"})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let a = create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({
            "name": "Pates",
            "ingredients": [{"name": "Parmesan", "quantity": 100.0, "unit": "g"}]
        }),
    )
    .await;
    select(&router, &cookie, &group_id, &a, None).await;

    assert!(generate(&router, &cookie, &group_id).await.is_empty());
    assert_eq!(
        counted(&unchecked_lines(&db).await),
        vec![("parmigiano reggiano", None, None, "manual")]
    );
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

/// #824: the lines generated from a recipe come in packs of a scanned
/// article of their name (« Coquillettes », 500 g) or of their category
/// (« Lait » and « Lait demi-écrémé Lactel », 1 L); a line no scanned
/// article matches (« Farine ») is unchanged. The stored quantity stays
/// exact, and only the list read carries the packs.
#[sqlx::test]
async fn the_list_counts_lines_in_packs_of_scanned_articles(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "grocery-packs@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;

    // What a scan would have cached (#402), written directly: the test
    // must not reach Open Food Facts.
    for (code, name, quantity, tags) in [
        ("3038350208002", "Coquillettes", "500 g", vec!["en:pastas"]),
        (
            "4006381333931",
            "Lait demi-écrémé Lactel",
            "1 L",
            vec!["en:dairies", "en:milks", "en:semi-skimmed-milks"],
        ),
    ] {
        sqlx::query(
            "INSERT INTO off_products (code, name, quantity, categories_tags) VALUES ($1, $2, $3, $4)",
        )
        .bind(code)
        .bind(name)
        .bind(quantity)
        .bind(tags)
        .execute(&db)
        .await
        .unwrap();
        // Used up: an article at 0 does not keep the line off the list.
        let res = call(
            &router,
            Method::POST,
            &format!("/groups/{group_id}/stock-items"),
            Some(&cookie),
            Some(serde_json::json!({"name": name, "quantity": 0.0, "unit": "g", "barcode": code})),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
    }
    let recipe = create_recipe(
        &router,
        &cookie,
        &group_id,
        serde_json::json!({
            "name": "Gratin",
            "ingredients": [
                {"name": "Coquillettes", "quantity": 380.0, "unit": "g", "is_optional": false},
                {"name": "Lait", "quantity": 40.0, "unit": "cl", "is_optional": false},
                {"name": "Farine", "quantity": 300.0, "unit": "g", "is_optional": false}
            ]
        }),
    )
    .await;
    select(&router, &cookie, &group_id, &recipe, None).await;
    let created = generate(&router, &cookie, &group_id).await;
    assert_eq!(created.len(), 3, "{created:?}");
    assert!(
        created.iter().all(|i| i.get("pack").is_none()),
        "{created:?}"
    );

    let res = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/grocery-items"),
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let items = json_body(res).await["items"].as_array().unwrap().clone();
    let item = |name: &str| items.iter().find(|i| i["name"] == name).unwrap().clone();
    let pasta = item("Coquillettes");
    assert_eq!(pasta["quantity"], 380.0);
    assert_eq!(
        pasta["pack"],
        serde_json::json!({"count": 1, "size": {"quantity": 500.0, "unit": "g"}})
    );
    let milk = item("Lait");
    assert_eq!(milk["quantity"], 40.0);
    assert_eq!(milk["unit"], "cl");
    assert_eq!(
        milk["pack"],
        serde_json::json!({"count": 1, "size": {"quantity": 1.0, "unit": "L"}})
    );
    let flour = item("Farine");
    assert_eq!(flour["quantity"], 300.0);
    assert!(flour.get("pack").is_none(), "{flour:?}");

    // The rounding is shown, never stored.
    let stored: Vec<(String, Option<f64>)> =
        sqlx::query_as("SELECT name, quantity FROM grocery_items ORDER BY name")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(
        stored,
        vec![
            ("Coquillettes".to_string(), Some(380.0)),
            ("Farine".to_string(), Some(300.0)),
            ("Lait".to_string(), Some(40.0)),
        ]
    );
}

/// AC #866: the edit and the delete of a grocery item follow the family's `grocery_edit` threshold, the creator
/// keeping the hand on what they wrote (`common::thresholds`).
#[sqlx::test]
async fn editing_a_grocery_item_follows_the_family_s_threshold(db: PgPool) {
    let router = test_router(db.clone());
    assert_edit_follows_the_threshold(
        &router,
        &db,
        Editable {
            feature: "grocery_edit",
            collection: "grocery-items",
            create: serde_json::json!({"name": "Farine", "quantity": 1.0, "unit": "kg"}),
            edit: serde_json::json!({"name": "Sucre"}),
        },
    )
    .await;
}
