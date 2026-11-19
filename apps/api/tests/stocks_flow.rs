mod common;

use axum::http::{Method, StatusCode};
use chrono::{Duration, Utc};
use common::thresholds::{assert_edit_follows_the_threshold, Editable};
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

/// #649: the reception's bound (1e9) holds for a creation, a full edit and a
/// quantity-only PATCH, for the quantity and the reorder threshold alike —
/// the bound itself is accepted, above it is a 400 and nothing is written.
#[sqlx::test]
async fn quantities_above_the_bound_are_rejected_everywhere(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "stock-max@example.test", "test-password-1234").await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let items = format!("/groups/{group_id}/stock-items");

    async fn error_code(res: axum::http::Response<axum::body::Body>) -> String {
        json_body(res).await["error"].as_str().unwrap().to_string()
    }

    let too_large = [
        (
            serde_json::json!({"name": "Lait", "quantity": 1_000_000_001.0}),
            "quantity_too_large",
        ),
        (
            serde_json::json!({"name": "Lait", "quantity": 1e300}),
            "quantity_too_large",
        ),
        (
            serde_json::json!({"name": "Lait", "quantity": 1.0, "reorder_threshold": 1_000_000_001.0}),
            "reorder_threshold_too_large",
        ),
    ];
    for (body, code) in &too_large {
        let res = call(
            &router,
            Method::POST,
            &items,
            Some(&cookie),
            Some(body.clone()),
        )
        .await;
        assert_status(&res, StatusCode::BAD_REQUEST);
        assert_eq!(error_code(res).await, *code, "{body}");
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM stock_items")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(count, 0, "a refused creation writes nothing");

    // The bound itself is accepted.
    let at_bound = call(
        &router,
        Method::POST,
        &items,
        Some(&cookie),
        Some(serde_json::json!({
            "name": "Lait", "quantity": 1_000_000_000.0, "reorder_threshold": 1_000_000_000.0
        })),
    )
    .await;
    assert_status(&at_bound, StatusCode::CREATED);
    let item_id = json_body(at_bound).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let item = format!("{items}/{item_id}");

    let patches = [
        // Quantity-only adjustment (open to any member, #39).
        (
            serde_json::json!({"quantity": 1_000_000_001.0}),
            "quantity_too_large",
        ),
        // Full edits.
        (
            serde_json::json!({"name": "Lait", "quantity": 1e300}),
            "quantity_too_large",
        ),
        (
            serde_json::json!({"reorder_threshold": 1_000_000_001.0}),
            "reorder_threshold_too_large",
        ),
    ];
    for (body, code) in &patches {
        let res = call(
            &router,
            Method::PATCH,
            &item,
            Some(&cookie),
            Some(body.clone()),
        )
        .await;
        assert_status(&res, StatusCode::BAD_REQUEST);
        assert_eq!(error_code(res).await, *code, "{body}");
    }
    let (quantity, threshold): (f64, Option<f64>) =
        sqlx::query_as("SELECT quantity, reorder_threshold FROM stock_items")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(
        (quantity, threshold),
        (1_000_000_000.0, Some(1_000_000_000.0)),
        "a refused PATCH writes nothing"
    );

    // Down from the bound, then back up to it by a quantity-only PATCH.
    for q in [3.0, 1_000_000_000.0] {
        let res = call(
            &router,
            Method::PATCH,
            &item,
            Some(&cookie),
            Some(serde_json::json!({ "quantity": q })),
        )
        .await;
        assert_status(&res, StatusCode::OK);
        assert_eq!(json_body(res).await["quantity"], q);
    }
}

