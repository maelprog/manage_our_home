//! The HTTP clients for requests this API makes to third parties on behalf
//! of a user request: Google's OAuth token and userinfo endpoints, and the
//! iCal feeds of calendar imports. Without a timeout, a peer that accepts
//! the connection and never answers holds the user's request open
//! indefinitely (#371).
//!
//! The OAuth calls get a client of their own that follows no redirect: on a
//! 307 or 308, reqwest replays the code exchange's POST body, authorization
//! code and PKCE verifier included, at whatever the `Location` names, and
//! the `client_secret` too when the redirect stays on the same host (it
//! drops the `Authorization` header on a change of host or port) (#389).
//! The `oauth2` crate's documentation advises the same for the code
//! exchange. The iCal import keeps following redirects, feeds move.
//!
//! The recipe import (#405) fetches a page at an address the member typed:
//! its client only reaches public addresses (`public_destination`), checks
//! each redirect the same way, and ignores any proxy configured in the
//! environment, whose address would be resolved instead of the page's.

use std::sync::LazyLock;
use std::time::Duration;

/// Time allowed to establish the TCP (and TLS) connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Time allowed for the whole request, connection and body included.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    build(
        CONNECT_TIMEOUT,
        REQUEST_TIMEOUT,
        reqwest::redirect::Policy::default(),
    )
});

static OAUTH_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    build(
        CONNECT_TIMEOUT,
        REQUEST_TIMEOUT,
        reqwest::redirect::Policy::none(),
    )
});

/// The shared client, which follows redirects (reqwest's default, up to
/// 10). Built once, so its connection pool is reused across requests.
pub fn client() -> &'static reqwest::Client {
    &CLIENT
}

/// The client for Google's OAuth endpoints: same timeouts, no redirect
/// followed. A redirect comes back as the 3xx response itself.
pub fn oauth_client() -> &'static reqwest::Client {
    &OAUTH_CLIENT
}

static RECIPE_IMPORT_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    recipe_import_builder()
        .build()
        .expect("static reqwest configuration")
});

/// The client for the recipe import: same timeouts, public destinations
/// only, each redirect checked like the first request
/// (`public_destination`).
pub fn recipe_import_client() -> &'static reqwest::Client {
    &RECIPE_IMPORT_CLIENT
}

/// The recipe import client's configuration. Public so that the flow tests
/// build the same client, and pin a test name to their local server with
/// `ClientBuilder::resolve` — a pin set in the tests, never here.
pub fn recipe_import_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(crate::public_destination::redirect_policy())
        .dns_resolver(std::sync::Arc::new(
            crate::public_destination::PublicOnlyResolver,
        ))
        .no_proxy()
}

fn build(
    connect: Duration,
    request: Duration,
    redirect: reqwest::redirect::Policy,
) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(connect)
        .timeout(request)
        .redirect(redirect)
        .build()
        .expect("static reqwest configuration")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// A peer that accepts the connection, then never answers.
    async fn silent_peer() -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let mut held = Vec::new();
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                held.push(socket);
            }
        });
        (url, handle)
    }

    /// reqwest exposes no getter for its timeouts; its `Debug` output names
    /// the total one (`TotalTimeout` as of 0.12). That timeout bounds the
    /// connection too, so a client that lost it fails here. The connect
    /// timeout does not show, and is not checked.
    #[test]
    fn both_clients_carry_the_request_timeout() {
        for client in [client(), oauth_client(), recipe_import_client()] {
            let shown = format!("{client:?}");
            assert!(
                shown.contains(&format!("TotalTimeout: {REQUEST_TIMEOUT:?}")),
                "{shown}"
            );
        }
    }

    /// Every outbound call goes through `client()`, `oauth_client()` or
    /// `recipe_import_client()`: a client built anywhere else in `src/`
    /// would come without these timeouts. `push.rs` keeps its own, with its
    /// own timeout and redirect policy.
    #[test]
    fn no_other_reqwest_client_is_built_in_the_api_sources() {
        // A line naming `reqwest` and building a client. This file and
        // `push.rs` are the two that may.
        let forbidden = ["Client::new()", "Client::builder()"];
        let allowed = ["outbound_http.rs", "push.rs"];
        let mut offenders = Vec::new();
        let mut dirs = vec![std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src"
        ))];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if !name.ends_with(".rs") || allowed.contains(&name.as_str()) {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                for (n, line) in source.lines().enumerate() {
                    if line.contains("reqwest") && forbidden.iter().any(|f| line.contains(f)) {
                        offenders.push(format!("{}:{}", path.display(), n + 1));
                    }
                }
            }
        }
        assert!(offenders.is_empty(), "{offenders:?}");
    }

    #[tokio::test]
    async fn a_peer_that_never_answers_fails_within_the_request_timeout() {
        let (url, peer) = silent_peer().await;
        let client = build(
            Duration::from_secs(1),
            Duration::from_millis(200),
            reqwest::redirect::Policy::default(),
        );
        let outcome = tokio::time::timeout(Duration::from_secs(5), client.get(&url).send()).await;
        peer.abort();
        let error = outcome
            .expect("the client gave up on its own, before the 5 s guard")
            .expect_err("a silent peer is a failure");
        assert!(error.is_timeout(), "{error}");
    }
}
