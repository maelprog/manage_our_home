//! Issue #188: a transaction whose `BEGIN` is cancelled mid-flight must not
//! come back to the pool still open.
//!
//! sqlx-postgres 0.9.0 sends `BEGIN`, then waits for the server's reply
//! before counting the transaction as open. A future dropped during that
//! wait (a client that hangs up — hyper drops the handler) leaves nothing to
//! roll back on sqlx's side, and the pool's release ping swallows the
//! pending reply: the connection goes back to the pool inside a transaction
//! the server still holds open. The next autocommit write on it — the
//! `UPDATE sessions SET last_seen_at` of every authenticated request — takes
//! its row lock inside that transaction and never releases it, freezing
//! every later request of that session.
//!
//! Runtime queries (`sqlx::query`, not the macros) so this binary adds
//! nothing to the `.sqlx` offline cache.

use std::future::Future;
use std::pin::pin;
use std::task::Poll;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection, PgPool};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use uuid::Uuid;

/// How many times the future is polled before being dropped, swept from 1 up.
/// With `test_before_acquire(false)` and an idle connection, one poll is
/// enough to reach the wait on `BEGIN`'s reply; the upper values cover a
/// scheduler that needs a few more turns to get there.
const MAX_POLLS: usize = 8;
const ROUNDS: usize = 5;

/// One connection, no ping on acquire: the connection the next caller gets
/// is exactly the one the cancelled future released, with no round trip in
/// between that could hide its state.
async fn single_connection_pool(
    pool_opts: PgPoolOptions,
    connect_opts: PgConnectOptions,
) -> PgPool {
    pool_opts
        .max_connections(1)
        .min_connections(0)
        .test_before_acquire(false)
        .connect_with(connect_opts)
        .await
        .unwrap()
}