/// #649: a reorder threshold written above the bound before it existed does
/// not block a quantity-only adjustment, which checks the quantity alone.
#[sqlx::test]
async fn a_legacy_threshold_above_the_bound_does_not_block_a_quantity_adjustment(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "stock-legacy@example.test",
        "test-password-1234",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&cookie),
        Some(serde_json::json!({"name": "Sel", "quantity": 1.0, "reorder_threshold": 1.0})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let item_id = json_body(create).await["id"].as_str().unwrap().to_string();
    sqlx::query("UPDATE stock_items SET reorder_threshold = 1e12 WHERE id = $1::uuid")
        .bind(&item_id)
        .execute(&db)
        .await
        .unwrap();

    let res = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&cookie),
        Some(serde_json::json!({"quantity": 2.0})),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["quantity"], 2.0);
    assert_eq!(
        body["reorder_threshold"], 1e12,
        "the threshold is left as is"
    );
}

/// AC: `name` is trimmed on write, `category` can be explicitly cleared via
/// `{"category": null}` (distinct from omitting the field), and a blank
/// `unit` becomes "unité" instead of an error (#458).
#[sqlx::test]
async fn update_can_clear_category_and_defaults_blank_unit(db: PgPool) {
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
    assert_status(&blank_unit, StatusCode::OK);
    assert_eq!(json_body(blank_unit).await["unit"], "unité");
}

/// #458: an empty, blank or absent `unit` on create is stored as "unité"
/// (never the SQL column default `'unit'`), and a filled one is trimmed.
#[sqlx::test]
async fn create_defaults_an_empty_unit_to_unite(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "stock-unit@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    let cases = [
        (
            serde_json::json!({"name": "Oeufs", "quantity": 6.0, "unit": ""}),
            "unité",
        ),
        (
            serde_json::json!({"name": "Citrons", "quantity": 2.0, "unit": "   "}),
            "unité",
        ),
        (
            serde_json::json!({"name": "Pommes", "quantity": 3.0}),
            "unité",
        ),
        (
            serde_json::json!({"name": "Farine", "quantity": 1.0, "unit": " kg "}),
            "kg",
        ),
    ];
    for (body, expected) in cases {
        let create = call(
            &router,
            Method::POST,
            &format!("/groups/{group_id}/stock-items"),
            Some(&owner_cookie),
            Some(body.clone()),
        )
        .await;
        assert_status(&create, StatusCode::CREATED);
        let item = json_body(create).await;
        assert_eq!(item["unit"], expected, "returned unit for {body}");
        let stored: String = sqlx::query_scalar("SELECT unit FROM stock_items WHERE id = $1::uuid")
            .bind(item["id"].as_str().unwrap())
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(stored, expected, "stored unit for {body}");
    }
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

