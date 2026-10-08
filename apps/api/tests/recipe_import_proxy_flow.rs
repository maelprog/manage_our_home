//! The recipe import client ignores the proxies the environment names
//! (#405): a proxy's address would be resolved and connected to instead of
//! the page's, out of reach of `PublicOnlyResolver`.
//!
//! A test binary of its own: it sets `HTTP_PROXY` and `ALL_PROXY` for the
//! whole process, which no other test may see.

use std::time::Duration;

use manage_our_home::outbound_http::recipe_import_builder;

#[tokio::test]
async fn the_import_client_does_not_go_through_an_environment_proxy() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().fallback(|| async { "page" });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // Nothing listens there: going through it fails.
    for name in ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
        std::env::set_var(name, "http://127.0.0.1:1");
    }
    std::env::remove_var("NO_PROXY");
    std::env::remove_var("no_proxy");

    let client = recipe_import_builder()
        .resolve("recettes.test", addr)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let response = client
        .get(format!("http://recettes.test:{}/", addr.port()))
        .send()
        .await
        .expect("reached the page directly, not through the proxy");
    assert_eq!(response.text().await.unwrap(), "page");
}
