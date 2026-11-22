//! Issue #188: a transaction whose `BEGIN` is cancelled mid-flight must not
//! come back to the pool still open.
//!
//! sqlx-postgres 0.9.0 sent `BEGIN`, then waited for the server's reply
//! before counting the transaction as open. A future dropped during that
//! wait (a client that hangs up — hyper drops the handler) left nothing to
//! roll back on sqlx's side, and the first query of the pool's release path
//! swallowed the pending reply — the `after_release` query of
//! `db::pool_options`, which sqlx runs before its own release ping (the ping
//! did it on a pool without that hook). Without the hook's check, the
//! connection went back to the pool inside a transaction the server still
//! held open. The next autocommit write on it — the `UPDATE sessions SET
//! last_seen_at` of every authenticated request — took its row lock inside
//! that transaction and never released it, freezing every later request of
//! that session.
//!
//! sqlx-postgres 0.9.1 fixed that upstream (launchbadge/sqlx#4394); these
//! tests keep guarding against it.
//!
//! Runtime queries (`sqlx::query`, not the macros) so this binary adds
//! nothing to the `.sqlx` offline cache.

use std::future::Future;
use std::pin::pin;
use std::task::Poll;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection, PgPool};
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
    let token_hash = manage_our_home::auth::token::new_token().hash();
    assert_cancel_never_leaks(&pool, &connect_opts, || {
        manage_our_home::auth::session::token_scoped_tx(&pool, &token_hash)
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

/// The safety net, which has to hold whatever leaves the server inside a
/// transaction that sqlx does not know about: a connection that comes back
/// to the pool that way is ended there and then, so the server rolls the
/// transaction back and releases its locks.
///
/// The leak is made by hand — a `BEGIN` sent with `raw_sql`, which sqlx
/// does not count as a transaction — so the bound is measured on a live
/// leak rather than assumed. Up to sqlx-postgres 0.9.0, a `pool.begin()`
/// cancelled while waiting for `BEGIN`'s reply made this same leak; 0.9.1
/// fixed it upstream (launchbadge/sqlx#4394), and this check is what still
/// stands if anything reopens it.
#[sqlx::test]
async fn a_connection_returned_inside_a_transaction_is_ended(
    _pool_opts: PgPoolOptions,
    connect_opts: PgConnectOptions,
) {
    let pool = single_runtime_connection_pool(connect_opts.clone()).await;
    let mut observer = PgConnection::connect_with(&connect_opts).await.unwrap();

    let leaked_pid = {
        let mut conn = pool.acquire().await.unwrap();
        sqlx::raw_sql("BEGIN").execute(&mut *conn).await.unwrap();
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(
            backend_state(&mut observer, pid).await.as_deref(),
            Some("idle in transaction"),
            "the hand-made leak must hold a transaction open on the server"
        );
        pid
    };

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
