mod common;

use axum::http::{Method, StatusCode};
use chrono::{Duration, Utc};
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

/// AC: full CRUD lifecycle for a manually-entered stock item, scoped to a group.
#[sqlx::test]
async fn full_stock_item_lifecycle(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "stock-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Farine", "quantity": 2.0, "unit": "kg", "reorder_threshold": 0.5})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let item = json_body(create).await;
    let item_id = item["id"].as_str().unwrap().to_string();
    assert_eq!(item["name"], "Farine");
    assert_eq!(item["low_stock"], false);

    let get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&get, StatusCode::OK);

    let update = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"quantity": 0.3})),
    )
    .await;
    assert_status(&update, StatusCode::OK);
    let updated = json_body(update).await;
    assert_eq!(updated["quantity"], 0.3);
    assert_eq!(
        updated["low_stock"], true,
        "quantity at/below threshold must report low_stock"
    );

    let list = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&list, StatusCode::OK);
    assert_eq!(json_body(list).await["items"].as_array().unwrap().len(), 1);

    let low_stock_list = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/stock-items?low_stock=true"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&low_stock_list, StatusCode::OK);
    assert_eq!(
        json_body(low_stock_list).await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::NO_CONTENT);

    let get_after_delete = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&get_after_delete, StatusCode::NOT_FOUND);
}

/// AC: a non-member of the group cannot read or write its stock items, even
/// with a valid session for another account.
#[sqlx::test]
async fn non_member_cannot_access_group_stock_items(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "stock-owner2@example.test", "owner-password1").await;
    let outsider_cookie = register_verify_login(
        &router,
        &db,
        "stock-outsider@example.test",
        "outsider-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Riz", "quantity": 1.0})),
    )
    .await;
    let item_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let outsider_get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&outsider_cookie),
        None,
    )
    .await;
    assert_status(&outsider_get, StatusCode::FORBIDDEN);

    let outsider_create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&outsider_cookie),
        Some(serde_json::json!({"name": "Intrusion", "quantity": 1.0})),
    )
    .await;
    assert_status(&outsider_create, StatusCode::FORBIDDEN);
}

/// AC: quantity and reorder_threshold cannot go negative.
#[sqlx::test]
async fn negative_quantity_is_rejected(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "stock-neg@example.test", "test-password-1234").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Lait", "quantity": -1.0})),
    )
    .await;
    assert_status(&create, StatusCode::BAD_REQUEST);
}

/// AC: `name` is trimmed on write, `category` can be explicitly cleared via
/// `{"category": null}` (distinct from omitting the field), and a blank
/// `unit` is rejected the same way a blank `name` is.
#[sqlx::test]
async fn update_can_clear_category_and_rejects_blank_unit(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "stock-clear@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "  Riz  ", "category": "Cereales", "quantity": 1.0})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let item = json_body(create).await;
    assert_eq!(
        item["name"], "Riz",
        "leading/trailing whitespace must be trimmed on create"
    );
    let item_id = item["id"].as_str().unwrap().to_string();

    let clear_category = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"category": null})),
    )
    .await;
    assert_status(&clear_category, StatusCode::OK);
    assert_eq!(
        json_body(clear_category).await["category"],
        serde_json::Value::Null,
        "explicit null must clear the category, not leave it unchanged"
    );

    let blank_unit = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"unit": "  "})),
    )
    .await;
    assert_status(&blank_unit, StatusCode::BAD_REQUEST);
}

/// AC: `reorder_threshold` can be explicitly cleared via
/// `{"reorder_threshold": null}` (distinct from omitting the field, which
/// must leave it untouched).
#[sqlx::test]
async fn update_can_clear_reorder_threshold(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "stock-clear-threshold@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Sucre", "quantity": 1.0, "reorder_threshold": 0.5})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let item_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let untouched = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"quantity": 2.0})),
    )
    .await;
    assert_status(&untouched, StatusCode::OK);
    assert_eq!(
        json_body(untouched).await["reorder_threshold"],
        0.5,
        "omitting the field must leave reorder_threshold untouched"
    );

    let cleared = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        Some(serde_json::json!({"reorder_threshold": null})),
    )
    .await;
    assert_status(&cleared, StatusCode::OK);
    assert_eq!(
        json_body(cleared).await["reorder_threshold"],
        serde_json::Value::Null,
        "explicit null must clear reorder_threshold, not leave it unchanged"
    );
}

