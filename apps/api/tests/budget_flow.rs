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

/// AC: full CRUD lifecycle for a manually-entered budget entry, scoped to a group.
#[sqlx::test]
async fn full_budget_entry_lifecycle(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "budget-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Farine", "amount": 2.5})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let entry = json_body(create).await;
    let entry_id = entry["id"].as_str().unwrap().to_string();
    assert_eq!(entry["name"], "Farine");
    assert_eq!(entry["amount"], 2.5);

    let get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/budget-entries/{entry_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&get, StatusCode::OK);

    let update = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/budget-entries/{entry_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"amount": 3.0})),
    )
    .await;
    assert_status(&update, StatusCode::OK);
    assert_eq!(json_body(update).await["amount"], 3.0);

    let list = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&list, StatusCode::OK);
    assert_eq!(
        json_body(list).await["entries"].as_array().unwrap().len(),
        1
    );

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/budget-entries/{entry_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::NO_CONTENT);

    let get_after_delete = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/budget-entries/{entry_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&get_after_delete, StatusCode::NOT_FOUND);
}

/// AC: only the entry's creator or a group admin/owner may edit or delete it,
/// same permission bar as Stocks/Recipes/Grocery list.
#[sqlx::test]
async fn only_creator_or_admin_can_modify(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "budget-owner2@example.test",
        "owner-password1",
    )
    .await;
    let member_cookie = register_verify_login(
        &router,
        &db,
        "budget-member@example.test",
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
        &format!("/groups/{group_id}/budget-entries"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Lait", "amount": 1.2})),
    )
    .await;
    let entry_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let member_edit = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/budget-entries/{entry_id}"),
        Some(&member_cookie),
        Some(serde_json::json!({"amount": 5.0})),
    )
    .await;
    assert_status(&member_edit, StatusCode::FORBIDDEN);

    let member_delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/budget-entries/{entry_id}"),
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&member_delete, StatusCode::FORBIDDEN);

    let owner_delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/budget-entries/{entry_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&owner_delete, StatusCode::NO_CONTENT);
}

/// AC: a non-member of the group cannot read or write its budget entries.
#[sqlx::test]
async fn non_member_cannot_access_group_budget_entries(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "budget-owner3@example.test",
        "owner-password1",
    )
    .await;
    let outsider_cookie = register_verify_login(
        &router,
        &db,
        "budget-outsider@example.test",
        "outsider-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Riz", "amount": 1.0})),
    )
    .await;
    let entry_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let outsider_get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/budget-entries/{entry_id}"),
        Some(&outsider_cookie),
        None,
    )
    .await;
    assert_status(&outsider_get, StatusCode::FORBIDDEN);

    let outsider_create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&outsider_cookie),
        Some(serde_json::json!({"name": "Intrusion", "amount": 1.0})),
    )
    .await;
    assert_status(&outsider_create, StatusCode::FORBIDDEN);
}

/// AC: `name` is required (rejects blank) and `amount` cannot go negative.
#[sqlx::test]
async fn validation_rejects_blank_name_and_negative_amount(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "budget-valid@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let blank_name = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "   ", "amount": 1.0})),
    )
    .await;
    assert_status(&blank_name, StatusCode::BAD_REQUEST);

    let negative_amount = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Beurre", "amount": -1.0})),
    )
    .await;
    assert_status(&negative_amount, StatusCode::BAD_REQUEST);
}

/// AC: a price can be associated with a grocery item, e.g. once it's been
/// checked/bought (docs/v1-scope.md epic #6), denormalizing the item's name
/// onto the budget entry.
#[sqlx::test]
async fn price_can_be_set_on_grocery_item(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "budget-price@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create_item = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Farine"})),
    )
    .await;
    let item_id = json_body(create_item).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items/{item_id}/check"),
        Some(&owner_cookie),
        Some(serde_json::json!({"checked": true})),
    )
    .await;

    let set_price = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items/{item_id}/price"),
        Some(&owner_cookie),
        Some(serde_json::json!({"amount": 2.35})),
    )
    .await;
    assert_status(&set_price, StatusCode::CREATED);
    let entry = json_body(set_price).await;
    assert_eq!(entry["name"], "Farine");
    assert_eq!(entry["amount"], 2.35);
    assert_eq!(entry["grocery_item_id"], item_id);
}

/// AC: spend is cumulated per period (month) at the family level.
#[sqlx::test]
async fn summary_cumulates_spend_per_month(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "budget-summary@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Farine", "amount": 2.0, "spent_at": "2026-07-01"})),
    )
    .await;
    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Lait", "amount": 1.5, "spent_at": "2026-07-15"})),
    )
    .await;
    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Oeufs", "amount": 3.0, "spent_at": "2026-06-20"})),
    )
    .await;

    let summary = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/budget-entries/summary"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&summary, StatusCode::OK);
    let periods = json_body(summary).await["periods"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(periods.len(), 2);

    let july = periods
        .iter()
        .find(|p| p["period"] == "2026-07-01")
        .expect("july period present");
    assert_eq!(july["total"], 3.5);

    let june = periods
        .iter()
        .find(|p| p["period"] == "2026-06-01")
        .expect("june period present");
    assert_eq!(june["total"], 3.0);
}