/// `GET …/stocks/expiring_count`, returning `expiring_count`.
async fn expiring_count(router: &axum::Router, group_id: &str, cookie: &str) -> i64 {
    let res = call(
        router,
        Method::GET,
        &format!("/groups/{group_id}/stocks/expiring_count"),
        Some(cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    json_body(res).await["expiring_count"].as_i64().unwrap()
}

/// AC (#564): the Stocks tab's pastille counts the lines still in stock
/// dated up to J+`EXPIRY_SOON_DAYS` in Europe/Paris, past dates included —
/// not J+4, not an empty line, not an undated one, not another family's.
#[sqlx::test]
async fn the_expiring_count_is_expired_or_soon_lines_still_in_stock(db: PgPool) {
    let router = test_router(db.clone());
    let cookie = register_verify_login(
        &router,
        &db,
        "stock-expiring-count@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &cookie, "Foyer").await;
    let other_group = create_group(&router, &cookie, "Autre foyer").await;
    let today = paris_day(Utc::now());
    let day = |offset: i64| Some((today + Duration::days(offset)).to_string());

    assert_eq!(expiring_count(&router, &group_id, &cookie).await, 0);

    for (group, name, quantity, expires_on) in [
        (&group_id, "Crème", 1.0, day(3)),
        (&group_id, "Yaourt", 2.0, day(-5)),
        (&group_id, "Lait", 1.0, day(0)),
        (&group_id, "Beurre", 1.0, day(4)),
        (&group_id, "Fromage", 0.0, day(1)),
        (&group_id, "Sel", 1.0, None),
        (&other_group, "Jambon", 1.0, day(1)),
    ] {
        let res = call(
            &router,
            Method::POST,
            &format!("/groups/{group}/stock-items"),
            Some(&cookie),
            Some(serde_json::json!({"name": name, "quantity": quantity, "expires_on": expires_on})),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
    }

    // Crème (J+3), Yaourt (past) and Lait (today).
    assert_eq!(expiring_count(&router, &group_id, &cookie).await, 3);
    assert_eq!(expiring_count(&router, &other_group, &cookie).await, 1);
}

/// AC (#564): a non-member is refused the count, as the rest of the stocks.
#[sqlx::test]
async fn non_member_cannot_read_the_expiring_count(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(
        &router,
        &db,
        "stock-expiring-owner@example.test",
        "owner-password1",
    )
    .await;
    let outsider = register_verify_login(
        &router,
        &db,
        "stock-expiring-outsider@example.test",
        "outsider-password1",
    )
    .await;
    let group_id = create_group(&router, &owner, "Foyer").await;

    let res = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/stocks/expiring_count"),
        Some(&outsider),
        None,
    )
    .await;
    assert_status(&res, StatusCode::FORBIDDEN);
}

/// Creates an article as `cookie` and returns its id.
async fn create_item(
    router: &axum::Router,
    group_id: &str,
    cookie: &str,
    body: serde_json::Value,
) -> String {
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

/// PATCHes an article as `cookie`, expects a 200 and returns its body.
async fn patch_item(
    router: &axum::Router,
    group_id: &str,
    item_id: &str,
    cookie: &str,
    body: serde_json::Value,
) -> serde_json::Value {
    let res = call(
        router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(cookie),
        Some(body),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    json_body(res).await
}

/// The status of `GET` on an article.
async fn item_status(
    router: &axum::Router,
    group_id: &str,
    item_id: &str,
    cookie: &str,
) -> StatusCode {
    call(
        router,
        Method::GET,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(cookie),
        None,
    )
    .await
    .status()
}

/// AC (#800): a quantity adjustment that brings an article without a
/// reorder threshold to 0 removes it, for the creator and for any other
/// member alike — the consequence of the adjustment, not a DELETE, so it
/// does not ask the creator/admin/owner bar. An article with a threshold
/// stays at 0, « à racheter ». A full-record edit to 0 keeps the article.
#[sqlx::test]
async fn an_adjustment_to_zero_removes_an_article_without_a_threshold(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(
        &router,
        &db,
        "stock-zero-owner@example.test",
        "owner-password1",
    )
    .await;
    let member = register_verify_login(
        &router,
        &db,
        "stock-zero-member@example.test",
        "member-password1",
    )
    .await;
    let group_id = create_group(&router, &owner, "Foyer").await;
    let invite = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(&owner),
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
        Some(&member),
        None,
    )
    .await;
    let r = &router;
    let g = group_id.as_str();
    let zero = serde_json::json!({"quantity": 0.0});

    // The creator empties an article without a threshold: 200, told it is
    // removed, and it is gone.
    let sel = create_item(
        r,
        g,
        &owner,
        serde_json::json!({"name": "Sel", "quantity": 1.0}),
    )
    .await;
    let res = patch_item(r, g, &sel, &owner, zero.clone()).await;
    assert_eq!(res["removed"], true, "{res}");
    assert_eq!(res["quantity"], 0.0, "{res}");
    assert_eq!(item_status(r, g, &sel, &owner).await, StatusCode::NOT_FOUND);

    // Another member, who could not DELETE it, gets the same result.
    let riz = create_item(
        r,
        g,
        &owner,
        serde_json::json!({"name": "Riz", "quantity": 2.0}),
    )
    .await;
    let res = patch_item(r, g, &riz, &member, serde_json::json!({"quantity": 0})).await;
    assert_eq!(res["removed"], true, "{res}");
    assert_eq!(item_status(r, g, &riz, &owner).await, StatusCode::NOT_FOUND);

    // With a threshold, the article stays at 0, low on stock — for the
    // creator as for another member.
    for cookie in [&owner, &member] {
        let lait = create_item(
            r,
            g,
            &owner,
            serde_json::json!({"name": "Lait", "quantity": 2.0, "reorder_threshold": 1.0}),
        )
        .await;
        let res = patch_item(r, g, &lait, cookie, zero.clone()).await;
        assert!(res.get("removed").is_none(), "{res}");
        assert_eq!(res["quantity"], 0.0, "{res}");
        assert_eq!(res["low_stock"], true, "{res}");
        assert_eq!(item_status(r, g, &lait, &owner).await, StatusCode::OK);
    }

    // An adjustment that leaves some stock says nothing of a removal.
    let pates = create_item(
        r,
        g,
        &owner,
        serde_json::json!({"name": "Pâtes", "quantity": 2.0}),
    )
    .await;
    let res = patch_item(r, g, &pates, &member, serde_json::json!({"quantity": 1.0})).await;
    assert!(res.get("removed").is_none(), "{res}");

    // A full-record edit to 0 is not an adjustment: the article stays.
    let res = patch_item(
        r,
        g,
        &pates,
        &owner,
        serde_json::json!({"name": "Pâtes complètes", "quantity": 0.0}),
    )
    .await;
    assert!(res.get("removed").is_none(), "{res}");
    assert_eq!(item_status(r, g, &pates, &owner).await, StatusCode::OK);

    // A PATCH that names no quantity adjusts nothing: an article already at
    // 0 without a threshold stays.
    let vide = create_item(
        r,
        g,
        &owner,
        serde_json::json!({"name": "Vide", "quantity": 0.0}),
    )
    .await;
    let res = patch_item(r, g, &vide, &member, serde_json::json!({})).await;
    assert!(res.get("removed").is_none(), "{res}");
    assert_eq!(item_status(r, g, &vide, &owner).await, StatusCode::OK);
}

/// AC #866: the full edit and the delete of a stock item follow the family's `stocks_edit` threshold, the creator
/// keeping the hand on what they wrote (`common::thresholds`).
#[sqlx::test]
async fn editing_a_stock_item_follows_the_family_s_threshold(db: PgPool) {
    let router = test_router(db.clone());
    assert_edit_follows_the_threshold(
        &router,
        &db,
        Editable {
            feature: "stocks_edit",
            collection: "stock-items",
            create: serde_json::json!({"name": "Pâtes", "quantity": 5.0}),
            edit: serde_json::json!({"name": "Riz"}),
        },
    )
    .await;
}

/// AC #866: a quantity-only adjustment stays open to every member whatever
/// the `stocks_edit` threshold, as since #39, while the full edit follows
/// it.
#[sqlx::test]
async fn a_quantity_adjustment_stays_open_whatever_the_threshold(db: PgPool) {
    let router = test_router(db.clone());
    let fam = common::thresholds::family(&router, &db, "stocks-quantity").await;
    common::thresholds::set_threshold(&router, &fam, "stocks_edit", "owner").await;
    let group_id = &fam.group_id;
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&fam.owner),
        Some(serde_json::json!({"name": "Pâtes", "quantity": 5.0})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let item_id = json_body(create).await["id"].as_str().unwrap().to_string();
    let uri = format!("/groups/{group_id}/stock-items/{item_id}");

    for (who, cookie, quantity) in [("standard", &fam.standard, 4.0), ("admin", &fam.admin, 3.0)] {
        let adjust = call(
            &router,
            Method::PATCH,
            &uri,
            Some(cookie),
            Some(serde_json::json!({"quantity": quantity})),
        )
        .await;
        assert_status(&adjust, StatusCode::OK);
        assert_eq!(json_body(adjust).await["quantity"], quantity, "{who}");
        let full = call(
            &router,
            Method::PATCH,
            &uri,
            Some(cookie),
            Some(serde_json::json!({"name": "Riz"})),
        )
        .await;
        assert_eq!(full.status(), StatusCode::FORBIDDEN, "{who}'s full edit");
    }
}

// -- ingredient links (#903) ------------------------------------------------

/// « Mon p'tit plaisir », percent-encoded for a path.
const PLAISIR: &str = "Mon%20p'tit%20plaisir";

/// An owner, a member who joined their family, and the family.
async fn family_with_member(
    router: &axum::Router,
    db: &PgPool,
    prefix: &str,
) -> (String, String, String) {
    let owner = register_verify_login(
        router,
        db,
        &format!("{prefix}-owner@example.test"),
        "owner-password1",
    )
    .await;
    let member = register_verify_login(
        router,
        db,
        &format!("{prefix}-member@example.test"),
        "member-password1",
    )
    .await;
    let group_id = create_group(router, &owner, "Foyer").await;
    let invite = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(&owner),
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
        Some(&member),
        None,
    )
    .await;
    assert_status(&accept, StatusCode::OK);
    (owner, member, group_id)
}

async fn put_link(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    name: &str,
    tag: &str,
) -> axum::response::Response {
    call(
        router,
        Method::PUT,
        &format!("/groups/{group_id}/ingredient-links/{name}"),
        Some(cookie),
        Some(serde_json::json!({ "tag": tag })),
    )
    .await
}

async fn delete_link(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    name: &str,
) -> axum::response::Response {
    call(
        router,
        Method::DELETE,
        &format!("/groups/{group_id}/ingredient-links/{name}"),
        Some(cookie),
        None,
    )
    .await
}

/// The family's links, as `(name, tag)`.
async fn links(router: &axum::Router, cookie: &str, group_id: &str) -> Vec<(String, String)> {
    let res = call(
        router,
        Method::GET,
        &format!("/groups/{group_id}/ingredient-links"),
        Some(cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    json_body(res).await["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["name"].as_str().unwrap().to_string(),
                l["tag"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// A planned recipe « Tarte » that needs « Crème fraîche », no quantity.
async fn planned_tart(router: &axum::Router, cookie: &str, group_id: &str) {
    let res = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/recipes"),
        Some(cookie),
        Some(serde_json::json!({
            "name": "Tarte",
            "ingredients": [{"name": "Crème fraîche", "is_optional": false}],
        })),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let recipe_id = json_body(res).await["id"].as_str().unwrap().to_string();
    let select = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/recipes/{recipe_id}/selection"),
        Some(cookie),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&select, StatusCode::CREATED);
}

/// What the tart lacks, read by the suggestions and by the planned
/// selection: `(suggestions, planned)`.
async fn tart_missing(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
) -> (serde_json::Value, serde_json::Value) {
    let res = call(
        router,
        Method::GET,
        &format!("/groups/{group_id}/recipes/suggestions"),
        Some(cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let suggested = json_body(res).await["suggestions"][0]["missing_ingredients"].clone();
    let res = call(
        router,
        Method::GET,
        &format!("/groups/{group_id}/recipes/selections"),
        Some(cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let planned = json_body(res).await["selections"][0]["missing"].clone();
    (suggested, planned)
}

async fn stock_plaisir(router: &axum::Router, cookie: &str, group_id: &str, name: &str) -> String {
    let res = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(cookie),
        Some(serde_json::json!({"name": name, "quantity": 1.0, "unit": "pot"})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

/// Issue #903: once « mon p'tit plaisir » is linked to crème fraîche, a
/// recipe needing crème fraîche is covered, for the suggestions and for
/// what a planned recipe lacks; the link holds for every line of that name,
/// one added later included, and outlives the article.
#[sqlx::test]
async fn a_link_covers_the_recipes_needing_its_category(db: PgPool) {
    let router = test_router(db.clone());
    let (owner, member, group_id) = family_with_member(&router, &db, "link-cover").await;
    let item_id = stock_plaisir(&router, &owner, &group_id, "Mon p'tit plaisir").await;
    planned_tart(&router, &owner, &group_id).await;

    let lacking = serde_json::json!([{"name": "Crème fraîche", "quantity": null, "unit": null}]);
    let (suggested, planned) = tart_missing(&router, &member, &group_id).await;
    assert_eq!(suggested, lacking);
    assert_eq!(planned.as_array().unwrap().len(), 1, "{planned}");

    // Any member may link; the name is the one typed, the label the
    // category's French name.
    let res = put_link(&router, &member, &group_id, PLAISIR, "en:fermented-creams").await;
    assert_status(&res, StatusCode::OK);
    let link = json_body(res).await;
    assert_eq!(link["name"], "Mon p'tit plaisir");
    assert_eq!(link["tag"], "en:fermented-creams");
    assert!(!link["label"].as_str().unwrap().is_empty(), "{link}");

    let (suggested, planned) = tart_missing(&router, &member, &group_id).await;
    assert_eq!(suggested, serde_json::json!([]));
    assert_eq!(planned, serde_json::json!([]));

    // Deleting the article leaves the link; a line of the same name,
    // written otherwise, added later, is covered by it.
    let res = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&owner),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(
        links(&router, &owner, &group_id).await,
        vec![(
            "Mon p'tit plaisir".to_string(),
            "en:fermented-creams".to_string()
        )]
    );
    assert_eq!(tart_missing(&router, &member, &group_id).await.0, lacking);
    stock_plaisir(&router, &member, &group_id, "MON P’TIT  PLAISIRS").await;
    let (suggested, planned) = tart_missing(&router, &member, &group_id).await;
    assert_eq!(suggested, serde_json::json!([]));
    assert_eq!(planned, serde_json::json!([]));

    // Replacing the link changes the category: the tart lacks it again.
    let res = put_link(&router, &owner, &group_id, PLAISIR, "en:butters").await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(links(&router, &owner, &group_id).await.len(), 1);
    assert_eq!(tart_missing(&router, &member, &group_id).await.0, lacking);
}

/// Issue #903: another family neither sees nor applies a link — its own
/// line of the same name stays uncovered, its list stays empty, and it
/// reaches the first family's links through no path.
#[sqlx::test]
async fn another_family_neither_sees_nor_applies_a_link(db: PgPool) {
    let router = test_router(db.clone());
    let (owner_a, _, group_a) = family_with_member(&router, &db, "link-a").await;
    let (owner_b, _, group_b) = family_with_member(&router, &db, "link-b").await;
    stock_plaisir(&router, &owner_a, &group_a, "Mon p'tit plaisir").await;
    stock_plaisir(&router, &owner_b, &group_b, "Mon p'tit plaisir").await;
    planned_tart(&router, &owner_a, &group_a).await;
    planned_tart(&router, &owner_b, &group_b).await;

    let res = put_link(&router, &owner_a, &group_a, PLAISIR, "en:fermented-creams").await;
    assert_status(&res, StatusCode::OK);

    assert_eq!(
        tart_missing(&router, &owner_a, &group_a).await,
        (serde_json::json!([]), serde_json::json!([]))
    );
    let (suggested, planned) = tart_missing(&router, &owner_b, &group_b).await;
    assert_eq!(suggested.as_array().unwrap().len(), 1, "{suggested}");
    assert_eq!(planned.as_array().unwrap().len(), 1, "{planned}");
    assert!(links(&router, &owner_b, &group_b).await.is_empty());

    // Through its own family, the name finds nothing to delete.
    let res = delete_link(&router, &owner_b, &group_b, PLAISIR).await;
    assert_status(&res, StatusCode::NOT_FOUND);
    // Through the other's, it is no member.
    for res in [
        call(
            &router,
            Method::GET,
            &format!("/groups/{group_a}/ingredient-links"),
            Some(&owner_b),
            None,
        )
        .await,
        put_link(&router, &owner_b, &group_a, PLAISIR, "en:butters").await,
        delete_link(&router, &owner_b, &group_a, PLAISIR).await,
    ] {
        assert_status(&res, StatusCode::FORBIDDEN);
    }
    assert_eq!(
        links(&router, &owner_a, &group_a).await,
        vec![(
            "Mon p'tit plaisir".to_string(),
            "en:fermented-creams".to_string()
        )]
    );
}

async fn item_ingredient(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    item_id: &str,
) -> axum::response::Response {
    call(
        router,
        Method::GET,
        &format!("/groups/{group_id}/stock-items/{item_id}/ingredient"),
        Some(cookie),
        None,
    )
    .await
}

/// Issue #904: an article's page reads whether it has a category — by its
/// name, its scanned product or its name's link — and the link itself,
/// for its family only.
#[sqlx::test]
async fn an_article_s_page_reads_its_category_and_its_link(db: PgPool) {
    let router = test_router(db.clone());
    let (owner, member, group_id) = family_with_member(&router, &db, "link-item").await;
    let (stranger, _, _) = family_with_member(&router, &db, "link-item-b").await;
    let plaisir = stock_plaisir(&router, &owner, &group_id, "Mon p'tit plaisir").await;
    let lait = stock_plaisir(&router, &owner, &group_id, "Lait demi-écrémé").await;
    let code = "3017620422003";
    sqlx::query("INSERT INTO off_products (code, name, categories_tags) VALUES ($1, 'Pot', $2)")
        .bind(code)
        .bind(vec!["en:fermented-creams".to_string()])
        .execute(&db)
        .await
        .unwrap();
    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner),
        Some(serde_json::json!({"name": "Pot blanc", "quantity": 1.0, "barcode": code})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let scanned = json_body(res).await["id"].as_str().unwrap().to_string();

    for (item, recognized) in [(&plaisir, false), (&lait, true), (&scanned, true)] {
        let res = item_ingredient(&router, &member, &group_id, item).await;
        assert_status(&res, StatusCode::OK);
        assert_eq!(
            json_body(res).await,
            serde_json::json!({"recognized": recognized, "link": null}),
            "{item}"
        );
    }

    let res = put_link(&router, &member, &group_id, PLAISIR, "en:fermented-creams").await;
    assert_status(&res, StatusCode::OK);
    let set = json_body(res).await;
    let res = item_ingredient(&router, &owner, &group_id, &plaisir).await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(
        json_body(res).await,
        serde_json::json!({
            "recognized": true,
            "link": {
                "tag": "en:fermented-creams",
                "label": set["label"],
                "created_by": set["created_by"],
            },
        })
    );

    // Another family's member is no member; an unknown article is a 404.
    let res = item_ingredient(&router, &stranger, &group_id, &plaisir).await;
    assert_status(&res, StatusCode::FORBIDDEN);
    let res = item_ingredient(&router, &owner, &group_id, &uuid::Uuid::new_v4().to_string()).await;
    assert_status(&res, StatusCode::NOT_FOUND);
}

/// Issue #903: a category the taxonomy lacks, or a name without a word, is
/// a 400 and stores nothing.
#[sqlx::test]
async fn a_link_to_an_unknown_category_is_refused(db: PgPool) {
    let router = test_router(db.clone());
    let (owner, _, group_id) = family_with_member(&router, &db, "link-400").await;
    for (name, tag, code) in [
        (PLAISIR, "en:not-a-category", "unknown_tag"),
        (PLAISIR, "", "unknown_tag"),
        ("%20-%20", "en:fermented-creams", "name_required"),
    ] {
        let res = put_link(&router, &owner, &group_id, name, tag).await;
        assert_status(&res, StatusCode::BAD_REQUEST);
        assert_eq!(json_body(res).await["error"], code, "{name} {tag}");
    }
    assert!(links(&router, &owner, &group_id).await.is_empty());
}

/// Issue #903: a link is deleted by whoever set it last, or by an admin or
/// the owner; another standard member may not.
#[sqlx::test]
async fn deleting_a_link_is_for_its_creator_or_an_admin(db: PgPool) {
    let router = test_router(db.clone());
    let (owner, member, group_id) = family_with_member(&router, &db, "link-del").await;

    let res = put_link(&router, &owner, &group_id, PLAISIR, "en:fermented-creams").await;
    assert_status(&res, StatusCode::OK);
    let res = delete_link(&router, &member, &group_id, PLAISIR).await;
    assert_status(&res, StatusCode::FORBIDDEN);
    assert_eq!(links(&router, &owner, &group_id).await.len(), 1);
    // The name written otherwise reaches the same link.
    let res = delete_link(&router, &owner, &group_id, "mon%20P'TIT%20plaisirs").await;
    assert_status(&res, StatusCode::NO_CONTENT);
    let res = delete_link(&router, &owner, &group_id, PLAISIR).await;
    assert_status(&res, StatusCode::NOT_FOUND);

    // The member's own link: theirs to delete.
    let res = put_link(&router, &member, &group_id, PLAISIR, "en:fermented-creams").await;
    assert_status(&res, StatusCode::OK);
    let res = delete_link(&router, &member, &group_id, PLAISIR).await;
    assert_status(&res, StatusCode::NO_CONTENT);

    // The owner deletes a member's link.
    let res = put_link(&router, &member, &group_id, PLAISIR, "en:fermented-creams").await;
    assert_status(&res, StatusCode::OK);
    let res = delete_link(&router, &owner, &group_id, PLAISIR).await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert!(links(&router, &owner, &group_id).await.is_empty());
}

/// Issue #903: a member leaving the family takes the links they set with
/// them (the key onto the membership cascades); the others' stay.
#[sqlx::test]
async fn leaving_the_family_takes_the_member_s_links(db: PgPool) {
    let router = test_router(db.clone());
    let (owner, member, group_id) = family_with_member(&router, &db, "link-leave").await;
    let res = put_link(&router, &member, &group_id, PLAISIR, "en:fermented-creams").await;
    assert_status(&res, StatusCode::OK);
    let res = put_link(&router, &owner, &group_id, "Beurre%20maison", "en:butters").await;
    assert_status(&res, StatusCode::OK);

    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/leave"),
        Some(&member),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(
        links(&router, &owner, &group_id).await,
        vec![("Beurre maison".to_string(), "en:butters".to_string())]
    );
}

/// Issue #903: a family holds at most `LINKS_MAX` links; replacing one
/// never counts.
#[sqlx::test]
async fn the_family_s_links_are_bounded(db: PgPool) {
    use manage_our_home::stocks::ingredient_links::LINKS_MAX;
    let router = test_router(db.clone());
    let (owner, _, group_id) = family_with_member(&router, &db, "link-max").await;
    // The bound's worth of links, written directly.
    sqlx::query(
        "INSERT INTO ingredient_links (group_id, name_key, name, tag, created_by)
         SELECT g.group_id, 'article ' || n, 'Article ' || n, 'en:butters', g.user_id
         FROM group_members g, generate_series(1, $2) n
         WHERE g.group_id = $1::uuid AND g.role = 'owner'",
    )
    .bind(&group_id)
    .bind(LINKS_MAX as i32)
    .execute(&db)
    .await
    .unwrap();

    let res = put_link(&router, &owner, &group_id, PLAISIR, "en:fermented-creams").await;
    assert_status(&res, StatusCode::CONFLICT);
    assert_eq!(json_body(res).await["error"], "link_limit_reached");
    let res = put_link(
        &router,
        &owner,
        &group_id,
        "Article%201",
        "en:fermented-creams",
    )
    .await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(
        links(&router, &owner, &group_id).await.len(),
        LINKS_MAX as usize
    );
}