/// AC (#401): an item carries an optional expiry date (one per article, the
/// nearest), readable on create/get/list, changed by a full edit, left alone
/// by a quantity-only adjust, cleared by an explicit `null`, and refused when
/// it is not a calendar date.
#[sqlx::test]
async fn expiry_date_is_set_changed_kept_and_cleared(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "stock-expiry@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let items = format!("/groups/{group_id}/stock-items");

    let undated = call(
        &router,
        Method::POST,
        &items,
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Sel", "quantity": 1.0})),
    )
    .await;
    assert_status(&undated, StatusCode::CREATED);
    assert_eq!(
        json_body(undated).await["expires_on"],
        serde_json::Value::Null,
        "an item created without a date has none"
    );

    let create = call(
        &router,
        Method::POST,
        &items,
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Yaourt", "quantity": 4.0, "expires_on": "2026-10-09"})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let created = json_body(create).await;
    assert_eq!(created["expires_on"], "2026-10-09");
    let item_id = created["id"].as_str().unwrap().to_string();
    let item = format!("{items}/{item_id}");

    let get = call(&router, Method::GET, &item, Some(&owner_cookie), None).await;
    assert_status(&get, StatusCode::OK);
    assert_eq!(json_body(get).await["expires_on"], "2026-10-09");

    let list = call(&router, Method::GET, &items, Some(&owner_cookie), None).await;
    let listed = json_body(list).await;
    let yaourt = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == "Yaourt")
        .unwrap();
    assert_eq!(yaourt["expires_on"], "2026-10-09");

    let changed = call(
        &router,
        Method::PATCH,
        &item,
        Some(&owner_cookie),
        Some(serde_json::json!({"expires_on": "2026-10-12"})),
    )
    .await;
    assert_status(&changed, StatusCode::OK);
    assert_eq!(json_body(changed).await["expires_on"], "2026-10-12");

    let adjusted = call(
        &router,
        Method::PATCH,
        &item,
        Some(&owner_cookie),
        Some(serde_json::json!({"quantity": 3.0})),
    )
    .await;
    assert_status(&adjusted, StatusCode::OK);
    assert_eq!(
        json_body(adjusted).await["expires_on"],
        "2026-10-12",
        "a quantity-only adjust must leave the expiry date untouched"
    );

    let not_a_date = call(
        &router,
        Method::PATCH,
        &item,
        Some(&owner_cookie),
        Some(serde_json::json!({"expires_on": "2026-02-30"})),
    )
    .await;
    assert!(
        not_a_date.status().is_client_error(),
        "an impossible date must be refused, got {}",
        not_a_date.status()
    );

    let cleared = call(
        &router,
        Method::PATCH,
        &item,
        Some(&owner_cookie),
        Some(serde_json::json!({"expires_on": null})),
    )
    .await;
    assert_status(&cleared, StatusCode::OK);
    assert_eq!(
        json_body(cleared).await["expires_on"],
        serde_json::Value::Null,
        "explicit null must clear the expiry date"
    );
}

