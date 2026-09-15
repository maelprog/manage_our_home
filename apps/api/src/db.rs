//! Runtime database plumbing shared by every handler and job: how a
//! transaction is opened, and how the runtime pools configure their
//! connections. Issue #188.

use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool, Postgres, Transaction};

/// Server-side bound on how long a connection may sit idle inside an open
/// transaction before Postgres terminates it (`idle_in_transaction_session_timeout`).
///
/// A safety net, not the fix: [`begin`] closes the one leak path known
/// today. If another one ever appears, a leaked transaction holds its locks
/// for this long instead of for as long as the process lives.
///
/// Sized above the longest legitimate idle gap inside a transaction:
/// `agenda::attachments::upload_attachment` opens its transaction before
/// reading the multipart body (capped by axum's default 2 MB body limit)
/// and keeps it open across `put_object`. A transaction idle past this
/// bound loses its connection and the request fails.
const IDLE_IN_TRANSACTION_TIMEOUT: &str = "SET idle_in_transaction_session_timeout = '60s'";

/// Opens a transaction on `pool`, and leaves no transaction open on the
/// server if the caller is cancelled while waiting.
///
/// Use it instead of `pool.begin()`. In sqlx-postgres 0.9.0,
/// `PgTransactionManager::begin` sends `BEGIN` and only counts the
/// transaction as open once the server has answered. A future dropped
/// during that wait — which is what hyper does to a handler when the client
/// hangs up, e.g. a WebSocket handshake cut short by a navigation — has
/// nothing to roll back on sqlx's side, and the pool's release ping then
/// consumes the pending reply. The connection goes back to the pool inside
/// a transaction that sqlx believes closed and the server holds open. The
/// next autocommit write on it (`AuthUser`'s `UPDATE sessions SET
/// last_seen_at`) takes its row lock inside that transaction and never
/// releases it: every later request of that session waits on it.
///
/// Here the `BEGIN` runs in its own task. If the caller is dropped, the task
/// still completes, and the `Transaction` it produces is dropped with its
/// depth already counted: sqlx queues the `ROLLBACK`, and the release ping
/// sends it before the connection is reused.
pub async fn begin(pool: &PgPool) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let pool = pool.clone();
    match tokio::spawn(async move { pool.begin().await }).await {
        Ok(result) => result,
        Err(join_error) if join_error.is_panic() => {
            std::panic::resume_unwind(join_error.into_panic())
        }
        // Only when the runtime is shutting down.
        Err(_) => Err(sqlx::Error::WorkerCrashed),
    }
}

/// Pool options for the runtime pools (`DATABASE_URL`, `ADMIN_DATABASE_URL`).
/// Every connection they open carries [`IDLE_IN_TRANSACTION_TIMEOUT`].
pub fn pool_options() -> PgPoolOptions {
    PgPoolOptions::new().after_connect(|conn, _meta| {
        Box::pin(async move {
            conn.execute(IDLE_IN_TRANSACTION_TIMEOUT).await?;
            Ok(())
        })
    })
}
