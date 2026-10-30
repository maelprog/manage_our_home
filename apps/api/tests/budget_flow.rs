mod common;

use axum::http::{Method, StatusCode};
use chrono::{Datelike, NaiveDate, Utc};
use common::{
    assert_status, call, json_body, set_cookie, test_router, test_router_sized, waiting_on_table,
};
use manage_our_home_shared::validation::auth::paris_day;
use sqlx::PgPool;

/// `day` is today in Paris, read before and after the request (#608): the
/// undated entry takes the Paris day, not the database's UTC one.
fn assert_paris_today(day: &serde_json::Value, before: NaiveDate, after: NaiveDate) {
    let day: NaiveDate = day.as_str().unwrap().parse().unwrap();
    assert!(
        day == before || day == after,
        "an undated entry is dated today in Paris, got {day}"
    );
}

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

/// AC: full CRUD lifecycle for a manually-entered budget entry, scoped to a group.
#[sqlx::test]
async fn full_budget_entry_lifecycle(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "budget-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let before = paris_day(Utc::now());
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Farine", "amount": 2.5})),
    )
    .await;
    let after = paris_day(Utc::now());
    assert_status(&create, StatusCode::CREATED);
    let entry = json_body(create).await;
    let entry_id = entry["id"].as_str().unwrap().to_string();
    assert_eq!(entry["name"], "Farine");
    assert_eq!(entry["amount"], 2.5);
    assert_paris_today(&entry["spent_at"], before, after);

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

/// Joins `member_cookie` to the group through an invitation of the owner's:
/// the member comes in with the `standard` role.
async fn join_as_standard(
    router: &axum::Router,
    owner_cookie: &str,
    member_cookie: &str,
    group_id: &str,
) {
    let invite = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(owner_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&invite, StatusCode::CREATED);
    let token = json_body(invite).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let accept = call(
        router,
        Method::POST,
        &format!("/groups/invitations/{token}/accept"),
        Some(member_cookie),
        None,
    )
    .await;
    assert_status(&accept, StatusCode::OK);
}

/// The owner gives `email`'s account `role` in the group.
async fn set_role(
    router: &axum::Router,
    db: &PgPool,
    owner_cookie: &str,
    group_id: &str,
    email: &str,
    role: &str,
) {
    let member_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(db)
        .await
        .unwrap();
    let res = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/members/{member_id}/role"),
        Some(owner_cookie),
        Some(serde_json::json!({ "role": role })),
    )
    .await;
    assert_status(&res, StatusCode::OK);
}

/// A grocery item of the group, checked off by `cookie`'s account.
async fn checked_grocery_item(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    name: &str,
) -> String {
    let create = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items"),
        Some(cookie),
        Some(serde_json::json!({ "name": name })),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let item_id = json_body(create).await["id"].as_str().unwrap().to_string();
    let check = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items/{item_id}/check"),
        Some(cookie),
        Some(serde_json::json!({"checked": true})),
    )
    .await;
    assert_status(&check, StatusCode::OK);
    item_id
}

/// AC #531: a standard member — a child of the family — is refused every
/// budget route, the grocery item's price included, with the 404 a
/// nonexistent resource gets: neither the entries nor the existence of an
/// entry id leak. Nothing they send changes the budget.
#[sqlx::test]
async fn a_standard_member_gets_404_on_every_budget_route(db: PgPool) {
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
    join_as_standard(&router, &owner_cookie, &member_cookie, &group_id).await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Lait", "amount": 1.2})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let entry_id = json_body(create).await["id"].as_str().unwrap().to_string();
    // The member may still check the item off: the grocery list stays theirs.
    let item_id = checked_grocery_item(&router, &member_cookie, &group_id, "Pain").await;

    let entries = format!("/groups/{group_id}/budget-entries");
    let entry = format!("{entries}/{entry_id}");
    let requests = [
        ("list", Method::GET, entries.clone(), None),
        ("summary", Method::GET, format!("{entries}/summary"), None),
        (
            "create",
            Method::POST,
            entries.clone(),
            Some(serde_json::json!({"name": "Bonbons", "amount": 2.0})),
        ),
        ("get", Method::GET, entry.clone(), None),
        (
            "update",
            Method::PATCH,
            entry.clone(),
            Some(serde_json::json!({"amount": 5.0})),
        ),
        ("delete", Method::DELETE, entry.clone(), None),
        (
            "price",
            Method::POST,
            format!("/groups/{group_id}/grocery-items/{item_id}/price"),
            Some(serde_json::json!({"amount": 3.0})),
        ),
    ];
    for (what, method, uri, body) in requests {
        let res = call(&router, method, &uri, Some(&member_cookie), body).await;
        assert_eq!(
            res.status(),
            StatusCode::NOT_FOUND,
            "a standard member's {what} must be a 404"
        );
    }

    // The budget is as the owner left it: one entry, untouched, and no price
    // on the item.
    let list = call(&router, Method::GET, &entries, Some(&owner_cookie), None).await;
    assert_status(&list, StatusCode::OK);
    let list = json_body(list).await;
    let list = list["entries"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0]["id"], entry_id.as_str());
    assert_eq!(list[0]["amount"], 1.2);
}

/// AC #531: an admin and the owner reach every budget route — list, summary,
/// create, read, edit, delete and a grocery item's price — each acting on an
/// entry the other one created.
#[sqlx::test]
async fn an_admin_and_the_owner_reach_every_budget_route(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "budget-owner5@example.test",
        "owner-password1",
    )
    .await;
    let admin_cookie =
        register_verify_login(&router, &db, "budget-admin@example.test", "admin-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    join_as_standard(&router, &owner_cookie, &admin_cookie, &group_id).await;
    set_role(
        &router,
        &db,
        &owner_cookie,
        &group_id,
        "budget-admin@example.test",
        "admin",
    )
    .await;

    let entries = format!("/groups/{group_id}/budget-entries");
    for (who, cookie, other) in [
        ("owner", &owner_cookie, &admin_cookie),
        ("admin", &admin_cookie, &owner_cookie),
    ] {
        let create = call(
            &router,
            Method::POST,
            &entries,
            Some(other),
            Some(serde_json::json!({"name": "Lait", "amount": 1.2})),
        )
        .await;
        assert_status(&create, StatusCode::CREATED);
        let entry_id = json_body(create).await["id"].as_str().unwrap().to_string();
        let entry = format!("{entries}/{entry_id}");
        let item_id = checked_grocery_item(&router, cookie, &group_id, who).await;

        let requests = [
            ("list", Method::GET, entries.clone(), None, StatusCode::OK),
            (
                "summary",
                Method::GET,
                format!("{entries}/summary"),
                None,
                StatusCode::OK,
            ),
            (
                "create",
                Method::POST,
                entries.clone(),
                Some(serde_json::json!({"name": "Beurre", "amount": 2.0})),
                StatusCode::CREATED,
            ),
            ("get", Method::GET, entry.clone(), None, StatusCode::OK),
            (
                "update",
                Method::PATCH,
                entry.clone(),
                Some(serde_json::json!({"amount": 5.0})),
                StatusCode::OK,
            ),
            (
                "delete",
                Method::DELETE,
                entry.clone(),
                None,
                StatusCode::NO_CONTENT,
            ),
            (
                "price",
                Method::POST,
                format!("/groups/{group_id}/grocery-items/{item_id}/price"),
                Some(serde_json::json!({"amount": 3.0})),
                StatusCode::CREATED,
            ),
        ];
        for (what, method, uri, body, expected) in requests {
            let res = call(&router, method, &uri, Some(cookie), body).await;
            assert_eq!(res.status(), expected, "the {who}'s {what}");
        }
    }
}

