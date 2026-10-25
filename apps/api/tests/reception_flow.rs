//! Receiving the groceries into the stock (#471): `GET`/`POST
//! /groups/:id/stock-items/reception`.

mod common;

use axum::http::{Method, StatusCode};
use common::{assert_status, call, json_body, set_cookie, test_router};
use serde_json::{json, Value};
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

async fn create_group(router: &axum::Router, cookie: &str, name: &str) -> String {
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
    assert!(accept.status().is_success(), "{}", accept.status());
}

async fn stock(router: &axum::Router, cookie: &str, group_id: &str, body: Value) -> String {
    let res = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(cookie),
        Some(body),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

/// A grocery item, checked off.
async fn checked(router: &axum::Router, cookie: &str, group_id: &str, body: Value) -> String {
    let res = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items"),
        Some(cookie),
        Some(body),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let id = json_body(res).await["id"].as_str().unwrap().to_string();
    let check = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items/{id}/check"),
        Some(cookie),
        Some(json!({"checked": true})),
    )
    .await;
    assert_status(&check, StatusCode::OK);
    id
}

async fn receive(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    items: Value,
) -> axum::http::Response<axum::body::Body> {
    call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items/reception"),
        Some(cookie),
        Some(json!({ "items": items })),
    )
    .await
}