/// AC (#401): every read derives `expiry_status` from the date and today in
/// Europe/Paris, and `?sort=expires_on` lists soonest first, undated items
/// last, name breaking ties. Any other `sort` value is a 400.
#[sqlx::test]
async fn expiry_status_is_derived_and_sort_puts_undated_last(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "stock-expiry-sort@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let items = format!("/groups/{group_id}/stock-items");
    let today = paris_day(Utc::now());
    let day = |offset: i64| (today + Duration::days(offset)).to_string();

    let mut yaourt_id = String::new();
    for (name, expires_on) in [
        ("Abricot", None),
        ("Lait", Some(day(2))),
        ("Yaourt", Some(day(-1))),
        ("Riz", Some(day(30))),
        ("Beurre", Some(day(2))),
        ("Crème", Some(day(0))),
    ] {
        let res = call(
            &router,
            Method::POST,
            &items,
            Some(&cookie),
            Some(serde_json::json!({"name": name, "quantity": 1.0, "expires_on": expires_on})),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
        let body = json_body(res).await;
        if name == "Yaourt" {
            assert_eq!(
                body["expiry_status"], "expired",
                "create returns the status"
            );
            yaourt_id = body["id"].as_str().unwrap().to_string();
        }
    }

    let get = call(
        &router,
        Method::GET,
        &format!("{items}/{yaourt_id}"),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(json_body(get).await["expiry_status"], "expired");

    let listing = |query: &'static str| {
        let router = router.clone();
        let cookie = cookie.clone();
        let path = format!("{items}{query}");
        async move {
            let res = call(&router, Method::GET, &path, Some(&cookie), None).await;
            assert_status(&res, StatusCode::OK);
            json_body(res).await["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| {
                    (
                        i["name"].as_str().unwrap().to_string(),
                        i["expiry_status"].as_str().unwrap().to_string(),
                    )
                })
                .collect::<Vec<_>>()
        }
    };
    let pairs = |v: &[(&str, &str)]| {
        v.iter()
            .map(|(n, s)| (n.to_string(), s.to_string()))
            .collect::<Vec<_>>()
    };

    assert_eq!(
        listing("").await,
        pairs(&[
            ("Abricot", "unknown"),
            ("Beurre", "soon"),
            ("Crème", "soon"),
            ("Lait", "soon"),
            ("Riz", "ok"),
            ("Yaourt", "expired"),
        ]),
        "no sort: name order, each with its derived status"
    );
    assert_eq!(
        listing("?sort=expires_on").await,
        pairs(&[
            ("Yaourt", "expired"),
            ("Crème", "soon"),
            ("Beurre", "soon"),
            ("Lait", "soon"),
            ("Riz", "ok"),
            ("Abricot", "unknown"),
        ]),
        "sort=expires_on: soonest first, name breaks ties, undated last"
    );

    for bad in ["?sort=expiry", "?sort=name", "?sort="] {
        let res = call(
            &router,
            Method::GET,
            &format!("{items}{bad}"),
            Some(&cookie),
            None,
        )
        .await;
        assert_status(&res, StatusCode::BAD_REQUEST);
        assert_eq!(json_body(res).await["error"], "invalid_sort", "{bad}");
    }
}

/// AC (#39): a regular member may adjust the **quantity** of an item another
/// member created (shared inventory), but a full-record edit (touching any
/// other field) and delete stay behind the creator/admin/owner bar.
#[sqlx::test]
async fn member_can_adjust_quantity_but_not_edit_or_delete(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "stock-adj-owner@example.test",
        "owner-password1",
    )
    .await;
    let member_cookie = register_verify_login(
        &router,
        &db,
        "stock-adj-member@example.test",
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

    // The owner creates the item.
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Sel", "quantity": 5.0, "unit": "kg"})),
    )
    .await;
    let item_id = json_body(create).await["id"].as_str().unwrap().to_string();

    // Quantity-only adjust by the non-creator member → allowed.
    let member_adjust = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&member_cookie),
        Some(serde_json::json!({"quantity": 1.5})),
    )
    .await;
    assert_status(&member_adjust, StatusCode::OK);
    assert_eq!(json_body(member_adjust).await["quantity"], 1.5);

    // A full-record edit (touching name) by the same member → forbidden.
    let member_edit = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&member_cookie),
        Some(serde_json::json!({"name": "Sel fin"})),
    )
    .await;
    assert_status(&member_edit, StatusCode::FORBIDDEN);

    // Even a quantity change bundled with a full-record field is forbidden.
    let member_edit_bundled = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&member_cookie),
        Some(serde_json::json!({"quantity": 2.0, "unit": "g"})),
    )
    .await;
    assert_status(&member_edit_bundled, StatusCode::FORBIDDEN);

    // The expiry date (#401) is part of the full record: setting it on
    // another member's item is forbidden too, and so is clearing it.
    for body in [
        serde_json::json!({"expires_on": "2026-10-09"}),
        serde_json::json!({"expires_on": null}),
    ] {
        let member_expiry = call(
            &router,
            Method::PATCH,
            &format!("/groups/{group_id}/stock-items/{item_id}"),
            Some(&member_cookie),
            Some(body),
        )
        .await;
        assert_status(&member_expiry, StatusCode::FORBIDDEN);
    }

    // Delete by the same member → forbidden.
    let member_delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&member_delete, StatusCode::FORBIDDEN);
}

/// AC: a regular member may create/read/adjust stock, but only the item's
/// creator or a group admin/owner may delete it.
#[sqlx::test]
async fn only_creator_or_admin_can_delete_stock_item(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "stock-owner3@example.test", "owner-password1").await;
    let member_cookie = register_verify_login(
        &router,
        &db,
        "stock-member@example.test",
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
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Pâtes", "quantity": 5.0})),
    )
    .await;
    let item_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let member_delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&member_delete, StatusCode::FORBIDDEN);

    let owner_delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&owner_delete, StatusCode::NO_CONTENT);
}