/// AC #531: the RGPD export of a member who has become standard still hands
/// back the entries they created while they were an admin — the export reads
/// `created_by`, not the budget's access bar.
#[sqlx::test]
async fn a_standard_member_s_export_keeps_the_entries_they_created(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "budget-owner6@example.test",
        "owner-password1",
    )
    .await;
    let email = "budget-former-admin@example.test";
    let member_cookie = register_verify_login(&router, &db, email, "member-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    join_as_standard(&router, &owner_cookie, &member_cookie, &group_id).await;
    set_role(&router, &db, &owner_cookie, &group_id, email, "admin").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&member_cookie),
        Some(serde_json::json!({"name": "Cantine", "amount": 30.0})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);

    set_role(&router, &db, &owner_cookie, &group_id, email, "standard").await;
    let list = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&list, StatusCode::NOT_FOUND);

    let export = call(
        &router,
        Method::GET,
        "/account/export",
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&export, StatusCode::OK);
    let doc = json_body(export).await;
    let exported = doc["budget_entries"].as_array().unwrap();
    assert_eq!(exported.len(), 1, "{exported:?}");
    assert_eq!(exported[0]["name"], "Cantine");
    assert_eq!(exported[0]["amount"], 30.0);
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

    let before = paris_day(Utc::now());
    let set_price = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items/{item_id}/price"),
        Some(&owner_cookie),
        Some(serde_json::json!({"amount": 2.35})),
    )
    .await;
    let after = paris_day(Utc::now());
    assert_status(&set_price, StatusCode::CREATED);
    let entry = json_body(set_price).await;
    assert_eq!(entry["name"], "Farine");
    assert_eq!(entry["amount"], 2.35);
    assert_eq!(entry["grocery_item_id"], item_id);
    assert_paris_today(&entry["spent_at"], before, after);

    let list = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_eq!(
        json_body(list).await["entries"].as_array().unwrap().len(),
        1
    );
}

/// AC: retrying/double-tapping "set price" on the same grocery item upserts
/// the price instead of creating a second, double-counted budget entry.
#[sqlx::test]
async fn setting_price_twice_on_same_item_is_idempotent(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "budget-price-retry@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create_item = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Lait"})),
    )
    .await;
    let item_id = json_body(create_item).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let first = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items/{item_id}/price"),
        Some(&owner_cookie),
        Some(serde_json::json!({"amount": 2.35})),
    )
    .await;
    assert_status(&first, StatusCode::CREATED);
    let first_id = json_body(first).await["id"].as_str().unwrap().to_string();

    let retry = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items/{item_id}/price"),
        Some(&owner_cookie),
        Some(serde_json::json!({"amount": 2.50})),
    )
    .await;
    assert_status(&retry, StatusCode::CREATED);
    let retry_body = json_body(retry).await;
    assert_eq!(retry_body["id"], first_id, "retry must update the same row");
    assert_eq!(retry_body["amount"], 2.50);

    let list = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_eq!(
        json_body(list).await["entries"].as_array().unwrap().len(),
        1,
        "double-tap must not double-count spend"
    );
}

/// AC: manually linking a second budget entry to an already-priced grocery
/// item is rejected instead of silently double-counting spend.
#[sqlx::test]
async fn manual_entry_on_already_priced_item_is_rejected(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "budget-conflict@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create_item = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Beurre"})),
    )
    .await;
    let item_id = json_body(create_item).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/grocery-items/{item_id}/price"),
        Some(&owner_cookie),
        Some(serde_json::json!({"amount": 2.35})),
    )
    .await;

    let duplicate = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Beurre", "amount": 2.35, "grocery_item_id": item_id})),
    )
    .await;
    assert_status(&duplicate, StatusCode::CONFLICT);
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

// -- Categories and caps (#532) ----------------------------------------------

/// An owner and an admin of a fresh group, logged in: (owner, admin, group).
async fn owner_and_admin(
    router: &axum::Router,
    db: &PgPool,
    prefix: &str,
) -> (String, String, String) {
    let owner_email = format!("{prefix}-owner@example.test");
    let admin_email = format!("{prefix}-admin@example.test");
    let owner = register_verify_login(router, db, &owner_email, "owner-password1").await;
    let admin = register_verify_login(router, db, &admin_email, "admin-password1").await;
    let group_id = create_group(router, &owner, "Foyer").await;
    join_as_standard(router, &owner, &admin, &group_id).await;
    set_role(router, db, &owner, &group_id, &admin_email, "admin").await;
    (owner, admin, group_id)
}

