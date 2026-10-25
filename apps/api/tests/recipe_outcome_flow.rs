//! Marking a planned recipe done or abandoned (#473): `GET`/`POST
//! /groups/:id/recipes/:recipe_id/selection/done` and `…/abandoned`.

mod common;

use axum::http::{Method, StatusCode};
use common::{assert_status, call, json_body, set_cookie, test_router};
use serde_json::{json, Value};
use sqlx::PgPool;

type Res = axum::http::Response<axum::body::Body>;

async fn register_verify_login(router: &axum::Router, db: &PgPool, email: &str) -> String {
    let password = "owner-password1";
    call(
        router,
        Method::POST,
        "/auth/register",
        None,
        Some(json!({"email": email, "password": password, "display_name": email, "declares_minimum_age": true, "accepts_terms": true})),
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
        Some(json!({"email": email, "password": password})),
    )
    .await;
    set_cookie(&login).unwrap()
}

async fn create_group(router: &axum::Router, cookie: &str) -> String {
    let res = call(
        router,
        Method::POST,
        "/groups",
        Some(cookie),
        Some(json!({"name": "Foyer"})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

async fn join(router: &axum::Router, owner: &str, member: &str, group_id: &str) {
    let invite = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
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

async fn post(router: &axum::Router, cookie: &str, path: &str, body: Value) -> Res {
    call(router, Method::POST, path, Some(cookie), Some(body)).await
}

async fn get(router: &axum::Router, cookie: &str, path: &str) -> Res {
    call(router, Method::GET, path, Some(cookie), None).await
}

async fn created_id(res: Res) -> String {
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

/// A recipe, selected (`planned`).
async fn planned_recipe(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    recipe: Value,
    selection: Value,
) -> String {
    let id = created_id(
        post(
            router,
            cookie,
            &format!("/groups/{group_id}/recipes"),
            recipe,
        )
        .await,
    )
    .await;
    let sel = post(
        router,
        cookie,
        &format!("/groups/{group_id}/recipes/{id}/selection"),
        selection,
    )
    .await;
    assert_status(&sel, StatusCode::CREATED);
    id
}

async fn stock(router: &axum::Router, cookie: &str, group_id: &str, body: Value) -> String {
    created_id(
        post(
            router,
            cookie,
            &format!("/groups/{group_id}/stock-items"),
            body,
        )
        .await,
    )
    .await
}

async fn stock_quantity(router: &axum::Router, cookie: &str, group_id: &str, id: &str) -> f64 {
    let res = get(
        router,
        cookie,
        &format!("/groups/{group_id}/stock-items/{id}"),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    json_body(res).await["quantity"].as_f64().unwrap()
}

fn done_path(group_id: &str, recipe_id: &str) -> String {
    format!("/groups/{group_id}/recipes/{recipe_id}/selection/done")
}

fn abandoned_path(group_id: &str, recipe_id: &str) -> String {
    format!("/groups/{group_id}/recipes/{recipe_id}/selection/abandoned")
}

async fn planned_count(router: &axum::Router, cookie: &str, group_id: &str) -> usize {
    let res = get(
        router,
        cookie,
        &format!("/groups/{group_id}/recipes/selections"),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    json_body(res).await["selections"].as_array().unwrap().len()
}

async fn meals(router: &axum::Router, cookie: &str, group_id: &str) -> usize {
    let res = get(
        router,
        cookie,
        &format!("/groups/{group_id}/recipes/meal-history"),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    json_body(res).await["entries"].as_array().unwrap().len()
}

async fn grocery_names(router: &axum::Router, cookie: &str, group_id: &str) -> Vec<String> {
    let res = get(router, cookie, &format!("/groups/{group_id}/grocery-items")).await;
    assert_status(&res, StatusCode::OK);
    let mut names: Vec<String> = json_body(res).await["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    names
}

/// The preview, by ingredient name.
async fn done_preview(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    recipe_id: &str,
) -> Vec<Value> {
    let res = get(router, cookie, &done_path(group_id, recipe_id)).await;
    assert_status(&res, StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["selection"]["status"], "planned");
    body["ingredients"].as_array().unwrap().clone()
}

fn by_name<'a>(lines: &'a [Value], name: &str) -> &'a Value {
    lines
        .iter()
        .find(|l| l["name"] == name)
        .unwrap_or_else(|| panic!("{name} in {lines:?}"))
}

/// A recipe for 2 cooked for 4, and its stock: Farine 500 g, Beurre 250 g,
/// Sucre 1 kg (another unit), no Sel.
async fn crepes(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
) -> (String, String, String, String) {
    let farine = stock(
        router,
        cookie,
        group_id,
        json!({"name": "Farine", "quantity": 500.0, "unit": "g"}),
    )
    .await;
    let beurre = stock(
        router,
        cookie,
        group_id,
        json!({"name": "beurre", "quantity": 250.0, "unit": "g"}),
    )
    .await;
    let sucre = stock(
        router,
        cookie,
        group_id,
        json!({"name": "Sucre", "quantity": 1.0, "unit": "kg"}),
    )
    .await;
    let recipe = planned_recipe(
        router,
        cookie,
        group_id,
        json!({"name": "Crepes", "servings": 2, "ingredients": [
            {"name": "Farine", "quantity": 200.0, "unit": "g"},
            {"name": "Beurre", "quantity": 50.0, "unit": "g"},
            {"name": "Sucre", "quantity": 30.0, "unit": "g"},
            {"name": "Sel", "quantity": 1.0, "unit": "pincée"}
        ]}),
        json!({"servings": 4}),
    )
    .await;
    (recipe, farine, beurre, sucre)
}

/// AC: done with a correction at 0 on one ingredient leaves that stock
/// unchanged and decrements the others; the meal is logged and the
/// selection closed. The preview scales the recipe to the selection and
/// warns where it proposes 0.
#[sqlx::test]
async fn done_with_a_zero_correction_leaves_that_stock_untouched(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "done-zero@example.test").await;
    let group_id = create_group(&router, &cookie).await;
    let (recipe, farine, beurre, sucre) = crepes(&router, &cookie, &group_id).await;

    let lines = done_preview(&router, &cookie, &group_id, &recipe).await;
    assert_eq!(lines.len(), 4);
    let f = by_name(&lines, "Farine");
    assert_eq!(f["quantity"], 400.0, "200 g for 2, cooked for 4");
    assert_eq!(f["lots"][0]["stock_item"]["id"], farine.as_str());
    assert_eq!(f["lots"][0]["quantity"], 400.0);
    assert_eq!(f["proposed_quantity"], 400.0);
    assert_eq!(f["warning"], Value::Null);
    let b = by_name(&lines, "Beurre");
    assert_eq!(b["lots"][0]["stock_item"]["id"], beurre.as_str());
    assert_eq!(b["proposed_quantity"], 100.0);
    let s = by_name(&lines, "Sucre");
    assert_eq!(s["lots"][0]["stock_item"]["id"], sucre.as_str());
    assert_eq!(s["proposed_quantity"], 0.0);
    assert_eq!(s["warning"], "unit_mismatch");
    let sel = by_name(&lines, "Sel");
    assert_eq!(sel["lots"], json!([]));
    assert_eq!(sel["warning"], "no_stock");

    let res = post(
        &router,
        &cookie,
        &done_path(&group_id, &recipe),
        json!({"removals": [
            {"stock_item_id": farine, "quantity": 400.0},
            {"stock_item_id": beurre, "quantity": 0.0},
            {"stock_item_id": sucre, "quantity": 0.5, "exclude": true}
        ]}),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["selection"]["status"], "done");
    assert_eq!(body["meal"]["recipe_id"], recipe.as_str());
    assert_eq!(body["stock_items"].as_array().unwrap().len(), 1);

    assert_eq!(
        stock_quantity(&router, &cookie, &group_id, &farine).await,
        100.0
    );
    assert_eq!(
        stock_quantity(&router, &cookie, &group_id, &beurre).await,
        250.0
    );
    assert_eq!(
        stock_quantity(&router, &cookie, &group_id, &sucre).await,
        1.0
    );
    assert_eq!(meals(&router, &cookie, &group_id).await, 1);
    assert_eq!(planned_count(&router, &cookie, &group_id).await, 0);
    // Closed: neither action applies any more.
    let again = post(
        &router,
        &cookie,
        &done_path(&group_id, &recipe),
        json!({"removals": []}),
    )
    .await;
    assert_status(&again, StatusCode::NOT_FOUND);
}

/// AC: a quantity above the stock is refused (409, nothing applied), never
/// capped; above the recipe's but within the stock is accepted (precision
/// of 2026-10-21). The preview caps at the stock.
#[sqlx::test]
async fn more_than_the_stock_is_refused(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "done-over@example.test").await;
    let group_id = create_group(&router, &cookie).await;
    let (recipe, farine, beurre, _) = crepes(&router, &cookie, &group_id).await;
    let small = stock(
        &router,
        &cookie,
        &group_id,
        json!({"name": "Sel", "quantity": 0.5, "unit": "pincée"}),
    )
    .await;

    let lines = done_preview(&router, &cookie, &group_id, &recipe).await;
    let sel = by_name(&lines, "Sel");
    assert_eq!(sel["lots"][0]["stock_item"]["id"], small.as_str());
    assert_eq!(sel["proposed_quantity"], 0.5, "2 pincées capped at 0.5");
    assert_eq!(sel["warning"], "capped");

    let res = post(
        &router,
        &cookie,
        &done_path(&group_id, &recipe),
        json!({"removals": [
            {"stock_item_id": beurre, "quantity": 100.0},
            {"stock_item_id": farine, "quantity": 500.5}
        ]}),
    )
    .await;
    assert_status(&res, StatusCode::CONFLICT);
    let body = json_body(res).await;
    assert_eq!(body["error"], "insufficient_stock");
    assert_eq!(body["stock_item_id"], farine.as_str());
    assert_eq!(body["available"], 500.0);
    assert_eq!(
        stock_quantity(&router, &cookie, &group_id, &farine).await,
        500.0
    );
    assert_eq!(
        stock_quantity(&router, &cookie, &group_id, &beurre).await,
        250.0
    );
    assert_eq!(meals(&router, &cookie, &group_id).await, 0);
    assert_eq!(planned_count(&router, &cookie, &group_id).await, 1);

    // 450 g is more than the recipe's 400 g, within the stock.
    let res = post(
        &router,
        &cookie,
        &done_path(&group_id, &recipe),
        json!({"removals": [{"stock_item_id": farine, "quantity": 450.0}]}),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(
        stock_quantity(&router, &cookie, &group_id, &farine).await,
        50.0
    );
    // The whole line may go, down to 0 and not below.
    let res = post(
        &router,
        &cookie,
        &format!("/groups/{group_id}/recipes/{recipe}/selection"),
        json!({}),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let res = post(
        &router,
        &cookie,
        &done_path(&group_id, &recipe),
        json!({"removals": [{"stock_item_id": farine, "quantity": 50.0}]}),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(
        stock_quantity(&router, &cookie, &group_id, &farine).await,
        0.0
    );
}

/// Concurrency preview → application: a line lowered or deleted since the
/// preview refuses the application with a clear code, and nothing is
/// applied.
#[sqlx::test]
async fn a_line_changed_since_the_preview_refuses_the_application(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(&router, &db, "done-stale@example.test").await;
    let member = register_verify_login(&router, &db, "done-stale-m@example.test").await;
    let group_id = create_group(&router, &owner).await;
    join(&router, &owner, &member, &group_id).await;
    let (recipe, farine, beurre, _) = crepes(&router, &owner, &group_id).await;

    let lines = done_preview(&router, &member, &group_id, &recipe).await;
    let proposed = by_name(&lines, "Farine")["proposed_quantity"].clone();
    // Another member uses some flour meanwhile.
    let patch = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{farine}"),
        Some(&owner),
        Some(json!({"quantity": 100.0})),
    )
    .await;
    assert_status(&patch, StatusCode::OK);

    let res = post(
        &router,
        &member,
        &done_path(&group_id, &recipe),
        json!({"removals": [
            {"stock_item_id": beurre, "quantity": 100.0},
            {"stock_item_id": farine, "quantity": proposed}
        ]}),
    )
    .await;
    assert_status(&res, StatusCode::CONFLICT);
    assert_eq!(json_body(res).await["error"], "insufficient_stock");
    assert_eq!(
        stock_quantity(&router, &owner, &group_id, &beurre).await,
        250.0
    );

    // The butter line is deleted meanwhile.
    let del = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/stock-items/{beurre}"),
        Some(&owner),
        None,
    )
    .await;
    assert!(del.status().is_success(), "{}", del.status());
    let res = post(
        &router,
        &member,
        &done_path(&group_id, &recipe),
        json!({"removals": [
            {"stock_item_id": farine, "quantity": 100.0},
            {"stock_item_id": beurre, "quantity": 100.0}
        ]}),
    )
    .await;
    assert_status(&res, StatusCode::CONFLICT);
    let body = json_body(res).await;
    assert_eq!(body["error"], "stock_item_gone");
    assert_eq!(body["stock_item_id"], beurre.as_str());
    assert_eq!(
        stock_quantity(&router, &owner, &group_id, &farine).await,
        100.0
    );
    assert_eq!(meals(&router, &owner, &group_id).await, 0);
    assert_eq!(planned_count(&router, &owner, &group_id).await, 1);
}

/// Two applications at once: the second waits on the first's lock on the
/// selection, then finds it closed. The stock is decremented once and one
/// meal is logged.
#[sqlx::test]
async fn done_twice_at_once_is_applied_once(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "done-race@example.test").await;
    let group_id = create_group(&router, &cookie).await;
    let (recipe, farine, _, _) = crepes(&router, &cookie, &group_id).await;

    // Hold the flour line, so that both applications are parked on a lock
    // before either can commit.
    let mut hold = manage_our_home::db::begin(&db).await.unwrap();
    sqlx::query("SELECT 1 FROM stock_items WHERE id = $1 FOR UPDATE")
        .bind(farine.parse::<uuid::Uuid>().unwrap())
        .execute(&mut *hold)
        .await
        .unwrap();

    let spawn = || {
        let (router, cookie, path) = (
            router.clone(),
            cookie.clone(),
            done_path(&group_id, &recipe),
        );
        let body = json!({"removals": [{"stock_item_id": farine, "quantity": 200.0}]});
        tokio::spawn(async move { post(&router, &cookie, &path, body).await.status() })
    };
    let (a, b) = (spawn(), spawn());
    common::wait_for_lock_waiters(&db, None, 2).await;
    hold.commit().await.unwrap();

    let mut statuses = vec![a.await.unwrap(), b.await.unwrap()];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::NOT_FOUND]);
    assert_eq!(
        stock_quantity(&router, &cookie, &group_id, &farine).await,
        300.0
    );
    assert_eq!(meals(&router, &cookie, &group_id).await, 1);
}

/// AC: abandoned removes only the unchecked items generated for the recipe
/// that the request names; the checked one, the kept one and the others
/// stay. An item checked since the preview refuses the application.
#[sqlx::test]
async fn abandoned_removes_only_the_named_unchecked_items(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(&router, &db, "abandon@example.test").await;
    let member = register_verify_login(&router, &db, "abandon-m@example.test").await;
    let group_id = create_group(&router, &owner).await;
    join(&router, &owner, &member, &group_id).await;
    let recipe = planned_recipe(
        &router,
        &owner,
        &group_id,
        json!({"name": "Pates tomate", "ingredients": [
            {"name": "Pates", "quantity": 500.0, "unit": "g"},
            {"name": "Tomates", "quantity": 4.0},
            {"name": "Basilic", "quantity": 1.0, "unit": "botte"}
        ]}),
        json!({}),
    )
    .await;
    let manual = created_id(
        post(
            &router,
            &owner,
            &format!("/groups/{group_id}/grocery-items"),
            json!({"name": "Pain"}),
        )
        .await,
    )
    .await;
    let generated = post(
        &router,
        &owner,
        &format!("/groups/{group_id}/grocery-items/generate"),
        json!({}),
    )
    .await;
    assert_status(&generated, StatusCode::CREATED);
    let items = json_body(generated).await["items"]
        .as_array()
        .unwrap()
        .clone();
    let id_of = |name: &str| {
        items.iter().find(|i| i["name"] == name).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (pates, tomates, basilic) = (id_of("Pates"), id_of("Tomates"), id_of("Basilic"));
    let check = post(
        &router,
        &owner,
        &format!("/groups/{group_id}/grocery-items/{basilic}/check"),
        json!({"checked": true}),
    )
    .await;
    assert_status(&check, StatusCode::OK);

    let res = get(&router, &member, &abandoned_path(&group_id, &recipe)).await;
    assert_status(&res, StatusCode::OK);
    let preview = json_body(res).await;
    let mut proposed: Vec<&str> = preview["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    proposed.sort();
    let mut expected = vec![pates.as_str(), tomates.as_str()];
    expected.sort();
    assert_eq!(proposed, expected, "only the unchecked ones");

    // The manual item and the checked one are not the recipe's to remove.
    for other in [&manual, &basilic] {
        let res = post(
            &router,
            &member,
            &abandoned_path(&group_id, &recipe),
            json!({"remove": [pates, other]}),
        )
        .await;
        assert_status(&res, StatusCode::CONFLICT);
        let body = json_body(res).await;
        assert_eq!(body["error"], "grocery_item_changed");
        assert_eq!(body["grocery_item_ids"], json!([other]));
    }
    assert_eq!(grocery_names(&router, &owner, &group_id).await.len(), 4);
    assert_eq!(planned_count(&router, &owner, &group_id).await, 1);

    // Tomates kept.
    let res = post(
        &router,
        &member,
        &abandoned_path(&group_id, &recipe),
        json!({"remove": [pates]}),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["selection"]["status"], "abandoned");
    assert_eq!(body["removed"], json!([pates]));
    assert_eq!(
        grocery_names(&router, &owner, &group_id).await,
        vec!["Basilic", "Pain", "Tomates"]
    );
    assert_eq!(planned_count(&router, &owner, &group_id).await, 0);
    assert_eq!(meals(&router, &owner, &group_id).await, 0);
}

/// An item checked between the preview and the application refuses it.
#[sqlx::test]
async fn an_item_checked_since_the_preview_refuses_the_abandon(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "abandon-stale@example.test").await;
    let group_id = create_group(&router, &cookie).await;
    let recipe = planned_recipe(
        &router,
        &cookie,
        &group_id,
        json!({"name": "Salade", "ingredients": [{"name": "Laitue", "quantity": 1.0}]}),
        json!({}),
    )
    .await;
    let generated = post(
        &router,
        &cookie,
        &format!("/groups/{group_id}/grocery-items/generate"),
        json!({}),
    )
    .await;
    assert_status(&generated, StatusCode::CREATED);
    let res = get(&router, &cookie, &abandoned_path(&group_id, &recipe)).await;
    let laitue = json_body(res).await["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let check = post(
        &router,
        &cookie,
        &format!("/groups/{group_id}/grocery-items/{laitue}/check"),
        json!({"checked": true}),
    )
    .await;
    assert_status(&check, StatusCode::OK);

    let res = post(
        &router,
        &cookie,
        &abandoned_path(&group_id, &recipe),
        json!({"remove": [laitue]}),
    )
    .await;
    assert_status(&res, StatusCode::CONFLICT);
    assert_eq!(json_body(res).await["error"], "grocery_item_changed");
    assert_eq!(
        grocery_names(&router, &cookie, &group_id).await,
        vec!["Laitue"]
    );
    assert_eq!(planned_count(&router, &cookie, &group_id).await, 1);
}

/// AC: a selection of another group is a 404 through one's own group, and an
/// outsider using the other group's path is refused (403). A stock line of
/// another group cannot be decremented, nor one that names no ingredient.
#[sqlx::test]
async fn outcomes_are_isolated_between_groups(db: PgPool) {
    let router = test_router(db.clone());
    let alice = register_verify_login(&router, &db, "iso-a@example.test").await;
    let bob = register_verify_login(&router, &db, "iso-b@example.test").await;
    let group_a = create_group(&router, &alice).await;
    let group_b = create_group(&router, &bob).await;
    let (recipe_a, farine_a, _, _) = crepes(&router, &alice, &group_a).await;
    let farine_b = stock(
        &router,
        &bob,
        &group_b,
        json!({"name": "Farine", "quantity": 500.0, "unit": "g"}),
    )
    .await;
    let recipe_b = planned_recipe(
        &router,
        &bob,
        &group_b,
        json!({"name": "Pain", "ingredients": [{"name": "Farine", "quantity": 300.0, "unit": "g"}]}),
        json!({}),
    )
    .await;

    for path in [
        done_path(&group_b, &recipe_a),
        abandoned_path(&group_b, &recipe_a),
    ] {
        assert_status(&get(&router, &bob, &path).await, StatusCode::NOT_FOUND);
    }
    let res = post(
        &router,
        &bob,
        &done_path(&group_b, &recipe_a),
        json!({"removals": [{"stock_item_id": farine_b, "quantity": 100.0}]}),
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);
    let res = post(
        &router,
        &bob,
        &abandoned_path(&group_b, &recipe_a),
        json!({"remove": []}),
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);
    for path in [
        done_path(&group_a, &recipe_a),
        abandoned_path(&group_a, &recipe_a),
    ] {
        assert_status(&get(&router, &bob, &path).await, StatusCode::FORBIDDEN);
    }

    // Bob's own recipe, with Alice's flour line: not visible from his group.
    let res = post(
        &router,
        &bob,
        &done_path(&group_b, &recipe_b),
        json!({"removals": [{"stock_item_id": farine_a, "quantity": 100.0}]}),
    )
    .await;
    assert_status(&res, StatusCode::CONFLICT);
    assert_eq!(json_body(res).await["error"], "stock_item_gone");
    assert_eq!(
        stock_quantity(&router, &alice, &group_a, &farine_a).await,
        500.0
    );

    // A line of his group that the recipe does not call for.
    let jam = stock(
        &router,
        &bob,
        &group_b,
        json!({"name": "Confiture", "quantity": 1.0}),
    )
    .await;
    let res = post(
        &router,
        &bob,
        &done_path(&group_b, &recipe_b),
        json!({"removals": [{"stock_item_id": jam, "quantity": 1.0}]}),
    )
    .await;
    assert_status(&res, StatusCode::BAD_REQUEST);
    assert_eq!(json_body(res).await["error"], "stock_item_not_in_recipe");
    assert_eq!(planned_count(&router, &alice, &group_a).await, 1);
    assert_eq!(planned_count(&router, &bob, &group_b).await, 1);
}

/// Malformed corrections are refused before anything is read.
#[sqlx::test]
async fn malformed_corrections_are_refused(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "done-bad@example.test").await;
    let group_id = create_group(&router, &cookie).await;
    let (recipe, farine, _, _) = crepes(&router, &cookie, &group_id).await;

    for (body, code) in [
        (
            json!({"removals": [{"stock_item_id": farine, "quantity": -1.0}]}),
            "invalid_quantity",
        ),
        (
            json!({"removals": [
                {"stock_item_id": farine, "quantity": 1.0},
                {"stock_item_id": farine, "quantity": 2.0}
            ]}),
            "duplicate_stock_item",
        ),
    ] {
        let res = post(&router, &cookie, &done_path(&group_id, &recipe), body).await;
        assert_status(&res, StatusCode::BAD_REQUEST);
        assert_eq!(json_body(res).await["error"], code);
    }
    let res = post(
        &router,
        &cookie,
        &abandoned_path(&group_id, &recipe),
        json!({"remove": [farine, farine]}),
    )
    .await;
    assert_status(&res, StatusCode::BAD_REQUEST);
    assert_eq!(json_body(res).await["error"], "duplicate_grocery_item");
    assert_eq!(
        stock_quantity(&router, &cookie, &group_id, &farine).await,
        500.0
    );
    assert_eq!(planned_count(&router, &cookie, &group_id).await, 1);
}

/// 1.0 − 0.9 leaves 0.1 in the stock, not `float8`'s 0.09999999999999998:
/// the 0.1 left can then be taken.
#[sqlx::test]
async fn what_is_left_carries_no_binary_noise(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "done-noise@example.test").await;
    let group_id = create_group(&router, &cookie).await;
    let lait = stock(
        &router,
        &cookie,
        &group_id,
        json!({"name": "Lait", "quantity": 1.0, "unit": "L"}),
    )
    .await;
    let recipe = planned_recipe(
        &router,
        &cookie,
        &group_id,
        json!({"name": "Flan", "ingredients": [{"name": "Lait", "quantity": 0.9, "unit": "L"}]}),
        json!({}),
    )
    .await;
    let res = post(
        &router,
        &cookie,
        &done_path(&group_id, &recipe),
        json!({"removals": [{"stock_item_id": lait, "quantity": 0.9}]}),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(
        stock_quantity(&router, &cookie, &group_id, &lait).await,
        0.1
    );

    let res = post(
        &router,
        &cookie,
        &format!("/groups/{group_id}/recipes/{recipe}/selection"),
        json!({}),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let lines = done_preview(&router, &cookie, &group_id, &recipe).await;
    assert_eq!(lines[0]["proposed_quantity"], 0.1);
    let res = post(
        &router,
        &cookie,
        &done_path(&group_id, &recipe),
        json!({"removals": [{"stock_item_id": lait, "quantity": 0.1}]}),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(
        stock_quantity(&router, &cookie, &group_id, &lait).await,
        0.0
    );
}

/// An ingredient is spread over several lots of its name and unit, the one
/// expiring first first: 100 g + 500 g cover 400 g instead of capping at
/// 100 g. The application takes from each lot, and refuses (nothing
/// applied) when one of them was lowered since the preview.
#[sqlx::test]
async fn an_ingredient_is_taken_from_several_lots(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "done-lots@example.test").await;
    let group_id = create_group(&router, &cookie).await;
    let later = stock(
        &router,
        &cookie,
        &group_id,
        json!({"name": "Farine", "quantity": 500.0, "unit": "g", "expires_on": "2026-12-20"}),
    )
    .await;
    let undated = stock(
        &router,
        &cookie,
        &group_id,
        json!({"name": "farine", "quantity": 300.0, "unit": "g"}),
    )
    .await;
    let sooner = stock(
        &router,
        &cookie,
        &group_id,
        json!({"name": "Farine", "quantity": 100.0, "unit": "g", "expires_on": "2026-12-03"}),
    )
    .await;
    let recipe = planned_recipe(
        &router,
        &cookie,
        &group_id,
        json!({"name": "Pain", "ingredients": [{"name": "Farine", "quantity": 400.0, "unit": "g"}]}),
        json!({}),
    )
    .await;

    let lines = done_preview(&router, &cookie, &group_id, &recipe).await;
    let f = by_name(&lines, "Farine");
    assert_eq!(f["proposed_quantity"], 400.0);
    assert_eq!(f["warning"], Value::Null);
    let lots = f["lots"].as_array().unwrap();
    assert_eq!(lots.len(), 2, "{lots:?}");
    assert_eq!(lots[0]["stock_item"]["id"], sooner.as_str());
    assert_eq!(lots[0]["quantity"], 100.0);
    assert_eq!(lots[1]["stock_item"]["id"], later.as_str());
    assert_eq!(lots[1]["quantity"], 300.0);
    let removals = json!({"removals": [
        {"stock_item_id": sooner, "quantity": 100.0},
        {"stock_item_id": later, "quantity": 300.0}
    ]});

    // The later lot is lowered meanwhile: refused, nothing applied.
    let patch = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{later}"),
        Some(&cookie),
        Some(json!({"quantity": 200.0})),
    )
    .await;
    assert_status(&patch, StatusCode::OK);
    let res = post(
        &router,
        &cookie,
        &done_path(&group_id, &recipe),
        removals.clone(),
    )
    .await;
    assert_status(&res, StatusCode::CONFLICT);
    let body = json_body(res).await;
    assert_eq!(body["error"], "insufficient_stock");
    assert_eq!(body["stock_item_id"], later.as_str());
    assert_eq!(body["available"], 200.0);
    assert_eq!(
        stock_quantity(&router, &cookie, &group_id, &sooner).await,
        100.0
    );
    assert_eq!(meals(&router, &cookie, &group_id).await, 0);
    assert_eq!(planned_count(&router, &cookie, &group_id).await, 1);

    // Restored: both lots are taken from, the undated one untouched.
    let patch = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{later}"),
        Some(&cookie),
        Some(json!({"quantity": 500.0})),
    )
    .await;
    assert_status(&patch, StatusCode::OK);
    let res = post(&router, &cookie, &done_path(&group_id, &recipe), removals).await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(
        json_body(res).await["stock_items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        stock_quantity(&router, &cookie, &group_id, &sooner).await,
        0.0
    );
    assert_eq!(
        stock_quantity(&router, &cookie, &group_id, &later).await,
        200.0
    );
    assert_eq!(
        stock_quantity(&router, &cookie, &group_id, &undated).await,
        300.0
    );
}

/// An outsider cannot apply either action through the group's path (403),
/// and nothing is applied.
#[sqlx::test]
async fn an_outsider_cannot_apply_either_action(db: PgPool) {
    let router = test_router(db.clone());
    let alice = register_verify_login(&router, &db, "out-a@example.test").await;
    let bob = register_verify_login(&router, &db, "out-b@example.test").await;
    let group_id = create_group(&router, &alice).await;
    let (recipe, farine, _, _) = crepes(&router, &alice, &group_id).await;

    let res = post(
        &router,
        &bob,
        &done_path(&group_id, &recipe),
        json!({"removals": [{"stock_item_id": farine, "quantity": 100.0}]}),
    )
    .await;
    assert_status(&res, StatusCode::FORBIDDEN);
    let res = post(
        &router,
        &bob,
        &abandoned_path(&group_id, &recipe),
        json!({"remove": []}),
    )
    .await;
    assert_status(&res, StatusCode::FORBIDDEN);
    assert_eq!(
        stock_quantity(&router, &alice, &group_id, &farine).await,
        500.0
    );
    assert_eq!(meals(&router, &alice, &group_id).await, 0);
    assert_eq!(planned_count(&router, &alice, &group_id).await, 1);
}

/// `remove` is required: an abandon without it is refused by the body's
/// deserialization (422) instead of keeping every item silently.
#[sqlx::test]
async fn an_abandon_without_remove_is_refused(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(&router, &db, "abandon-422@example.test").await;
    let group_id = create_group(&router, &cookie).await;
    let (recipe, _, _, _) = crepes(&router, &cookie, &group_id).await;

    let res = post(
        &router,
        &cookie,
        &abandoned_path(&group_id, &recipe),
        json!({}),
    )
    .await;
    assert_status(&res, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(planned_count(&router, &cookie, &group_id).await, 1);
}
