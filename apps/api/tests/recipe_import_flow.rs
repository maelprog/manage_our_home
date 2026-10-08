//! `POST /groups/:id/recipes/import` and `source_url` on a recipe (#405),
//! end to end, with the recipe sites played by a local server: no test
//! here reaches the network.
//!
//! Two clients fetch the pages. The SSRF tests use the production one,
//! untouched (`test_state`), and check that it refuses the local server
//! however it is named. The other tests use the same configuration
//! (`outbound_http::recipe_import_builder`), with one test name,
//! `recettes.test`, pinned to the local server: the redirects it serves are
//! still checked by the production redirect policy and resolver.

mod common;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use common::{assert_status, call, json_body, set_cookie, test_state};
use manage_our_home::outbound_http::recipe_import_builder;
use manage_our_home::recipes::import::{MAX_PAGE_BYTES, RATE_LIMIT};
use sqlx::PgPool;

const MARMITON: &str = include_str!("fixtures/recipe_pages/marmiton_graph.html");
const NO_RECIPE: &str = include_str!("fixtures/recipe_pages/no_recipe.html");

/// The name the test client pins to the local server.
const SITE: &str = "recettes.test";

/// A recipe site: serves the fixtures, the failures and the redirects the
/// tests need, and counts the requests it gets.
async fn site() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let port = addr.port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let html =
        |body: String| ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response();
    let redirect = |to: String| {
        Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, to)
            .body(Body::empty())
            .unwrap()
    };
    let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
        let counter = counter.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            match uri.path() {
                "/marmiton" => html(MARMITON.into()),
                "/no-recipe" => html(NO_RECIPE.into()),
                "/json" => axum::Json(serde_json::json!({"@type": "Recipe"})).into_response(),
                "/no-type" => Response::new(Body::from(MARMITON)),
                "/broken" => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                "/heavy" => html("a".repeat(MAX_PAGE_BYTES + 1)),
                "/heavy-unannounced" => {
                    // Streamed, without a Content-Length: only counting the
                    // bytes as they come catches it.
                    let chunk = bytes::Bytes::from(vec![b'a'; 64 * 1024]);
                    let chunks = MAX_PAGE_BYTES / chunk.len() + 2;
                    let stream = futures::stream::iter(
                        (0..chunks).map(move |_| Ok::<_, std::io::Error>(chunk.clone())),
                    );
                    Response::builder()
                        .header(header::CONTENT_TYPE, "text/html")
                        .body(Body::from_stream(stream))
                        .unwrap()
                }
                "/slow" => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    html(MARMITON.into())
                }
                "/to-site" => redirect(format!("http://{SITE}:{port}/marmiton")),
                "/to-loopback" => redirect(format!("http://127.0.0.1:{port}/marmiton")),
                "/to-private" => redirect("http://10.0.0.1/recette".into()),
                "/to-ipv6-loopback" => redirect(format!("http://[::1]:{port}/marmiton")),
                "/to-localhost" => redirect(format!("http://localhost:{port}/marmiton")),
                "/to-ftp" => redirect("ftp://example.com/recette".into()),
                "/loop" => redirect(format!("http://{SITE}:{port}/loop")),
                _ => StatusCode::NOT_FOUND.into_response(),
            }
        }
    });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, hits)
}

/// The production import client's configuration, with `SITE` pinned to
/// the local server and a short timeout for the slow page.
fn test_client(addr: SocketAddr) -> reqwest::Client {
    recipe_import_builder()
        .resolve(SITE, addr)
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap()
}

struct Setup {
    router: axum::Router,
    cookie: String,
    group_id: String,
    port: u16,
    hits: Arc<AtomicUsize>,
}

/// A member of a family, the local site, and a router whose import client
/// is the production one (`pinned: false`) or the pinned test client.
async fn setup(db: PgPool, pinned: bool) -> Setup {
    let (addr, hits) = site().await;
    let mut state = test_state(db.clone());
    if pinned {
        state.recipe_import_client = test_client(addr);
    }
    let router = manage_our_home::build_router(state);
    let cookie = register_verify_login(&router, &db, "importer@example.test").await;
    let group_id = create_group(&router, &cookie).await;
    Setup {
        router,
        cookie,
        group_id,
        port: addr.port(),
        hits,
    }
}

