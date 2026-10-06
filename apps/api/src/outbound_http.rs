//! The HTTP client for requests this API makes to third parties on behalf
//! of a user request: Google's OAuth token and userinfo endpoints, and the
//! iCal feeds of calendar imports. Without a timeout, a peer that accepts
//! the connection and never answers holds the user's request open
//! indefinitely (#371).

use std::sync::LazyLock;
use std::time::Duration;

/// Time allowed to establish the TCP (and TLS) connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Time allowed for the whole request, connection and body included.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

static CLIENT: LazyLock<reqwest::Client> =
    LazyLock::new(|| build(CONNECT_TIMEOUT, REQUEST_TIMEOUT));

/// The shared client. Built once, so its connection pool is reused across
/// requests.
pub fn client() -> &'static reqwest::Client {
    &CLIENT
}

fn build(connect: Duration, request: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(connect)
        .timeout(request)
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

    #[tokio::test]
    async fn a_peer_that_never_answers_fails_within_the_request_timeout() {
        let (url, peer) = silent_peer().await;
        let client = build(Duration::from_secs(1), Duration::from_millis(200));
        let outcome = tokio::time::timeout(Duration::from_secs(5), client.get(&url).send()).await;
        peer.abort();
        let error = outcome
            .expect("the client gave up on its own, before the 5 s guard")
            .expect_err("a silent peer is a failure");
        assert!(error.is_timeout(), "{error}");
    }
}
