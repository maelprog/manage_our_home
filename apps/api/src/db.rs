//! Runtime database plumbing shared by every handler and job: how a
//! transaction is opened, and how the runtime pools configure their
//! connections. Issue #188.

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Postgres, Row, Transaction};
use tracing::Instrument;

/// Asks the server whether it still holds a transaction block open on this
/// connection. Sent with the simple query protocol (`raw_sql`), which is
/// what makes the answer exact: outside a transaction block Postgres opens
/// an implicit one for this very statement and gives it this statement's
/// own start timestamp, so the two timestamps are equal by construction.
/// Inside a block they differ by at least the round trip that opened it.
///
/// sqlx's own view (`transaction_depth`) cannot answer this: the leak
/// [`begin`] guards against is precisely the case where the server and
/// sqlx disagree, and `PgConnection::in_transaction`, which reads the
/// server's answer, is private to the crate.
const LEFT_IN_TRANSACTION: &str = "SELECT transaction_timestamp() <> statement_timestamp()";

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
///
/// Note the two costs of running the `BEGIN` elsewhere: a caller that is
/// dropped keeps its place in the pool's acquire queue until the task is
/// done (bounded by the pool's acquire timeout), and the task carries the
/// caller's tracing span explicitly rather than by being its child.
pub async fn begin(pool: &PgPool) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let pool = pool.clone();
    let task = tokio::spawn(async move { pool.begin().await }.in_current_span());
    match task.await {
        Ok(result) => result,
        Err(join_error) if join_error.is_panic() => {
            std::panic::resume_unwind(join_error.into_panic())
        }
        // Only when the runtime is shutting down.
        Err(_) => Err(sqlx::Error::WorkerCrashed),
    }
}

/// Pool options for the runtime pools (`DATABASE_URL`, `ADMIN_DATABASE_URL`).
///
/// A connection that comes back to the pool with a transaction still open
/// on the server is closed instead of being reused, which ends that
/// transaction and releases its locks. This is the bound on any leak
/// [`begin`] does not cover: without it, such a connection is handed out
/// again for autocommit queries, each of which runs *inside* the leaked
/// transaction, so the leak lives on — for as long as the connection keeps
/// being reused, no matter what `idle_in_transaction_session_timeout` is
/// set to, since every query resets that idle counter.
///
/// The check is one extra round trip per release, next to the ping sqlx
/// already does there.
pub fn pool_options() -> PgPoolOptions {
    PgPoolOptions::new().after_release(|conn, _meta| {
        Box::pin(async move {
            let left_in_transaction: bool = sqlx::raw_sql(LEFT_IN_TRANSACTION)
                .fetch_one(&mut *conn)
                .await?
                .try_get(0)?;
            if left_in_transaction {
                tracing::warn!(
                    "connection returned to the pool inside an open transaction: closing it"
                );
            }
            Ok(!left_in_transaction)
        })
    })
}
