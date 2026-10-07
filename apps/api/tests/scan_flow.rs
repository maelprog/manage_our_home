//! `POST /groups/:id/stock-items/scan` and the barcode on a stock item
//! (#402), end to end, with Open Food Facts played by a local stub: no test
//! here reaches the network.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::extract::Path;
use axum::http::{Method, StatusCode};
use axum::response::IntoResponse;
use common::{assert_status, call, json_body, set_cookie, test_state};
use sqlx::PgPool;

const NUTELLA: &str = "3017620422003";
/// Known to the stub under its EAN-13 form, `0036000291452`.
const UPC_A: &str = "036000291452";
const UNKNOWN: &str = "96385074";
const NAMELESS: &str = "4006381333931";
const FAILING: &str = "5000159484695";
const WEIGHED: &str = "2123456012347";

/// Stands in for Open Food Facts: counts the reads it gets and answers as
/// the real service does — a product, a 404 carrying `status: 0`, a record
/// without a name, or a 500.
async fn off_stub() -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let app = axum::Router::new().route(
        "/api/v2/product/:code",
        axum::routing::get(move |Path(code): Path<String>| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let found = |product: serde_json::Value| {
                    axum::Json(serde_json::json!({ "code": code, "status": 1, "product": product }))
                        .into_response()
                };
                match code.as_str() {
                    NUTELLA => found(serde_json::json!({
                        "product_name": "Nutella",
                        "product_name_fr": "Pâte à tartiner Nutella",
                        "quantity": "400 g",
                        "categories_tags": ["en:spreads", "en:sweet-spreads"],
                    })),
                    "0036000291452" => found(serde_json::json!({ "product_name": "Mouchoirs" })),
                    NAMELESS => {
                        found(serde_json::json!({ "product_name": "", "quantity": "1 kg" }))
                    }
                    FAILING => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    _ => (
                        StatusCode::NOT_FOUND,
                        axum::Json(serde_json::json!({ "code": code, "status": 0 })),
                    )
                        .into_response(),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, hits)
}

async fn router_with_off(db: PgPool) -> (axum::Router, Arc<AtomicUsize>) {
    let (base, hits) = off_stub().await;
    let mut state = test_state(db);
    state.openfoodfacts_base_url = base;
    (manage_our_home::build_router(state), hits)
}

async fn register_verify_login(router: &axum::Router, db: &PgPool, email: &str) -> String {
    let password = "scan-password1";
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

async fn create_group(router: &axum::Router, cookie: &str) -> String {
    let res = call(
        router,
        Method::POST,
        "/groups",
        Some(cookie),
        Some(serde_json::json!({"name": "Foyer"})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

async fn scan(
    router: &axum::Router,
    cookie: &str,
    group_id: &str,
    raw: &str,
) -> axum::response::Response {
    call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items/scan"),
        Some(cookie),
        Some(serde_json::json!({ "raw": raw })),
    )
    .await
}

async fn stock_count(db: &PgPool) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM stock_items")
        .fetch_one(db)
        .await
        .unwrap()
}

#[sqlx::test]
async fn a_known_code_brings_the_product_and_writes_no_article(db: PgPool) {
    let (router, hits) = router_with_off(db.clone()).await;
    let cookie = register_verify_login(&router, &db, "scan-known@example.test").await;
    let group_id = create_group(&router, &cookie).await;

    let res = scan(&router, &cookie, &group_id, NUTELLA).await;
    assert_status(&res, StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["code"], NUTELLA);
    assert_eq!(body["weighed"], false);
    assert_eq!(body["product"]["name"], "Pâte à tartiner Nutella");
    assert_eq!(body["product"]["quantity"], "400 g");
    assert_eq!(
        body["product"]["categories_tags"],
        serde_json::json!(["en:spreads", "en:sweet-spreads"])
    );
    assert!(body["existing_item_id"].is_null(), "{body}");
    assert!(body["expires_on"].is_null(), "{body}");
    assert!(body["expires_on_source"].is_null(), "{body}");
    assert_eq!(
        stock_count(&db).await,
        0,
        "a scan adds nothing to the stock"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // The second scan is served from the cache.
    let again = json_body(scan(&router, &cookie, &group_id, NUTELLA).await).await;
    assert_eq!(again["product"]["name"], "Pâte à tartiner Nutella");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "Open Food Facts asked twice"
    );
}

#[sqlx::test]
async fn an_expired_cache_entry_is_asked_again(db: PgPool) {
    let (router, hits) = router_with_off(db.clone()).await;
    let cookie = register_verify_login(&router, &db, "scan-expired@example.test").await;
    let group_id = create_group(&router, &cookie).await;
    sqlx::query(
        "INSERT INTO off_products (code, name, fetched_at) VALUES ($1, 'Ancien nom', now() - interval '31 days')",
    )
    .bind(NUTELLA)
    .execute(&db)
    .await
    .unwrap();

    let body = json_body(scan(&router, &cookie, &group_id, NUTELLA).await).await;
    assert_eq!(body["product"]["name"], "Pâte à tartiner Nutella");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM off_products WHERE code = $1")
        .bind(NUTELLA)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(name.as_deref(), Some("Pâte à tartiner Nutella"));
}

#[sqlx::test]
async fn an_unknown_or_nameless_code_has_no_product_and_the_miss_is_cached(db: PgPool) {
    let (router, hits) = router_with_off(db.clone()).await;
    let cookie = register_verify_login(&router, &db, "scan-unknown@example.test").await;
    let group_id = create_group(&router, &cookie).await;

    for code in [UNKNOWN, NAMELESS] {
        for _ in 0..2 {
            let res = scan(&router, &cookie, &group_id, code).await;
            assert_status(&res, StatusCode::OK);
            let body = json_body(res).await;
            assert_eq!(body["code"], code);
            assert!(body["product"].is_null(), "{code}: {body}");
        }
    }
    assert_eq!(hits.load(Ordering::SeqCst), 2, "one read per code");
}

#[sqlx::test]
async fn open_food_facts_failing_is_no_product_and_is_not_cached(db: PgPool) {
    let (router, hits) = router_with_off(db.clone()).await;
    let cookie = register_verify_login(&router, &db, "scan-failing@example.test").await;
    let group_id = create_group(&router, &cookie).await;

    for _ in 0..2 {
        let res = scan(&router, &cookie, &group_id, FAILING).await;
        assert_status(&res, StatusCode::OK);
        assert!(json_body(res).await["product"].is_null());
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "a failure must not be cached"
    );
    let cached: i64 = sqlx::query_scalar("SELECT count(*) FROM off_products")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(cached, 0);
}

#[sqlx::test]
async fn open_food_facts_unreachable_is_no_product(db: PgPool) {
    // `test_state` points Open Food Facts at a port nothing listens on.
    let router = manage_our_home::build_router(test_state(db.clone()));
    let cookie = register_verify_login(&router, &db, "scan-down@example.test").await;
    let group_id = create_group(&router, &cookie).await;

    let res = scan(&router, &cookie, &group_id, NUTELLA).await;
    assert_status(&res, StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["code"], NUTELLA);
    assert!(body["product"].is_null(), "{body}");
}

#[sqlx::test]
async fn a_weighing_label_is_not_sent_to_open_food_facts(db: PgPool) {
    let (router, hits) = router_with_off(db.clone()).await;
    let cookie = register_verify_login(&router, &db, "scan-weighed@example.test").await;
    let group_id = create_group(&router, &cookie).await;

    let res = scan(&router, &cookie, &group_id, WEIGHED).await;
    assert_status(&res, StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["weighed"], true);
    assert!(body["product"].is_null(), "{body}");
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[sqlx::test]
async fn a_string_that_is_not_a_code_is_a_422(db: PgPool) {
    let (router, hits) = router_with_off(db.clone()).await;
    let cookie = register_verify_login(&router, &db, "scan-invalid@example.test").await;
    let group_id = create_group(&router, &cookie).await;

    for raw in ["", "https://example.test", "3017620422004", "12345"] {
        let res = scan(&router, &cookie, &group_id, raw).await;
        assert_status(&res, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(json_body(res).await["error"], "invalid_barcode", "{raw:?}");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[sqlx::test]
async fn a_non_member_cannot_scan_into_a_family(db: PgPool) {
    let (router, hits) = router_with_off(db.clone()).await;
    let owner = register_verify_login(&router, &db, "scan-owner@example.test").await;
    let outsider = register_verify_login(&router, &db, "scan-outsider@example.test").await;
    let group_id = create_group(&router, &owner).await;

    let res = scan(&router, &outsider, &group_id, NUTELLA).await;
    assert_status(&res, StatusCode::FORBIDDEN);
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[sqlx::test]
async fn a_code_already_in_stock_names_the_article_for_any_member(db: PgPool) {
    let (router, hits) = router_with_off(db.clone()).await;
    let owner = register_verify_login(&router, &db, "scan-in-stock@example.test").await;
    let member = register_verify_login(&router, &db, "scan-member@example.test").await;
    let group_id = create_group(&router, &owner).await;
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

    // Created under its UPC-A form, stored under its EAN-13 one.
    let create = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner),
        Some(serde_json::json!({"name": "Mouchoirs", "quantity": 1.0, "barcode": UPC_A})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let item = json_body(create).await;
    assert_eq!(item["barcode"], "0036000291452");
    let item_id = item["id"].as_str().unwrap().to_string();

    // A standard member scans it under its EAN-13 form.
    let res = scan(&router, &member, &group_id, "0036000291452").await;
    assert_status(&res, StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["existing_item_id"], item_id.as_str());
    assert!(body["product"].is_null(), "{body}");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "no product needed");

    // The rescan's increment is the quantity-only PATCH any member may send.
    let adjust = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}/stock-items/{item_id}"),
        Some(&member),
        Some(serde_json::json!({"quantity": 2.0})),
    )
    .await;
    assert_status(&adjust, StatusCode::OK);
    assert_eq!(json_body(adjust).await["barcode"], "0036000291452");

    // The same code in another family is not "already in stock" there.
    let other_group = create_group(&router, &member).await;
    let elsewhere = json_body(scan(&router, &member, &other_group, UPC_A).await).await;
    assert!(elsewhere["existing_item_id"].is_null(), "{elsewhere}");
    assert_eq!(elsewhere["product"]["name"], "Mouchoirs");
}

#[sqlx::test]
async fn a_second_article_with_the_same_code_is_a_409(db: PgPool) {
    let (router, _) = router_with_off(db.clone()).await;
    let cookie = register_verify_login(&router, &db, "scan-dup@example.test").await;
    let group_id = create_group(&router, &cookie).await;
    let path = format!("/groups/{group_id}/stock-items");
    let create = |barcode: &'static str| {
        call(
            &router,
            Method::POST,
            &path,
            Some(&cookie),
            Some(serde_json::json!({"name": "Nutella", "quantity": 1.0, "barcode": barcode})),
        )
    };

    assert_status(&create(NUTELLA).await, StatusCode::CREATED);
    let dup = create(" 3 017620 422003 ").await;
    assert_status(&dup, StatusCode::CONFLICT);
    assert_eq!(json_body(dup).await["error"], "barcode_already_in_stock");
    assert_eq!(stock_count(&db).await, 1);

    // Articles without a code are not constrained.
    for _ in 0..2 {
        let res = call(
            &router,
            Method::POST,
            &format!("/groups/{group_id}/stock-items"),
            Some(&cookie),
            Some(serde_json::json!({"name": "Nutella", "quantity": 1.0})),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
        assert!(json_body(res).await["barcode"].is_null());
    }
}

#[sqlx::test]
async fn an_invalid_barcode_on_creation_is_a_400(db: PgPool) {
    let (router, _) = router_with_off(db.clone()).await;
    let cookie = register_verify_login(&router, &db, "scan-badcode@example.test").await;
    let group_id = create_group(&router, &cookie).await;

    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&cookie),
        Some(serde_json::json!({"name": "Riz", "quantity": 1.0, "barcode": "3017620422004"})),
    )
    .await;
    assert_status(&res, StatusCode::BAD_REQUEST);
    assert_eq!(json_body(res).await["error"], "invalid_barcode");
    assert_eq!(stock_count(&db).await, 0);
}
