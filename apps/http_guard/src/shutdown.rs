//! Stopping a process without cutting what it is serving (#424).
//!
//! On SIGTERM — what Kubernetes sends a pod it is about to stop — or
//! Ctrl-C, [`serve`] stops accepting connections, lets the requests in
//! flight finish, and waits for every [`Hold`] (an open Messagerie
//! WebSocket in apps/api) to be released, all within a bounded grace
//! period. [`Shutdown`] is what the rest of the process sees of it:
//! `/readyz` answers 503 once it is draining, and a WebSocket closes with a
//! Close frame instead of being dropped with the process.
//!
//! The grace period must end before the orchestrator's own: Kubernetes
//! kills the container `terminationGracePeriodSeconds` (30 s by default)
//! after SIGTERM, hence [`DEFAULT_GRACE`].

use std::future::{Future, IntoFuture};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::watch;

/// The variable that sets the grace period, in whole seconds.
pub const GRACE_VAR: &str = "SHUTDOWN_GRACE_SECONDS";

/// The grace period when [`GRACE_VAR`] is unset: under Kubernetes' default
/// `terminationGracePeriodSeconds` of 30 s, with room to spare for the
/// process to exit once it is over.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(25);

/// The grace period [`GRACE_VAR`] asks for: [`DEFAULT_GRACE`] when unset or
/// blank, otherwise a whole number of seconds, at least one. Anything else
/// is refused rather than replaced by the default — a typo would otherwise
/// let a stop cut requests short, or outlast the orchestrator's patience,
/// without anyone noticing.
pub fn grace_period(raw: Option<&str>) -> Result<Duration, String> {
    let raw = raw.map(str::trim).unwrap_or("");
    if raw.is_empty() {
        return Ok(DEFAULT_GRACE);
    }
    match raw.parse::<u64>() {
        Ok(secs) if secs > 0 => Ok(Duration::from_secs(secs)),
        _ => Err(format!(
            "{GRACE_VAR}={raw:?}: expected a whole number of seconds, at least 1"
        )),
    }
}

/// Whether the process is stopping, shared by everything that has to
/// behave differently once it is. Cheap to clone.
#[derive(Clone)]
pub struct Shutdown {
    inner: Arc<Inner>,
}

struct Inner {
    /// `true` from the moment the stop begins.
    draining: watch::Sender<bool>,
    /// One receiver per live [`Hold`]; [`watch::Sender::closed`] resolves
    /// once there are none.
    holds: watch::Sender<()>,
}

/// Keeps the process from exiting before it is dropped, within the grace
/// period. Taken by what outlives the request that started it — the
/// Messagerie WebSocket — which axum's graceful shutdown does not wait for.
pub struct Hold {
    _held: watch::Receiver<()>,
}

impl Default for Shutdown {
    fn default() -> Self {
        Self::new()
    }
}

impl Shutdown {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                draining: watch::channel(false).0,
                holds: watch::channel(()).0,
            }),
        }
    }

    /// Starts the stop. Idempotent.
    pub fn trigger(&self) {
        self.inner.draining.send_replace(true);
    }

    /// Whether the stop has begun.
    pub fn is_draining(&self) -> bool {
        *self.inner.draining.borrow()
    }

    /// Resolves once the stop has begun, at once if it already has.
    pub async fn wait(&self) {
        let mut draining = self.inner.draining.subscribe();
        // Cannot fail: `self` keeps the sender alive.
        let _ = draining.wait_for(|draining| *draining).await;
    }

    /// A [`Hold`] on the process, released on drop.
    pub fn hold(&self) -> Hold {
        Hold {
            _held: self.inner.holds.subscribe(),
        }
    }

    /// Runs `server` — an axum server whose graceful shutdown is tied to
    /// [`Shutdown::wait`] — to its end, then waits for every [`Hold`] to
    /// be released. Once the stop has begun, gives the whole of it at most
    /// `grace`, and returns when that runs out, whatever is still open.
    pub async fn drain<S>(&self, server: S, grace: Duration) -> std::io::Result<()>
    where
        S: IntoFuture<Output = std::io::Result<()>>,
    {
        let done = async {
            server.await?;
            self.inner.holds.closed().await;
            Ok(())
        };
        let deadline = async {
            self.wait().await;
            tokio::time::sleep(grace).await;
        };
        tokio::select! {
            done = done => done,
            () = deadline => {
                tracing::warn!(
                    ?grace,
                    "grace period over: exiting with requests or WebSockets still open"
                );
                Ok(())
            }
        }
    }
}

/// Resolves on SIGTERM or Ctrl-C.
pub async fn terminate_signal() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut term) => {
                term.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
}

