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

/// The safety net: a connection left idle inside a transaction, whatever
/// the path that left it there, is ended by the server instead of holding
/// its locks for as long as the process lives.
#[sqlx::test]
async fn runtime_pool_bounds_idle_in_transaction(
    _pool_opts: PgPoolOptions,
    connect_opts: PgConnectOptions,
) {
    let pool = manage_our_home::db::pool_options()
        .max_connections(1)
        .connect_with(connect_opts)
        .await
        .unwrap();
    let setting: String = sqlx::query_scalar(
        "SELECT setting FROM pg_settings WHERE name = 'idle_in_transaction_session_timeout'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(setting, "60000", "milliseconds");
}