async fn create_category(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    body: serde_json::Value,
) -> String {
    let res = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/budget-categories"),
        Some(cookie),
        Some(body),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

async fn create_entry(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    body: serde_json::Value,
) -> serde_json::Value {
    let res = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/budget-entries"),
        Some(cookie),
        Some(body),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await
}

/// AC #532: an admin creates, lists, edits and deletes the family's
/// categories, and reads and sets its monthly cap.
#[sqlx::test]
async fn an_admin_runs_the_categories_and_the_family_cap(db: PgPool) {
    let router = test_router(db.clone());
    let (_owner, admin, group_id) = owner_and_admin(&router, &db, "cat-crud").await;
    let categories = format!("/groups/{group_id}/budget-categories");
    let settings = format!("/groups/{group_id}/budget-settings");

    let create = call(
        &router,
        Method::POST,
        &categories,
        Some(&admin),
        Some(serde_json::json!({"name": "  Loisirs ", "color": "ochre", "monthly_cap": 80.0})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let loisirs = json_body(create).await;
    assert_eq!(loisirs["name"], "Loisirs", "the name is trimmed");
    assert_eq!(loisirs["color"], "ochre");
    assert_eq!(loisirs["monthly_cap"], 80.0);
    assert_eq!(loisirs["group_id"], group_id.as_str());
    let loisirs_id = loisirs["id"].as_str().unwrap().to_string();
    create_category(
        &router,
        &admin,
        &group_id,
        serde_json::json!({"name": "alimentation", "color": "blue"}),
    )
    .await;

    // Listed by name, whatever the case; a category without cap says null.
    let list = json_body(call(&router, Method::GET, &categories, Some(&admin), None).await).await;
    let list = list["categories"].as_array().unwrap();
    let names: Vec<&str> = list.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["alimentation", "Loisirs"]);
    assert!(list[0]["monthly_cap"].is_null());

    // A PATCH changes what it names and keeps the rest; `null` removes the cap.
    let category = format!("{categories}/{loisirs_id}");
    let rename = call(
        &router,
        Method::PATCH,
        &category,
        Some(&admin),
        Some(serde_json::json!({"name": "Sorties"})),
    )
    .await;
    assert_status(&rename, StatusCode::OK);
    let renamed = json_body(rename).await;
    assert_eq!(renamed["name"], "Sorties");
    assert_eq!(renamed["color"], "ochre");
    assert_eq!(renamed["monthly_cap"], 80.0);
    let uncap = call(
        &router,
        Method::PATCH,
        &category,
        Some(&admin),
        Some(serde_json::json!({"monthly_cap": null, "color": "green"})),
    )
    .await;
    assert_status(&uncap, StatusCode::OK);
    let uncapped = json_body(uncap).await;
    assert!(uncapped["monthly_cap"].is_null());
    assert_eq!(uncapped["color"], "green");
    assert_eq!(uncapped["name"], "Sorties");

    let delete = call(&router, Method::DELETE, &category, Some(&admin), None).await;
    assert_status(&delete, StatusCode::NO_CONTENT);
    let again = call(&router, Method::DELETE, &category, Some(&admin), None).await;
    assert_status(&again, StatusCode::NOT_FOUND);
    let list = json_body(call(&router, Method::GET, &categories, Some(&admin), None).await).await;
    assert_eq!(list["categories"].as_array().unwrap().len(), 1);

    // The family cap: none at first, set, then removed.
    let get = call(&router, Method::GET, &settings, Some(&admin), None).await;
    assert_status(&get, StatusCode::OK);
    assert!(json_body(get).await["monthly_cap"].is_null());
    let put = call(
        &router,
        Method::PUT,
        &settings,
        Some(&admin),
        Some(serde_json::json!({"monthly_cap": 1200.5})),
    )
    .await;
    assert_status(&put, StatusCode::OK);
    assert_eq!(json_body(put).await["monthly_cap"], 1200.5);
    let get = call(&router, Method::GET, &settings, Some(&admin), None).await;
    assert_eq!(json_body(get).await["monthly_cap"], 1200.5);
    let clear = call(
        &router,
        Method::PUT,
        &settings,
        Some(&admin),
        Some(serde_json::json!({"monthly_cap": null})),
    )
    .await;
    assert_status(&clear, StatusCode::OK);
    assert!(json_body(clear).await["monthly_cap"].is_null());
}

/// AC #532 and the user's decision of 2026-10-29: a standard member gets 404
/// on every category and cap route — and on the entry and price routes —
/// before their body is looked at: a malformed body is a 404 too, never the
/// 400 that would tell them the route exists. Nothing they send lands.
#[sqlx::test]
async fn a_standard_member_gets_404_on_categories_and_caps_whatever_the_body(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(
        &router,
        &db,
        "cat-std-owner@example.test",
        "owner-password1",
    )
    .await;
    let member = register_verify_login(
        &router,
        &db,
        "cat-std-member@example.test",
        "member-password1",
    )
    .await;
    let group_id = create_group(&router, &owner, "Foyer").await;
    join_as_standard(&router, &owner, &member, &group_id).await;
    let category_id = create_category(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Enfants", "color": "violet", "monthly_cap": 50.0}),
    )
    .await;
    let item_id = checked_grocery_item(&router, &member, &group_id, "Pain").await;
    let entry_id = create_entry(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Courses", "amount": 12.0}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let categories = format!("/groups/{group_id}/budget-categories");
    let category = format!("{categories}/{category_id}");
    let settings = format!("/groups/{group_id}/budget-settings");
    let switches = format!("/groups/{group_id}/budget-alert-settings");
    let entries = format!("/groups/{group_id}/budget-entries");
    let entry = format!("{entries}/{entry_id}");
    let price = format!("/groups/{group_id}/grocery-items/{item_id}/price");
    // A body the extractor cannot read at all — missing fields, a wrong
    // type, not an object — is a 404 too: the bar comes before the body.
    let unreadable = [
        serde_json::json!({}),
        serde_json::json!({"name": 5, "color": [], "amount": "abc", "monthly_cap": "abc",
                           "global_cap_alerts": "abc"}),
        serde_json::json!("abc"),
    ];
    let body_routes = [
        ("create category", Method::POST, categories.clone()),
        ("update category", Method::PATCH, category.clone()),
        ("set cap", Method::PUT, settings.clone()),
        ("set alert switches", Method::PATCH, switches.clone()),
        ("create entry", Method::POST, entries.clone()),
        ("update entry", Method::PATCH, entry.clone()),
        ("price", Method::POST, price.clone()),
    ];
    for (what, method, uri) in &body_routes {
        for body in &unreadable {
            let res = call(
                &router,
                method.clone(),
                uri,
                Some(&member),
                Some(body.clone()),
            )
            .await;
            assert_eq!(
                res.status(),
                StatusCode::NOT_FOUND,
                "a standard member's {what} with body {body} must be a 404"
            );
        }
    }
    // Past the bar, the owner still gets the extractor's own answer.
    let res = call(
        &router,
        Method::POST,
        &categories,
        Some(&owner),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let res = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/budget-entries/summary?month=2026-01&month=2026-02"),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_body(res).await["error"], "invalid_month");

    let requests = [
        ("list categories", Method::GET, categories.clone(), None),
        (
            "create category",
            Method::POST,
            categories.clone(),
            Some(serde_json::json!({"name": "Bonbons", "color": "blue"})),
        ),
        (
            "create category, invalid",
            Method::POST,
            categories.clone(),
            Some(serde_json::json!({"name": " ", "color": "#ff0000", "monthly_cap": -1})),
        ),
        (
            "update category",
            Method::PATCH,
            category.clone(),
            Some(serde_json::json!({"monthly_cap": 999.0})),
        ),
        (
            "update category, invalid",
            Method::PATCH,
            category.clone(),
            Some(serde_json::json!({"name": ""})),
        ),
        ("delete category", Method::DELETE, category.clone(), None),
        ("read cap", Method::GET, settings.clone(), None),
        (
            "set cap",
            Method::PUT,
            settings.clone(),
            Some(serde_json::json!({"monthly_cap": 1.0})),
        ),
        (
            "set cap, invalid",
            Method::PUT,
            settings.clone(),
            Some(serde_json::json!({"monthly_cap": 0})),
        ),
        (
            "summary, unreadable query",
            Method::GET,
            format!("/groups/{group_id}/budget-entries/summary?month=2026-01&month=2026-02"),
            None,
        ),
        ("read alert switches", Method::GET, switches.clone(), None),
        (
            "set alert switches",
            Method::PATCH,
            switches.clone(),
            Some(serde_json::json!({"global_cap_alerts": false, "category_cap_alerts": false})),
        ),
        (
            "summary, invalid month",
            Method::GET,
            format!("/groups/{group_id}/budget-entries/summary?month=nope"),
            None,
        ),
        (
            "create entry, invalid",
            Method::POST,
            format!("/groups/{group_id}/budget-entries"),
            Some(serde_json::json!({"name": "", "amount": -1.0})),
        ),
        (
            "price, invalid",
            Method::POST,
            format!("/groups/{group_id}/grocery-items/{item_id}/price"),
            Some(serde_json::json!({"amount": -3.0})),
        ),
    ];
    for (what, method, uri, body) in requests {
        let res = call(&router, method, &uri, Some(&member), body).await;
        assert_eq!(
            res.status(),
            StatusCode::NOT_FOUND,
            "a standard member's {what} must be a 404"
        );
    }

    let list = json_body(call(&router, Method::GET, &categories, Some(&owner), None).await).await;
    let list = list["categories"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0]["monthly_cap"], 50.0);
    let cap = call(&router, Method::GET, &settings, Some(&owner), None).await;
    assert!(json_body(cap).await["monthly_cap"].is_null());
    let res = call(&router, Method::GET, &switches, Some(&owner), None).await;
    assert_eq!(
        json_body(res).await,
        serde_json::json!({"global_cap_alerts": true, "category_cap_alerts": true})
    );
}

/// AC #532: the bounds of the user's decision of 2026-10-29 — a trimmed name
/// of 1 to 40 characters, unique in the family whatever its case, a colour
/// from the closed list, caps above zero, 30 categories at most.
#[sqlx::test]
async fn categories_and_caps_are_bounded(db: PgPool) {
    let router = test_router(db.clone());
    let owner =
        register_verify_login(&router, &db, "cat-bounds@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner, "Foyer").await;
    let categories = format!("/groups/{group_id}/budget-categories");
    let first = create_category(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Santé", "color": "green"}),
    )
    .await;

    let refused = [
        (
            serde_json::json!({"name": "  ", "color": "blue"}),
            StatusCode::BAD_REQUEST,
            "name_required",
        ),
        (
            serde_json::json!({"name": "é".repeat(41), "color": "blue"}),
            StatusCode::BAD_REQUEST,
            "name_too_long",
        ),
        (
            serde_json::json!({"name": "Rouge", "color": "#a8342a"}),
            StatusCode::BAD_REQUEST,
            "invalid_color",
        ),
        (
            serde_json::json!({"name": "Rouge", "color": "red"}),
            StatusCode::BAD_REQUEST,
            "invalid_color",
        ),
        (
            serde_json::json!({"name": "Rouge", "color": "blue", "monthly_cap": 0}),
            StatusCode::BAD_REQUEST,
            "cap_must_be_positive",
        ),
        (
            serde_json::json!({"name": "SANTÉ", "color": "blue"}),
            StatusCode::CONFLICT,
            "category_name_taken",
        ),
    ];
    for (body, status, code) in refused {
        let res = call(
            &router,
            Method::POST,
            &categories,
            Some(&owner),
            Some(body.clone()),
        )
        .await;
        assert_eq!(res.status(), status, "{body}");
        assert_eq!(json_body(res).await["error"], code, "{body}");
    }
    let forty = create_category(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "é".repeat(40), "color": "orange"}),
    )
    .await;

    // A rename onto another category's name, whatever its case, is refused
    // too; onto its own name in another case, it is not.
    let rename = call(
        &router,
        Method::PATCH,
        &format!("{categories}/{forty}"),
        Some(&owner),
        Some(serde_json::json!({"name": "santé"})),
    )
    .await;
    assert_status(&rename, StatusCode::CONFLICT);
    let recase = call(
        &router,
        Method::PATCH,
        &format!("{categories}/{first}"),
        Some(&owner),
        Some(serde_json::json!({"name": "SANTÉ"})),
    )
    .await;
    assert_status(&recase, StatusCode::OK);
    for (body, code) in [
        (
            serde_json::json!({"monthly_cap": -5.0}),
            "cap_must_be_positive",
        ),
        (serde_json::json!({"color": "Blue"}), "invalid_color"),
        (serde_json::json!({"name": " "}), "name_required"),
    ] {
        let res = call(
            &router,
            Method::PATCH,
            &format!("{categories}/{first}"),
            Some(&owner),
            Some(body.clone()),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(json_body(res).await["error"], code, "{body}");
    }
    let family_cap = call(
        &router,
        Method::PUT,
        &format!("/groups/{group_id}/budget-settings"),
        Some(&owner),
        Some(serde_json::json!({"monthly_cap": -1.0})),
    )
    .await;
    assert_status(&family_cap, StatusCode::BAD_REQUEST);

    // 30 categories, not 31.
    for n in 3..=30 {
        create_category(
            &router,
            &owner,
            &group_id,
            serde_json::json!({"name": format!("Catégorie {n}"), "color": "blue"}),
        )
        .await;
    }
    let thirty_first = call(
        &router,
        Method::POST,
        &categories,
        Some(&owner),
        Some(serde_json::json!({"name": "Une de trop", "color": "blue"})),
    )
    .await;
    assert_status(&thirty_first, StatusCode::CONFLICT);
    assert_eq!(
        json_body(thirty_first).await["error"],
        "category_limit_reached"
    );
    let list = json_body(call(&router, Method::GET, &categories, Some(&owner), None).await).await;
    assert_eq!(list["categories"].as_array().unwrap().len(), 30);
}

/// AC #532: the 30-category bound holds under simultaneous creations — with
/// 25 categories on file, 15 creations at once leave exactly 30, the other
/// 10 refused `category_limit_reached`. Without the family's lock in
/// `create_budget_category`, several would read 25 together and pass.
#[sqlx::test]
async fn simultaneous_creations_stop_at_thirty_categories(db: PgPool) {
    let router = test_router(db.clone());
    let (owner, _admin, group_id) = owner_and_admin(&router, &db, "cat-race").await;
    let categories = format!("/groups/{group_id}/budget-categories");
    for n in 1..=25 {
        create_category(
            &router,
            &owner,
            &group_id,
            serde_json::json!({"name": format!("Avant {n}"), "color": "green"}),
        )
        .await;
    }

    let mut creations = tokio::task::JoinSet::new();
    for n in 1..=15 {
        let (router, owner, uri) = (router.clone(), owner.clone(), categories.clone());
        creations.spawn(async move {
            let body = serde_json::json!({"name": format!("Course {n}"), "color": "blue"});
            let res = call(&router, Method::POST, &uri, Some(&owner), Some(body)).await;
            let status = res.status();
            (status, json_body(res).await)
        });
    }
    let (mut created, mut refused) = (0, 0);
    while let Some(outcome) = creations.join_next().await {
        let (status, body) = outcome.unwrap();
        match status {
            StatusCode::CREATED => created += 1,
            StatusCode::CONFLICT => {
                assert_eq!(body["error"], "category_limit_reached", "{body}");
                refused += 1;
            }
            other => panic!("unexpected {other}: {body}"),
        }
    }
    assert_eq!((created, refused), (5, 10));

    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM budget_categories WHERE group_id = $1::uuid")
            .bind(&group_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(rows, 30);
}

/// AC #532: an entry takes a category at creation and on edit (`null` moves
/// it back to « Sans catégorie »), and deleting a category leaves its
/// entries in place with `category_id = NULL`.
#[sqlx::test]
async fn deleting_a_category_leaves_its_entries_uncategorized(db: PgPool) {
    let router = test_router(db.clone());
    let owner =
        register_verify_login(&router, &db, "cat-delete@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner, "Foyer").await;
    let food = create_category(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Alimentation", "color": "blue"}),
    )
    .await;
    let home = create_category(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Maison", "color": "orange"}),
    )
    .await;

    let entry = create_entry(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Supermarché", "amount": 40.0, "category_id": food}),
    )
    .await;
    assert_eq!(entry["category_id"], food.as_str());
    let entry_uri = format!(
        "/groups/{group_id}/budget-entries/{}",
        entry["id"].as_str().unwrap()
    );
    let plain = create_entry(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Divers", "amount": 1.0}),
    )
    .await;
    assert!(plain["category_id"].is_null());

    // Edit: another category, then none, then back; a PATCH that does not
    // name it keeps it.
    for (body, expected) in [
        (
            serde_json::json!({"category_id": home}),
            Some(home.as_str()),
        ),
        (serde_json::json!({"amount": 41.0}), Some(home.as_str())),
        (serde_json::json!({"category_id": null}), None),
        (
            serde_json::json!({"category_id": food}),
            Some(food.as_str()),
        ),
    ] {
        let res = call(
            &router,
            Method::PATCH,
            &entry_uri,
            Some(&owner),
            Some(body.clone()),
        )
        .await;
        assert_status(&res, StatusCode::OK);
        let got = json_body(res).await;
        assert_eq!(got["category_id"].as_str(), expected, "{body}");
    }
    // An unknown category is the 404 of any missing referenced resource.
    let unknown = uuid::Uuid::new_v4();
    let res = call(
        &router,
        Method::PATCH,
        &entry_uri,
        Some(&owner),
        Some(serde_json::json!({"category_id": unknown})),
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/budget-categories/{food}"),
        Some(&owner),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::NO_CONTENT);
    let after = json_body(call(&router, Method::GET, &entry_uri, Some(&owner), None).await).await;
    assert!(after["category_id"].is_null(), "{after}");
    assert_eq!(after["amount"], 41.0, "the entry itself is untouched");
    assert_eq!(after["name"], "Supermarché");
    let stored: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT category_id FROM budget_entries WHERE id = $1")
            .bind(uuid::Uuid::parse_str(entry["id"].as_str().unwrap()).unwrap())
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(stored, None);
}

/// AC #532: the summary reports the asked month against the caps — the
/// month's total and the family's cap, and per category its total, its cap,
/// the share spent and the overrun; no cap, no percentage.
#[sqlx::test]
async fn the_summary_reflects_the_caps(db: PgPool) {
    let router = test_router(db.clone());
    let owner =
        register_verify_login(&router, &db, "cat-summary@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner, "Foyer").await;
    let put = call(
        &router,
        Method::PUT,
        &format!("/groups/{group_id}/budget-settings"),
        Some(&owner),
        Some(serde_json::json!({"monthly_cap": 100.0})),
    )
    .await;
    assert_status(&put, StatusCode::OK);
    let leisure = create_category(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Loisirs", "color": "ochre", "monthly_cap": 50.0}),
    )
    .await;
    let health = create_category(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Santé", "color": "green"}),
    )
    .await;
    let food = create_category(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Alimentation", "color": "blue", "monthly_cap": 200.0}),
    )
    .await;
    for (name, amount, day, category) in [
        ("Cinéma", 30.0, "2026-10-03", Some(&leisure)),
        ("Concert", 22.0, "2026-10-31", Some(&leisure)),
        ("Pharmacie", 10.0, "2026-10-10", Some(&health)),
        ("Divers", 5.5, "2026-10-12", None),
        ("Bowling", 7.0, "2026-09-30", Some(&leisure)),
        ("Musée", 9.0, "2026-11-01", Some(&leisure)),
    ] {
        create_entry(
            &router,
            &owner,
            &group_id,
            serde_json::json!({"name": name, "amount": amount, "spent_at": day, "category_id": category}),
        )
        .await;
    }

    let summary = |month: &str| {
        let uri = format!("/groups/{group_id}/budget-entries/summary?month={month}");
        let router = router.clone();
        let owner = owner.clone();
        async move {
            let res = call(&router, Method::GET, &uri, Some(&owner), None).await;
            assert_status(&res, StatusCode::OK);
            json_body(res).await
        }
    };

    let october = summary("2026-10").await;
    let month = &october["month"];
    assert_eq!(month["period"], "2026-10-01");
    assert_eq!(month["total"], 67.5);
    assert_eq!(month["monthly_cap"], 100.0);
    assert_eq!(month["percent"], 67);
    assert_eq!(month["over_cap"], false);
    assert_eq!(month["uncategorized"], 5.5);
    let cats = month["categories"].as_array().unwrap();
    let names: Vec<&str> = cats.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        ["Alimentation", "Loisirs", "Santé"],
        "every category, by name"
    );
    // Nothing spent under a cap: 0 %, listed all the same.
    assert_eq!(cats[0]["id"], food.as_str());
    assert_eq!(cats[0]["total"], 0.0);
    assert_eq!(cats[0]["percent"], 0);
    assert_eq!(cats[0]["over_cap"], false);
    // 52 € of 50 €: 104 %, over.
    assert_eq!(cats[1]["id"], leisure.as_str());
    assert_eq!(cats[1]["color"], "ochre");
    assert_eq!(cats[1]["total"], 52.0);
    assert_eq!(cats[1]["monthly_cap"], 50.0);
    assert_eq!(cats[1]["percent"], 104);
    assert_eq!(cats[1]["over_cap"], true);
    // No cap: no percentage, no overrun.
    assert_eq!(cats[2]["total"], 10.0);
    assert!(cats[2]["monthly_cap"].is_null());
    assert!(cats[2]["percent"].is_null());
    assert_eq!(cats[2]["over_cap"], false);
    // The per-month history is still there.
    assert_eq!(october["periods"].as_array().unwrap().len(), 3);

    // Another month, under the family cap's overrun this time.
    let put = call(
        &router,
        Method::PUT,
        &format!("/groups/{group_id}/budget-settings"),
        Some(&owner),
        Some(serde_json::json!({"monthly_cap": 6.0})),
    )
    .await;
    assert_status(&put, StatusCode::OK);
    let september = summary("2026-09").await;
    let month = &september["month"];
    assert_eq!(month["total"], 7.0);
    assert_eq!(month["percent"], 116);
    assert_eq!(month["over_cap"], true);
    assert_eq!(month["uncategorized"], 0.0);

    // Without a family cap, no percentage.
    call(
        &router,
        Method::PUT,
        &format!("/groups/{group_id}/budget-settings"),
        Some(&owner),
        Some(serde_json::json!({})),
    )
    .await;
    let november = summary("2026-11").await;
    assert!(november["month"]["monthly_cap"].is_null());
    assert!(november["month"]["percent"].is_null());
    assert_eq!(november["month"]["over_cap"], false);
    assert_eq!(november["month"]["total"], 9.0);

    for bad in ["2026-13", "2026-10-01", "oct"] {
        let res = call(
            &router,
            Method::GET,
            &format!("/groups/{group_id}/budget-entries/summary?month={bad}"),
            Some(&owner),
            None,
        )
        .await;
        assert_status(&res, StatusCode::BAD_REQUEST);
    }

    // Left out, the month is the current one in Paris.
    let res = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/budget-entries/summary"),
        Some(&owner),
        None,
    )
    .await;
    let period: NaiveDate = json_body(res).await["month"]["period"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let today = paris_day(Utc::now());
    assert_eq!(
        (period.year(), period.month(), period.day()),
        (today.year(), today.month(), 1)
    );
}

/// AC #532: another family sees nothing of these categories or this cap —
/// not in its own lists, not by id, not by filing one of its entries under
/// them.
#[sqlx::test]
async fn another_family_sees_nothing_of_the_categories(db: PgPool) {
    let router = test_router(db.clone());
    let owner_a =
        register_verify_login(&router, &db, "cat-rls-a@example.test", "owner-password1").await;
    let owner_b =
        register_verify_login(&router, &db, "cat-rls-b@example.test", "owner-password1").await;
    let group_a = create_group(&router, &owner_a, "A").await;
    let group_b = create_group(&router, &owner_b, "B").await;
    let category_a = create_category(
        &router,
        &owner_a,
        &group_a,
        serde_json::json!({"name": "Secret", "color": "violet", "monthly_cap": 10.0}),
    )
    .await;
    call(
        &router,
        Method::PUT,
        &format!("/groups/{group_a}/budget-settings"),
        Some(&owner_a),
        Some(serde_json::json!({"monthly_cap": 900.0})),
    )
    .await;

    // B's own views are empty.
    let list = json_body(
        call(
            &router,
            Method::GET,
            &format!("/groups/{group_b}/budget-categories"),
            Some(&owner_b),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(list["categories"], serde_json::json!([]));
    let cap = call(
        &router,
        Method::GET,
        &format!("/groups/{group_b}/budget-settings"),
        Some(&owner_b),
        None,
    )
    .await;
    assert!(json_body(cap).await["monthly_cap"].is_null());
    let summary = call(
        &router,
        Method::GET,
        &format!("/groups/{group_b}/budget-entries/summary"),
        Some(&owner_b),
        None,
    )
    .await;
    let summary = json_body(summary).await;
    assert_eq!(summary["month"]["categories"], serde_json::json!([]));
    assert!(summary["month"]["monthly_cap"].is_null());

    // A's category through B's group: unknown.
    let through_b = format!("/groups/{group_b}/budget-categories/{category_a}");
    let patch = call(
        &router,
        Method::PATCH,
        &through_b,
        Some(&owner_b),
        Some(serde_json::json!({"name": "Pris"})),
    )
    .await;
    assert_status(&patch, StatusCode::NOT_FOUND);
    let delete = call(&router, Method::DELETE, &through_b, Some(&owner_b), None).await;
    assert_status(&delete, StatusCode::NOT_FOUND);
    let entry = call(
        &router,
        Method::POST,
        &format!("/groups/{group_b}/budget-entries"),
        Some(&owner_b),
        Some(serde_json::json!({"name": "Pain", "amount": 1.0, "category_id": category_a})),
    )
    .await;
    assert_status(&entry, StatusCode::NOT_FOUND);

    // A's group itself is closed to B, as every group route.
    let res = call(
        &router,
        Method::GET,
        &format!("/groups/{group_a}/budget-categories"),
        Some(&owner_b),
        None,
    )
    .await;
    assert_status(&res, StatusCode::FORBIDDEN);

    // And A's category is as A left it.
    let list = json_body(
        call(
            &router,
            Method::GET,
            &format!("/groups/{group_a}/budget-categories"),
            Some(&owner_a),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(list["categories"][0]["name"], "Secret");
}

/// AC #532: the RGPD export hands an owner or an admin the family's budget
/// settings — its cap and its categories — and each entry's category; a
/// standard member of the same family gets no `budget_settings` at all.
#[sqlx::test]
async fn the_export_carries_the_categories_to_those_who_reach_the_budget(db: PgPool) {
    let router = test_router(db.clone());
    let (owner, admin, group_id) = owner_and_admin(&router, &db, "cat-export").await;
    let member = register_verify_login(
        &router,
        &db,
        "cat-export-member@example.test",
        "member-password1",
    )
    .await;
    join_as_standard(&router, &owner, &member, &group_id).await;
    let food = create_category(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Alimentation", "color": "blue", "monthly_cap": 300.0}),
    )
    .await;
    call(
        &router,
        Method::PUT,
        &format!("/groups/{group_id}/budget-settings"),
        Some(&owner),
        Some(serde_json::json!({"monthly_cap": 750.0})),
    )
    .await;
    create_entry(
        &router,
        &admin,
        &group_id,
        serde_json::json!({"name": "Marché", "amount": 12.0, "category_id": food}),
    )
    .await;

    let export = |cookie: String| {
        let router = router.clone();
        async move {
            let res = call(&router, Method::GET, "/account/export", Some(&cookie), None).await;
            assert_status(&res, StatusCode::OK);
            json_body(res).await
        }
    };
    let doc = export(admin.clone()).await;
    let settings = doc["budget_settings"].as_array().unwrap();
    assert_eq!(settings.len(), 1, "{settings:?}");
    assert_eq!(settings[0]["group_id"], group_id.as_str());
    assert_eq!(settings[0]["monthly_cap"], 750.0);
    assert_eq!(settings[0]["categories"][0]["id"], food.as_str());
    assert_eq!(settings[0]["categories"][0]["name"], "Alimentation");
    assert_eq!(settings[0]["categories"][0]["color"], "blue");
    assert_eq!(settings[0]["categories"][0]["monthly_cap"], 300.0);
    assert_eq!(doc["budget_entries"][0]["category_id"], food.as_str());

    let doc = export(owner).await;
    assert_eq!(doc["budget_settings"].as_array().unwrap().len(), 1);
    let doc = export(member).await;
    assert_eq!(doc["budget_settings"], serde_json::json!([]));
}

// -- Cap alerts (#536) ---------------------------------------------------------

/// The bodies of the budget alerts `cookie`'s account holds, sorted: the
/// alerts one expense sends share their `created_at` (one transaction), so
/// the list's order between them is the ids'.
async fn budget_alerts(router: &axum::Router, cookie: &str) -> Vec<String> {
    let res = call(
        router,
        Method::GET,
        "/notifications?kind=budget_alert",
        Some(cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let page = json_body(res).await;
    let mut bodies: Vec<String> = page["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["body"].as_str().unwrap().to_string())
        .collect();
    bodies.sort();
    bodies
}

async fn patch_entry(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    entry_id: &str,
    body: serde_json::Value,
) {
    let res = call(
        router,
        Method::PATCH,
        &format!("/groups/{group_id}/budget-entries/{entry_id}"),
        Some(cookie),
        Some(body),
    )
    .await;
    assert_status(&res, StatusCode::OK);
}

const NEAR: &str = "Les dépenses du mois atteignent 80 % du budget de la famille.";
/// Sorts before `NEAR` (`1` before `8`).
const FULL: &str = "Les dépenses du mois atteignent 100 % du budget de la famille.";

/// AC #536: an expense that reaches 80 % of the family's cap, one that
/// reaches the cap exactly, then one that exceeds its category's cap, alert
/// the owner and the admins —
/// the author of the expense included — once per threshold and per month:
/// lowering an expense alerts nothing, and raising it again past the same
/// caps alerts nothing more. A standard member never gets one.
#[sqlx::test]
async fn cap_alerts_go_once_per_month_to_owners_and_admins_only(db: PgPool) {
    let router = test_router(db.clone());
    let (owner, admin, group_id) = owner_and_admin(&router, &db, "alert-once").await;
    let member = register_verify_login(
        &router,
        &db,
        "alert-once-member@example.test",
        "member-password1",
    )
    .await;
    join_as_standard(&router, &owner, &member, &group_id).await;
    let res = call(
        &router,
        Method::PUT,
        &format!("/groups/{group_id}/budget-settings"),
        Some(&owner),
        Some(serde_json::json!({"monthly_cap": 100.0})),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let loisirs = create_category(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Loisirs", "color": "ochre", "monthly_cap": 30.0}),
    )
    .await;

    // 79,99 € of 100 €: no alert yet.
    let first = create_entry(
        &router,
        &admin,
        &group_id,
        serde_json::json!({"name": "Courses", "amount": 79.99}),
    )
    .await;
    assert!(budget_alerts(&router, &owner).await.is_empty());
    // One cent more: 80 % exactly.
    let first_id = first["id"].as_str().unwrap();
    patch_entry(
        &router,
        &admin,
        &group_id,
        first_id,
        serde_json::json!({"amount": 80.0}),
    )
    .await;
    assert_eq!(budget_alerts(&router, &owner).await, [NEAR]);
    assert_eq!(
        budget_alerts(&router, &admin).await,
        [NEAR],
        "the author of the expense gets it too"
    );

    // 20 € more: the family's 100 € exactly — reaching it alerts.
    create_entry(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Pharmacie", "amount": 20.0}),
    )
    .await;
    assert_eq!(budget_alerts(&router, &admin).await, [FULL, NEAR]);

    // 30 € of Loisirs: its cap reached, not exceeded — nothing.
    let cinema = create_entry(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Cinéma", "amount": 30.0, "category_id": loisirs}),
    )
    .await;
    assert_eq!(budget_alerts(&router, &admin).await, [FULL, NEAR]);
    // 31 €: past it.
    let cinema_id = cinema["id"].as_str().unwrap();
    patch_entry(
        &router,
        &owner,
        &group_id,
        cinema_id,
        serde_json::json!({"amount": 31.0}),
    )
    .await;
    let category = "Les dépenses du mois dépassent le plafond de « Loisirs ».";
    for cookie in [&owner, &admin] {
        assert_eq!(budget_alerts(&router, cookie).await, [FULL, NEAR, category]);
    }

    // Lowered under every threshold: nothing. Raised past them again in the
    // same month: nothing more.
    patch_entry(
        &router,
        &owner,
        &group_id,
        cinema_id,
        serde_json::json!({"amount": 1.0}),
    )
    .await;
    patch_entry(
        &router,
        &admin,
        &group_id,
        first_id,
        serde_json::json!({"amount": 10.0}),
    )
    .await;
    patch_entry(
        &router,
        &owner,
        &group_id,
        cinema_id,
        serde_json::json!({"amount": 95.0}),
    )
    .await;
    for cookie in [&owner, &admin] {
        assert_eq!(budget_alerts(&router, cookie).await, [FULL, NEAR, category]);
    }
    assert!(budget_alerts(&router, &member).await.is_empty());
    let sent: i64 = sqlx::query_scalar("SELECT count(*) FROM budget_alerts")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(sent, 3);
}

/// AC #536: the two switches, both on by default, read and set by an owner
/// or an admin; a switch off silences its alerts, the other still alerts.
/// And an expense dated in another month than the current one alerts
/// nothing (user's decision of 2026-10-30), whatever it crosses there.
#[sqlx::test]
async fn the_switches_and_the_current_month_bound_the_alerts(db: PgPool) {
    let router = test_router(db.clone());
    let (owner, admin, group_id) = owner_and_admin(&router, &db, "alert-switch").await;
    let switches = format!("/groups/{group_id}/budget-alert-settings");

    let res = call(&router, Method::GET, &switches, Some(&admin), None).await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(
        json_body(res).await,
        serde_json::json!({"global_cap_alerts": true, "category_cap_alerts": true})
    );
    let res = call(
        &router,
        Method::PATCH,
        &switches,
        Some(&admin),
        Some(serde_json::json!({"global_cap_alerts": false})),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(
        json_body(res).await,
        serde_json::json!({"global_cap_alerts": false, "category_cap_alerts": true})
    );
    // An empty body keeps both.
    let res = call(
        &router,
        Method::PATCH,
        &switches,
        Some(&owner),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(
        json_body(res).await,
        serde_json::json!({"global_cap_alerts": false, "category_cap_alerts": true})
    );
    // Setting the family's cap afterwards keeps the switches.
    let res = call(
        &router,
        Method::PUT,
        &format!("/groups/{group_id}/budget-settings"),
        Some(&owner),
        Some(serde_json::json!({"monthly_cap": 100.0})),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let res = call(&router, Method::GET, &switches, Some(&owner), None).await;
    assert_eq!(json_body(res).await["global_cap_alerts"], false);

    let enfants = create_category(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Enfants", "color": "violet", "monthly_cap": 20.0}),
    )
    .await;
    // Last month, past both caps: nothing, whatever the switches.
    let today = paris_day(Utc::now());
    let last_month = today.with_day(1).unwrap().pred_opt().unwrap();
    create_entry(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Rentrée", "amount": 500.0,
                           "category_id": enfants, "spent_at": last_month}),
    )
    .await;
    assert!(budget_alerts(&router, &admin).await.is_empty());

    // This month, past both caps: the category's alert only.
    let entry = create_entry(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Cartable", "amount": 150.0, "category_id": enfants}),
    )
    .await;
    let category = "Les dépenses du mois dépassent le plafond de « Enfants ».";
    assert_eq!(budget_alerts(&router, &admin).await, [category]);

    // Moved to last month and back, the switches now swapped: the family's
    // alerts come, the category's (off) does not come again.
    let entry_id = entry["id"].as_str().unwrap();
    let res = call(
        &router,
        Method::PATCH,
        &switches,
        Some(&owner),
        Some(serde_json::json!({"global_cap_alerts": true, "category_cap_alerts": false})),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    patch_entry(
        &router,
        &owner,
        &group_id,
        entry_id,
        serde_json::json!({"spent_at": last_month}),
    )
    .await;
    assert_eq!(budget_alerts(&router, &admin).await, [category]);
    patch_entry(
        &router,
        &owner,
        &group_id,
        entry_id,
        serde_json::json!({"spent_at": today}),
    )
    .await;
    assert_eq!(budget_alerts(&router, &admin).await, [FULL, NEAR, category]);
}

/// AC #536: a price set on a checked grocery item is an expense like any
/// other — it alerts — and set again on the same item, it keeps its
/// entry's category, whose cap it may then cross.
#[sqlx::test]
async fn a_grocery_item_s_price_alerts_too(db: PgPool) {
    let router = test_router(db.clone());
    let (owner, admin, group_id) = owner_and_admin(&router, &db, "alert-price").await;
    let food = create_category(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Alimentation", "color": "blue", "monthly_cap": 10.0}),
    )
    .await;
    let item_id = checked_grocery_item(&router, &owner, &group_id, "Pain").await;
    let price = format!("/groups/{group_id}/grocery-items/{item_id}/price");
    let res = call(
        &router,
        Method::POST,
        &price,
        Some(&owner),
        Some(serde_json::json!({"amount": 2.0})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let entry_id = json_body(res).await["id"].as_str().unwrap().to_string();
    patch_entry(
        &router,
        &owner,
        &group_id,
        &entry_id,
        serde_json::json!({"category_id": food}),
    )
    .await;
    assert!(budget_alerts(&router, &admin).await.is_empty());
    let res = call(
        &router,
        Method::POST,
        &price,
        Some(&owner),
        Some(serde_json::json!({"amount": 12.0})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    assert_eq!(
        budget_alerts(&router, &admin).await,
        ["Les dépenses du mois dépassent le plafond de « Alimentation »."]
    );
}

/// The family's lock (`budget_settings … FOR UPDATE`): expenses written at
/// once read the month's totals one after the other, so 70 € then six
/// expenses of 2 € at once against a 100 € cap send the 80 % alert once —
/// never none because each read the others' writes uncommitted, never
/// twice. The entries table is held in SHARE mode meanwhile: the first
/// expense waits on it to insert, every other one on the family's lock.
#[sqlx::test]
async fn simultaneous_expenses_alert_once(db: PgPool) {
    let router = test_router_sized(db.clone(), 10);
    let (owner, admin, group_id) = owner_and_admin(&router, &db, "alert-race").await;
    let res = call(
        &router,
        Method::PUT,
        &format!("/groups/{group_id}/budget-settings"),
        Some(&owner),
        Some(serde_json::json!({"monthly_cap": 100.0})),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    create_entry(
        &router,
        &owner,
        &group_id,
        serde_json::json!({"name": "Base", "amount": 70.0}),
    )
    .await;

    let mut hold = db.begin().await.unwrap();
    sqlx::query("LOCK TABLE budget_entries IN SHARE MODE")
        .execute(&mut *hold)
        .await
        .unwrap();
    let uri = format!("/groups/{group_id}/budget-entries");
    let mut expenses = tokio::task::JoinSet::new();
    for n in 0..6 {
        let cookie = if n % 2 == 0 { owner.clone() } else { admin.clone() };
        let (router, uri) = (router.clone(), uri.clone());
        expenses.spawn(async move {
            let body = serde_json::json!({"name": format!("Dépense {n}"), "amount": 2.0});
            call(&router, Method::POST, &uri, Some(&cookie), Some(body))
                .await
                .status()
        });
    }
    assert_eq!(
        waiting_on_table(&db, 6, "budget_entries").await,
        1,
        "the expenses past the first wait on the family's lock"
    );
    hold.rollback().await.unwrap();
    while let Some(status) = expenses.join_next().await {
        assert_eq!(status.unwrap(), StatusCode::CREATED);
    }

    for cookie in [&owner, &admin] {
        assert_eq!(budget_alerts(&router, cookie).await, [NEAR]);
    }
    let sent: i64 = sqlx::query_scalar("SELECT count(*) FROM budget_alerts")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(sent, 1);
}