async fn register_verify_login(router: &axum::Router, db: &PgPool, email: &str) -> String {
    let password = "import-password1";
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

async fn import(s: &Setup, url: &str) -> Response<Body> {
    call(
        &s.router,
        Method::POST,
        &format!("/groups/{}/recipes/import", s.group_id),
        Some(&s.cookie),
        Some(serde_json::json!({ "url": url })),
    )
    .await
}

/// The 422's `error` code.
async fn refused(s: &Setup, url: &str) -> String {
    let res = import(s, url).await;
    assert_status(&res, StatusCode::UNPROCESSABLE_ENTITY);
    json_body(res).await["error"].as_str().unwrap().to_string()
}

async fn recipe_count(db: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM recipes")
        .fetch_one(db)
        .await
        .unwrap()
}

#[sqlx::test]
async fn a_recipe_page_gives_a_draft_and_creates_nothing(db: PgPool) {
    let s = setup(db.clone(), true).await;
    let url = format!("http://{SITE}:{}/marmiton", s.port);
    let res = import(&s, &url).await;
    assert_status(&res, StatusCode::OK);
    let draft = json_body(res).await;
    assert_eq!(draft["name"], "Pâte à crêpes simple : la meilleure recette");
    assert_eq!(draft["source_url"], url);
    assert_eq!(
        draft["ingredients"][1],
        serde_json::json!({"quantity": 500.0, "unit": "g", "name": "farine"})
    );
    assert_eq!(
        draft["ingredients"][3],
        serde_json::json!({"quantity": 3.0, "unit": "c. à soupe", "name": "huile"})
    );
    assert_eq!(draft["ingredients"].as_array().unwrap().len(), 5);
    assert_eq!(draft["steps"].as_array().unwrap().len(), 3);
    assert_eq!(
        draft["steps"][2],
        "Laisser reposer 1 heure, votre pâte est prête."
    );
    assert_eq!(recipe_count(&db).await, 0, "the import saves nothing");
}

#[sqlx::test]
async fn a_redirect_to_a_public_name_is_followed(db: PgPool) {
    let s = setup(db, true).await;
    let res = import(&s, &format!("http://{SITE}:{}/to-site", s.port)).await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(s.hits.load(Ordering::SeqCst), 2);
}

#[sqlx::test]
async fn a_page_without_a_recipe_or_not_html_is_a_clear_422(db: PgPool) {
    let s = setup(db, true).await;
    let at = |path: &str| format!("http://{SITE}:{}{path}", s.port);
    assert_eq!(refused(&s, &at("/no-recipe")).await, "no_recipe_found");
    assert_eq!(refused(&s, &at("/json")).await, "source_not_html");
    assert_eq!(refused(&s, &at("/no-type")).await, "source_not_html");
    assert_eq!(refused(&s, &at("/broken")).await, "source_unreachable");
    assert_eq!(refused(&s, &at("/missing")).await, "source_unreachable");
}

#[sqlx::test]
async fn a_page_too_heavy_is_refused_announced_or_not(db: PgPool) {
    let s = setup(db, true).await;
    let at = |path: &str| format!("http://{SITE}:{}{path}", s.port);
    assert_eq!(refused(&s, &at("/heavy")).await, "source_too_large");
    assert_eq!(
        refused(&s, &at("/heavy-unannounced")).await,
        "source_too_large"
    );
}

#[sqlx::test]
async fn a_page_that_does_not_answer_in_time_is_a_timeout(db: PgPool) {
    let s = setup(db, true).await;
    let url = format!("http://{SITE}:{}/slow", s.port);
    assert_eq!(refused(&s, &url).await, "source_timeout");
}

#[sqlx::test]
async fn a_redirect_loop_stops(db: PgPool) {
    let s = setup(db, true).await;
    let url = format!("http://{SITE}:{}/loop", s.port);
    assert_eq!(refused(&s, &url).await, "source_unreachable");
    // The first request and `MAX_REDIRECTS` redirects, then no more.
    assert_eq!(
        s.hits.load(Ordering::SeqCst),
        1 + manage_our_home::public_destination::MAX_REDIRECTS
    );
}

#[sqlx::test]
async fn urls_that_are_not_http_are_refused(db: PgPool) {
    let s = setup(db, false).await;
    assert_eq!(refused(&s, "pas une adresse").await, "url_invalid");
    assert_eq!(refused(&s, "").await, "url_invalid");
    assert_eq!(
        refused(&s, "ftp://example.com/r").await,
        "url_scheme_refused"
    );
    assert_eq!(
        refused(&s, "file:///etc/passwd").await,
        "url_scheme_refused"
    );
}

/// SSRF: the production client, as it runs, never reaches the local
/// server — by its address, IPv4 or IPv6, by a private one, or by a name
/// that resolves to it.
#[sqlx::test]
async fn ssrf_local_and_private_destinations_are_refused(db: PgPool) {
    let s = setup(db, false).await;
    let port = s.port;
    for url in [
        format!("http://127.0.0.1:{port}/marmiton"),
        format!("http://[::1]:{port}/marmiton"),
        format!("http://localhost:{port}/marmiton"),
        "http://10.0.0.1/recette".to_string(),
        "http://192.168.1.1/recette".to_string(),
        "http://169.254.169.254/latest/meta-data/".to_string(),
    ] {
        assert_eq!(refused(&s, &url).await, "url_destination_refused", "{url}");
    }
    assert_eq!(
        s.hits.load(Ordering::SeqCst),
        0,
        "nothing reached the server"
    );
}

/// SSRF through a redirect: a public page that sends the server on to a
/// private address. Each hop is checked; only the first request lands.
#[sqlx::test]
async fn ssrf_a_redirect_to_a_private_destination_is_refused(db: PgPool) {
    let s = setup(db, true).await;
    for path in [
        "/to-loopback",
        "/to-private",
        "/to-ipv6-loopback",
        "/to-localhost",
    ] {
        let url = format!("http://{SITE}:{}{path}", s.port);
        assert_eq!(refused(&s, &url).await, "url_destination_refused", "{path}");
    }
    assert_eq!(
        refused(&s, &format!("http://{SITE}:{}/to-ftp", s.port)).await,
        "url_scheme_refused"
    );
    assert_eq!(
        s.hits.load(Ordering::SeqCst),
        5,
        "only the first hops landed"
    );
}

#[sqlx::test]
async fn a_member_is_limited_to_rate_limit_imports(db: PgPool) {
    let s = setup(db, true).await;
    let url = format!("http://{SITE}:{}/marmiton", s.port);
    for _ in 0..RATE_LIMIT {
        assert_status(&import(&s, &url).await, StatusCode::OK);
    }
    let res = import(&s, &url).await;
    assert_status(&res, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(s.hits.load(Ordering::SeqCst), RATE_LIMIT, "not fetched");
}

#[sqlx::test]
async fn a_non_member_cannot_import(db: PgPool) {
    let s = setup(db.clone(), true).await;
    let stranger = register_verify_login(&s.router, &db, "stranger@example.test").await;
    let res = call(
        &s.router,
        Method::POST,
        &format!("/groups/{}/recipes/import", s.group_id),
        Some(&stranger),
        Some(serde_json::json!({ "url": format!("http://{SITE}:{}/marmiton", s.port) })),
    )
    .await;
    assert_status(&res, StatusCode::FORBIDDEN);
    assert_eq!(s.hits.load(Ordering::SeqCst), 0);
}

#[sqlx::test]
async fn a_recipe_keeps_its_source_url(db: PgPool) {
    let s = setup(db, false).await;
    let recipes = format!("/groups/{}/recipes", s.group_id);
    let source = "https://www.marmiton.org/recettes/recette_crepes_27121.aspx";
    let res = call(
        &s.router,
        Method::POST,
        &recipes,
        Some(&s.cookie),
        Some(serde_json::json!({"name": "Crêpes", "source_url": format!("  {source} ")})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let created = json_body(res).await;
    assert_eq!(created["source_url"], source);
    let id = created["id"].as_str().unwrap();

    let got = json_body(
        call(
            &s.router,
            Method::GET,
            &format!("{recipes}/{id}"),
            Some(&s.cookie),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(got["source_url"], source);
    let list = json_body(call(&s.router, Method::GET, &recipes, Some(&s.cookie), None).await).await;
    assert_eq!(list["recipes"][0]["source_url"], source);

    // An edit leaves it in place.
    let res = call(
        &s.router,
        Method::PATCH,
        &format!("{recipes}/{id}"),
        Some(&s.cookie),
        Some(serde_json::json!({"name": "Crêpes au lait"})),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(json_body(res).await["source_url"], source);

    // A recipe typed by hand has none.
    let res = call(
        &s.router,
        Method::POST,
        &recipes,
        Some(&s.cookie),
        Some(serde_json::json!({"name": "Gaufres"})),
    )
    .await;
    assert_eq!(json_body(res).await["source_url"], serde_json::Value::Null);
}

#[sqlx::test]
async fn a_source_url_that_is_not_a_web_address_is_refused(db: PgPool) {
    let s = setup(db.clone(), false).await;
    let long = format!("https://e.fr/{}", "a".repeat(2048));
    for bad in [
        "javascript:alert(1)",
        "ftp://e.fr/r",
        "pas une url",
        "",
        long.as_str(),
    ] {
        let res = call(
            &s.router,
            Method::POST,
            &format!("/groups/{}/recipes", s.group_id),
            Some(&s.cookie),
            Some(serde_json::json!({"name": "Crêpes", "source_url": bad})),
        )
        .await;
        assert_status(&res, StatusCode::BAD_REQUEST);
        assert_eq!(json_body(res).await["error"], "invalid_source_url", "{bad}");
    }
    assert_eq!(recipe_count(&db).await, 0);
}