async fn wait_until_idle(pool: &PgPool) {
    for _ in 0..500 {
        if pool.num_idle() == 1 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    panic!("the pool's only connection never came back");
}

/// Drops `make()`'s future after `polls` polls, then reports whether the
/// connection the pool hands out next sits inside an open transaction, as
/// the server sees it (`pg_stat_activity`, read from a separate connection).
/// Returns `(dropped_while_pending, leaked)`.
async fn leaks_after_cancel<F, Fut, T>(
    pool: &PgPool,
    observer: &mut PgConnection,
    polls: usize,
    make: &F,
) -> (bool, bool)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, sqlx::Error>>,
{
    wait_until_idle(pool).await;

    let mut dropped_while_pending = false;
    {
        let mut fut = pin!(make());
        for i in 0..polls {
            let step = std::future::poll_fn(|cx| Poll::Ready(fut.as_mut().poll(cx))).await;
            match step {
                Poll::Ready(tx) => {
                    // Finished before the cut: an ordinary drop, which sqlx
                    // already rolls back.
                    drop(tx.unwrap());
                    dropped_while_pending = false;
                    break;
                }
                Poll::Pending => {
                    dropped_while_pending = true;
                    if i + 1 < polls {
                        tokio::task::yield_now().await;
                    }
                }
            }
        }
    }

    // Waits for the released connection to come back (single-connection pool).
    let mut conn = pool.acquire().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    let state: Option<String> =
        sqlx::query_scalar("SELECT state FROM pg_stat_activity WHERE pid = $1")
            .bind(pid)
            .fetch_one(&mut *observer)
            .await
            .unwrap();
    (
        dropped_while_pending,
        state.as_deref() == Some("idle in transaction"),
    )
}

/// Sweeps the cut point over `ROUNDS` x `MAX_POLLS` cancellations.
async fn assert_cancel_never_leaks<F, Fut, T>(
    pool: &PgPool,
    connect_opts: &PgConnectOptions,
    make: F,
) where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, sqlx::Error>>,
{
    let mut observer = PgConnection::connect_with(connect_opts).await.unwrap();

    let mut cancellations = 0;
    for round in 0..ROUNDS {
        for polls in 1..=MAX_POLLS {
            let (cancelled, leaked) = leaks_after_cancel(pool, &mut observer, polls, &make).await;
            assert!(
                !leaked,
                "connection returned to the pool inside an open transaction \
                 (round {round}, dropped after {polls} poll(s))"
            );
            if cancelled {
                cancellations += 1;
            }
        }
    }
    // Guards against a vacuous pass: some futures must actually have been
    // dropped before completing.
    assert!(cancellations > 0, "no future was dropped mid-flight");
}

#[sqlx::test]
async fn cancelled_scoped_tx_leaves_no_open_transaction(
    pool_opts: PgPoolOptions,
    connect_opts: PgConnectOptions,
) {
    let pool = single_connection_pool(pool_opts, connect_opts.clone()).await;
    let (family, user) = (Uuid::new_v4(), Uuid::new_v4());
    assert_cancel_never_leaks(&pool, &connect_opts, || {
        manage_our_home::auth::session::scoped_tx(&pool, family, user)
    })
    .await;
}

#[sqlx::test]
async fn cancelled_user_scoped_tx_leaves_no_open_transaction(
    pool_opts: PgPoolOptions,
    connect_opts: PgConnectOptions,
) {
    let pool = single_connection_pool(pool_opts, connect_opts.clone()).await;
    let user = Uuid::new_v4();
    assert_cancel_never_leaks(&pool, &connect_opts, || {
        manage_our_home::auth::session::user_scoped_tx(&pool, user)
    })
    .await;
}

#[sqlx::test]
async fn cancelled_token_scoped_tx_leaves_no_open_transaction(
    pool_opts: PgPoolOptions,
    connect_opts: PgConnectOptions,
) {
    let pool = single_connection_pool(pool_opts, connect_opts.clone()).await;
    let token = Uuid::new_v4();
    assert_cancel_never_leaks(&pool, &connect_opts, || {
        manage_our_home::auth::session::token_scoped_tx(&pool, token)
    })
    .await;
}

/// The replacement for the direct `pool.begin()` calls in handlers and jobs.
#[sqlx::test]
async fn cancelled_db_begin_leaves_no_open_transaction(
    pool_opts: PgPoolOptions,
    connect_opts: PgConnectOptions,
) {
    let pool = single_connection_pool(pool_opts, connect_opts.clone()).await;
    assert_cancel_never_leaks(&pool, &connect_opts, || manage_our_home::db::begin(&pool)).await;
}

/// A runtime pool, one connection, no ping on acquire.
async fn single_runtime_connection_pool(connect_opts: PgConnectOptions) -> PgPool {
    manage_our_home::db::pool_options()
        .max_connections(1)
        .min_connections(0)
        .test_before_acquire(false)
        .connect_with(connect_opts)
        .await
        .unwrap()
}

async fn backend_state(observer: &mut PgConnection, pid: i32) -> Option<String> {
    sqlx::query_scalar("SELECT state FROM pg_stat_activity WHERE pid = $1")
        .bind(pid)
        .fetch_optional(&mut *observer)
        .await
        .unwrap()
        .flatten()
}

async fn pool_backend_pid(pool: &PgPool) -> i32 {
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *conn)
        .await
        .unwrap()
}

/// A TCP relay between a pool and Postgres whose server-to-client direction
/// can be held. While it is held the server still receives and runs what
/// the client sends, but its replies wait in the relay.
///
/// This is what pins a cut point between "`BEGIN` has run on the server"
/// and "sqlx has read its reply" (#199): with the reply held, the future
/// cannot complete whatever the scheduler does, and the server's own state
/// says when `BEGIN` has run. Dropping the future after a single poll
/// instead raced that reply over loopback, and lost.
struct ReplyGate {
    open: watch::Sender<bool>,
}

impl ReplyGate {
    fn hold(&self) {
        self.open.send_replace(false);
    }

    fn release(&self) {
        self.open.send_replace(true);
    }
}

/// Starts a relay to `upstream`'s server and returns options that connect
/// through it, with the gate open.
async fn gated_relay(upstream: &PgConnectOptions) -> (PgConnectOptions, ReplyGate) {
    // sqlx also goes through a Unix socket when the host is a path
    // (`postgres:///db` defaults it to `/var/run/postgresql`), not only
    // when `socket` is set.
    assert!(
        upstream.get_socket().is_none() && !upstream.get_host().starts_with('/'),
        "the relay only speaks TCP, and DATABASE_URL points at a Unix socket"
    );
    // A (host, port) pair rather than "host:port", which an IPv6 literal
    // would break; the brackets a URL puts around one are dropped.
    let host = upstream.get_host();
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let target = (host.to_owned(), upstream.get_port());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (open, gate) = watch::channel(true);

    tokio::spawn(async move {
        while let Ok((client, _)) = listener.accept().await {
            let Ok(server) = TcpStream::connect(&target).await else {
                return;
            };
            tokio::spawn(relay_connection(client, server, gate.clone()));
        }
    });

    (
        upstream.clone().host("127.0.0.1").port(port),
        ReplyGate { open },
    )
}