/// Serves `app` on `listener` until `stop` resolves, then stops the way
/// [`Shutdown`] describes: `shutdown` turns draining, the listener stops
/// accepting, and the requests in flight and the [`Hold`]s get `grace` to
/// end. The peer address goes into each request's extensions
/// (`ConnectInfo`), which both apps' client-address resolution reads
/// (#178).
pub async fn serve(
    listener: TcpListener,
    app: Router,
    shutdown: Shutdown,
    stop: impl Future<Output = ()> + Send + 'static,
    grace: Duration,
) -> std::io::Result<()> {
    let trigger = shutdown.clone();
    tokio::spawn(async move {
        stop.await;
        trigger.trigger();
    });
    let draining = shutdown.clone();
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move { draining.wait().await });
    shutdown.drain(server, grace).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- grace_period ------------------------------------------------------

    #[test]
    fn unset_or_blank_is_the_default() {
        assert_eq!(grace_period(None), Ok(DEFAULT_GRACE));
        assert_eq!(grace_period(Some("")), Ok(DEFAULT_GRACE));
        assert_eq!(grace_period(Some("  ")), Ok(DEFAULT_GRACE));
    }

    #[test]
    fn the_default_ends_before_kubernetes_kills_the_pod() {
        assert!(DEFAULT_GRACE < Duration::from_secs(30));
    }

    #[test]
    fn a_whole_number_of_seconds_is_taken() {
        assert_eq!(grace_period(Some("1")), Ok(Duration::from_secs(1)));
        assert_eq!(grace_period(Some(" 55 ")), Ok(Duration::from_secs(55)));
    }

    #[test]
    fn zero_and_garbage_are_refused() {
        for raw in ["0", "-5", "2.5", "25s", "abc", "99999999999999999999999"] {
            assert!(grace_period(Some(raw)).is_err(), "{raw:?}");
        }
    }

    // -- Shutdown ----------------------------------------------------------

    #[tokio::test]
    async fn draining_starts_at_the_trigger_and_stays() {
        let shutdown = Shutdown::new();
        assert!(!shutdown.is_draining());
        let clone = shutdown.clone();
        clone.trigger();
        assert!(shutdown.is_draining());
        clone.trigger();
        assert!(shutdown.is_draining());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_resolves_at_the_trigger_and_after_it() {
        let shutdown = Shutdown::new();
        let waiter = tokio::spawn({
            let shutdown = shutdown.clone();
            async move { shutdown.wait().await }
        });
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(!waiter.is_finished());
        shutdown.trigger();
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("wait did not resolve at the trigger")
            .unwrap();
        // Already draining: at once.
        tokio::time::timeout(Duration::from_millis(1), shutdown.wait())
            .await
            .expect("wait did not resolve after the trigger");
    }

    /// A server that ends `after` the stop begins, like axum's once its
    /// connections are done.
    fn server_ending(
        shutdown: &Shutdown,
        after: Duration,
    ) -> impl Future<Output = std::io::Result<()>> {
        let shutdown = shutdown.clone();
        async move {
            shutdown.wait().await;
            tokio::time::sleep(after).await;
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn drain_runs_the_server_until_the_stop() {
        let shutdown = Shutdown::new();
        let drain = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                let server = server_ending(&shutdown, Duration::ZERO);
                shutdown.drain(server, Duration::from_secs(5)).await
            }
        });
        tokio::time::sleep(Duration::from_secs(3600)).await;
        assert!(!drain.is_finished(), "drained before any stop");
        shutdown.trigger();
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert!(drain.is_finished());
        drain.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn drain_waits_for_the_requests_in_flight() {
        let shutdown = Shutdown::new();
        shutdown.trigger();
        let start = tokio::time::Instant::now();
        let server = server_ending(&shutdown, Duration::from_secs(3));
        shutdown.drain(server, Duration::from_secs(5)).await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(3));
    }

    #[tokio::test(start_paused = true)]
    async fn drain_waits_for_the_holds() {
        let shutdown = Shutdown::new();
        let hold = shutdown.hold();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            drop(hold);
        });
        shutdown.trigger();
        let start = tokio::time::Instant::now();
        let server = server_ending(&shutdown, Duration::ZERO);
        shutdown.drain(server, Duration::from_secs(5)).await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn drain_gives_up_at_the_end_of_the_grace_period() {
        let shutdown = Shutdown::new();
        let _never_released = shutdown.hold();
        shutdown.trigger();
        let start = tokio::time::Instant::now();
        let server = server_ending(&shutdown, Duration::from_secs(3600));
        shutdown.drain(server, Duration::from_secs(5)).await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(5));
    }

    #[tokio::test(start_paused = true)]
    async fn the_grace_period_counts_from_the_stop() {
        let shutdown = Shutdown::new();
        let drain = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                let _never_released = shutdown.hold();
                let server = server_ending(&shutdown, Duration::ZERO);
                shutdown.drain(server, Duration::from_secs(5)).await
            }
        });
        tokio::time::sleep(Duration::from_secs(60)).await;
        shutdown.trigger();
        let start = tokio::time::Instant::now();
        drain.await.unwrap().unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(5));
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_error_is_returned_at_once() {
        let shutdown = Shutdown::new();
        let _never_released = shutdown.hold();
        let server = async { Err(std::io::Error::other("bind lost")) };
        let err = shutdown
            .drain(server, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "bind lost");
    }
}