async fn proposals(router: &axum::Router, cookie: &str, group_id: &str) -> Vec<Value> {
    let res = call(
        router,
        Method::GET,
        &format!("/groups/{group_id}/stock-items/reception"),
        Some(cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    json_body(res).await["items"].as_array().unwrap().clone()
}

async fn stock_items(router: &axum::Router, cookie: &str, group_id: &str) -> Vec<Value> {
    let res = call(
        router,
        Method::GET,
        &format!("/groups/{group_id}/stock-items"),
        Some(cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    json_body(res).await["items"].as_array().unwrap().clone()
}

async fn grocery_ids(router: &axum::Router, cookie: &str, group_id: &str) -> Vec<String> {
    let res = call(
        router,
        Method::GET,
        &format!("/groups/{group_id}/grocery-items"),
        Some(cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    json_body(res).await["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

async fn error_code(res: axum::http::Response<axum::body::Body>) -> String {
    json_body(res).await["error"].as_str().unwrap().to_string()
}

/// AC: « lait 2 L » received against an undated « Lait 1 L » merges → 3 L,
/// and the item leaves the list.
#[sqlx::test]
async fn an_undated_receipt_merges_into_the_matching_line(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "rcv-merge@example.test", "owner-password1").await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let milk = stock(
        &router,
        &cookie,
        &group_id,
        json!({"name": "Lait", "quantity": 1.0, "unit": "L"}),
    )
    .await;
    let item = checked(
        &router,
        &cookie,
        &group_id,
        json!({"name": "lait", "quantity": 2.0, "unit": "L"}),
    )
    .await;
    // An unchecked item is not proposed.
    let pending = {
        let res = call(
            &router,
            Method::POST,
            &format!("/groups/{group_id}/grocery-items"),
            Some(&cookie),
            Some(json!({"name": "Pain"})),
        )
        .await;
        json_body(res).await["id"].as_str().unwrap().to_string()
    };

    let proposed = proposals(&router, &cookie, &group_id).await;
    assert_eq!(proposed.len(), 1, "{proposed:?}");
    assert_eq!(proposed[0]["grocery_item_id"], item.as_str());
    assert_eq!(proposed[0]["mode"], "merge");
    assert_eq!(proposed[0]["quantity"], 2.0);
    assert_eq!(proposed[0]["unit"], "L");
    assert_eq!(proposed[0]["stock_item"]["id"], milk.as_str());

    let res = receive(
        &router,
        &cookie,
        &group_id,
        json!([{"grocery_item_id": item}]),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let lines = json_body(res).await["items"].as_array().unwrap().clone();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["mode"], "merge");
    assert_eq!(lines[0]["stock_item"]["id"], milk.as_str());
    assert_eq!(lines[0]["stock_item"]["quantity"], 3.0);

    let stock = stock_items(&router, &cookie, &group_id).await;
    assert_eq!(stock.len(), 1, "no new line: {stock:?}");
    assert_eq!(stock[0]["quantity"], 3.0);
    assert_eq!(
        grocery_ids(&router, &cookie, &group_id).await,
        vec![pending]
    );
}

/// AC: a date different from the line's proposes `new`; the receipt makes a
/// line of its own and leaves the existing one as it was. Forcing `merge`
/// there is refused.
#[sqlx::test]
async fn a_different_date_makes_a_new_line(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "rcv-new@example.test", "owner-password1").await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let milk = stock(
        &router,
        &cookie,
        &group_id,
        json!({"name": "Lait", "quantity": 1.0, "unit": "L", "expires_on": "2026-11-01"}),
    )
    .await;
    let item = checked(
        &router,
        &cookie,
        &group_id,
        json!({"name": "Lait", "quantity": 2.0, "unit": "L"}),
    )
    .await;

    let forced = receive(
        &router,
        &cookie,
        &group_id,
        json!([{"grocery_item_id": item, "expires_on": "2026-11-05", "mode": "merge"}]),
    )
    .await;
    assert_status(&forced, StatusCode::CONFLICT);
    assert_eq!(error_code(forced).await, "no_stock_to_merge");
    assert_eq!(
        grocery_ids(&router, &cookie, &group_id).await,
        vec![item.clone()]
    );

    let res = receive(
        &router,
        &cookie,
        &group_id,
        json!([{"grocery_item_id": item, "expires_on": "2026-11-05"}]),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let line = json_body(res).await["items"][0].clone();
    assert_eq!(line["mode"], "new");
    assert_ne!(line["stock_item"]["id"], milk.as_str());
    assert_eq!(line["stock_item"]["expires_on"], "2026-11-05");
    assert_eq!(line["stock_item"]["quantity"], 2.0);

    let stock = stock_items(&router, &cookie, &group_id).await;
    assert_eq!(stock.len(), 2, "{stock:?}");
    let old = stock.iter().find(|s| s["id"] == milk.as_str()).unwrap();
    assert_eq!(old["quantity"], 1.0);
    assert_eq!(old["expires_on"], "2026-11-01");
    assert!(grocery_ids(&router, &cookie, &group_id).await.is_empty());
}

/// `new` chosen against a matching line, edited fields, and two items of the
/// same article in one lot: the second merges into the line the first made.
#[sqlx::test]
async fn the_mode_and_fields_sent_are_applied(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "rcv-fields@example.test", "owner-password1").await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let rice = stock(
        &router,
        &cookie,
        &group_id,
        json!({"name": "Riz", "quantity": 1.0, "unit": "kg"}),
    )
    .await;
    let rice_item = checked(
        &router,
        &cookie,
        &group_id,
        json!({"name": "Riz", "quantity": 1.0, "unit": "kg"}),
    )
    .await;
    let yogurt_a = checked(&router, &cookie, &group_id, json!({"name": "Yaourts"})).await;
    let yogurt_b = checked(
        &router,
        &cookie,
        &group_id,
        json!({"name": "yaourts nature"}),
    )
    .await;

    let res = receive(
        &router,
        &cookie,
        &group_id,
        json!([
            {"grocery_item_id": rice_item, "mode": "new"},
            {"grocery_item_id": yogurt_a, "quantity": 4.0, "unit": "pot"},
            {"grocery_item_id": yogurt_b, "name": "Yaourts", "quantity": 8.0, "unit": "pot"},
        ]),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let lines = json_body(res).await["items"].as_array().unwrap().clone();
    assert_eq!(lines[0]["mode"], "new");
    assert_ne!(lines[0]["stock_item"]["id"], rice.as_str());
    assert_eq!(lines[1]["mode"], "new");
    assert_eq!(lines[2]["mode"], "merge");
    assert_eq!(lines[2]["stock_item"]["id"], lines[1]["stock_item"]["id"]);
    assert_eq!(lines[2]["stock_item"]["quantity"], 12.0);

    let stock = stock_items(&router, &cookie, &group_id).await;
    assert_eq!(
        stock.len(),
        3,
        "two rice lines and one yogurt line: {stock:?}"
    );
    assert!(grocery_ids(&router, &cookie, &group_id).await.is_empty());
}

/// AC (atomicity, isolation): an item of another group in the lot refuses
/// the whole lot; so does an invalid item after a valid one. Nothing is
/// applied either way.
#[sqlx::test]
async fn a_bad_item_refuses_the_whole_lot(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "rcv-atomic@example.test", "owner-password1").await;
    let home = create_group(&router, &cookie, "Foyer").await;
    let other = create_group(&router, &cookie, "Chalet").await;
    stock(
        &router,
        &cookie,
        &home,
        json!({"name": "Lait", "quantity": 1.0, "unit": "L"}),
    )
    .await;
    let milk_item = checked(
        &router,
        &cookie,
        &home,
        json!({"name": "Lait", "quantity": 2.0, "unit": "L"}),
    )
    .await;
    let foreign = checked(
        &router,
        &cookie,
        &other,
        json!({"name": "Bois", "quantity": 3.0}),
    )
    .await;

    let res = receive(
        &router,
        &cookie,
        &home,
        json!([{"grocery_item_id": milk_item}, {"grocery_item_id": foreign}]),
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);

    let res = receive(
        &router,
        &cookie,
        &home,
        json!([{"grocery_item_id": milk_item}, {"grocery_item_id": milk_item}]),
    )
    .await;
    assert_status(&res, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(res).await, "duplicate_grocery_item");

    let bread = checked(&router, &cookie, &home, json!({"name": "Pain"})).await;
    let res = receive(
        &router,
        &cookie,
        &home,
        json!([{"grocery_item_id": milk_item}, {"grocery_item_id": bread, "quantity": 0.0}]),
    )
    .await;
    assert_status(&res, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(res).await, "quantity_must_be_positive");

    let empty = receive(&router, &cookie, &home, json!([])).await;
    assert_status(&empty, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(empty).await, "items_required");

    let stock = stock_items(&router, &cookie, &home).await;
    assert_eq!(stock.len(), 1);
    assert_eq!(stock[0]["quantity"], 1.0, "the merge was rolled back");
    let mut left = grocery_ids(&router, &cookie, &home).await;
    left.sort();
    let mut expected = vec![milk_item, bread];
    expected.sort();
    assert_eq!(left, expected);
    assert_eq!(grocery_ids(&router, &cookie, &other).await, vec![foreign]);
    assert!(stock_items(&router, &cookie, &other).await.is_empty());
}

/// Permissions are those of stock creation: any member receives, whoever
/// added the item; an outsider is refused.
#[sqlx::test]
async fn any_member_receives_and_an_outsider_is_refused(db: PgPool) {
    let router = test_router(db.clone());
    let owner =
        register_verify_login(&router, &db, "rcv-owner@example.test", "owner-password1").await;
    let member =
        register_verify_login(&router, &db, "rcv-member@example.test", "member-password1").await;
    let outsider = register_verify_login(
        &router,
        &db,
        "rcv-outsider@example.test",
        "outsider-password1",
    )
    .await;
    let group_id = create_group(&router, &owner, "Foyer").await;
    join(&router, &owner, &member, &group_id).await;
    let item = checked(
        &router,
        &owner,
        &group_id,
        json!({"name": "Farine", "quantity": 1.0, "unit": "kg"}),
    )
    .await;

    let get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/stock-items/reception"),
        Some(&outsider),
        None,
    )
    .await;
    assert_status(&get, StatusCode::FORBIDDEN);
    let post = receive(
        &router,
        &outsider,
        &group_id,
        json!([{"grocery_item_id": item}]),
    )
    .await;
    assert_status(&post, StatusCode::FORBIDDEN);

    let res = receive(
        &router,
        &member,
        &group_id,
        json!([{"grocery_item_id": item}]),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    assert!(grocery_ids(&router, &owner, &group_id).await.is_empty());
    assert_eq!(stock_items(&router, &owner, &group_id).await.len(), 1);
}