async fn relay_connection(client: TcpStream, server: TcpStream, mut gate: watch::Receiver<bool>) {
    let (mut client_rx, mut client_tx) = client.into_split();
    let (mut server_rx, mut server_tx) = server.into_split();

    let requests = async move {
        let _ = tokio::io::copy(&mut client_rx, &mut server_tx).await;
        let _ = server_tx.shutdown().await;
    };
    let replies = async move {
        let mut buf = vec![0u8; 8192];
        loop {
            let n = match server_rx.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            // Checked after the read, so a reply that arrives while the gate
            // is held waits here rather than slipping through.
            if gate.wait_for(|open| *open).await.is_err() {
                break;
            }
            if client_tx.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
        let _ = client_tx.shutdown().await;
    };
    tokio::join!(requests, replies);
}

/// Waits until the server reports backend `pid` in `state`.
async fn wait_for_backend_state(observer: &mut PgConnection, pid: i32, state: &str) {
    for _ in 0..250 {
        if backend_state(observer, pid).await.as_deref() == Some(state) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("backend {pid} never reached `{state}` within five seconds");
}

/// The safety net, which has to hold even if the leak is reopened: a
/// connection that comes back to the pool inside a transaction is ended
/// there and then, so the server rolls the transaction back and releases
/// its locks.
///
/// The leak is reopened on purpose here — this is `pool.begin()`, the
/// pre-#188 path, not `db::begin` — so the bound is measured on a live
/// leak rather than assumed. The pool goes through [`gated_relay`], which
/// holds the reply to `BEGIN` so the future is always dropped at the same
/// point: after the server has opened the transaction, before sqlx knows.
#[sqlx::test]
async fn a_connection_returned_inside_a_transaction_is_ended(
    _pool_opts: PgPoolOptions,
    connect_opts: PgConnectOptions,
) {
    let (relayed_opts, gate) = gated_relay(&connect_opts).await;
    let pool = single_runtime_connection_pool(relayed_opts).await;
    let mut observer = PgConnection::connect_with(&connect_opts).await.unwrap();

    let leaked_pid = pool_backend_pid(&pool).await;
    wait_until_idle(&pool).await;

    gate.hold();
    {
        let mut fut = pin!(pool.begin());
        tokio::select! {
            biased;
            _ = &mut fut => panic!("the `BEGIN` completed although its reply was held"),
            () = wait_for_backend_state(&mut observer, leaked_pid, "idle in transaction") => {}
        }
    }
    // The future is gone with the reply still in the relay: let it through
    // to the pool's release ping, as it would have arrived without the cut.
    gate.release();

    // The leaked backend must be gone, not merely idle: nothing else in the
    // process is going to end that transaction.
    for i in 0.. {
        match backend_state(&mut observer, leaked_pid).await {
            None => break,
            Some(state) => {
                assert!(
                    i < 250,
                    "backend {leaked_pid} still alive ({state}) five seconds after the leak"
                );
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }
    }

    // And the pool still hands out a working connection.
    let fresh_pid = pool_backend_pid(&pool).await;
    assert_ne!(fresh_pid, leaked_pid);
    assert_ne!(
        backend_state(&mut observer, fresh_pid).await.as_deref(),
        Some("idle in transaction")
    );
}

/// The counterpart: the check must not churn healthy connections. Plain
/// queries, committed transactions and rolled-back ones all keep the same
/// backend.
#[sqlx::test]
async fn runtime_pool_keeps_connections_that_come_back_clean(
    _pool_opts: PgPoolOptions,
    connect_opts: PgConnectOptions,
) {
    let pool = single_runtime_connection_pool(connect_opts).await;
    let pid = pool_backend_pid(&pool).await;

    sqlx::query("SELECT 1").execute(&pool).await.unwrap();
    assert_eq!(pool_backend_pid(&pool).await, pid, "after a plain query");

    let mut tx = manage_our_home::db::begin(&pool).await.unwrap();
    sqlx::query("SELECT 1").execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(pool_backend_pid(&pool).await, pid, "after a commit");

    let mut tx = manage_our_home::db::begin(&pool).await.unwrap();
    sqlx::query("SELECT 1").execute(&mut *tx).await.unwrap();
    drop(tx);
    assert_eq!(pool_backend_pid(&pool).await, pid, "after a rollback");
}
