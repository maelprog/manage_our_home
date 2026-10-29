mod common;

use axum::http::{Method, StatusCode};
use chrono::{NaiveDate, Utc};
use common::{assert_status, call, json_body, set_cookie, test_router};
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
