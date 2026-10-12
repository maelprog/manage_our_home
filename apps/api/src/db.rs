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
/// nothing to roll back on sqlx's side, and the first query of the pool's
/// release path then consumes the pending reply (the `after_release` query
/// of [`pool_options`], which sqlx runs before its release ping, or that
/// ping on a pool without the hook). Without the `after_release` check, the
/// connection goes back to the pool inside a transaction that sqlx believes
/// closed and the server holds open. The next autocommit write on it
/// (`AuthUser`'s `UPDATE sessions SET last_seen_at`) takes its row lock
/// inside that transaction and never releases it: every later request of
/// that session waits on it.
///
/// Here the `BEGIN` runs in its own task. If the caller is dropped, the task
/// still completes, and the `Transaction` it produces is dropped with its
/// depth already counted: sqlx queues the `ROLLBACK`, and the first query
/// of the release path (the `after_release` one, or the ping) sends it
/// before the connection is reused.
///
/// Note the two costs of running the `BEGIN` elsewhere: a caller that is
/// dropped keeps its place in the pool's acquire queue until the task is
/// done (bounded by the pool's acquire timeout), and the task carries the
/// caller's tracing span explicitly rather than by being its child.
///
/// The crate's `clippy.toml` refuses a direct `begin` anywhere else (#191).
pub async fn begin(pool: &PgPool) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let pool = pool.clone();
    #[allow(clippy::disallowed_methods)]
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

/// Pool options for the runtime pools (`DATABASE_URL`, `ADMIN_DATABASE_URL`,
/// and the rate limits' own, [`pool_beside`]: the login throttle's,
/// `auth::throttle::LoginThrottle::new`, and those of #483).
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

/// The size the environment variable `var`, of value `raw`, asks for a
/// pool: `default` when unset or blank, otherwise a whole number, at least
/// one. Anything else is refused rather than replaced by the default, so a
/// typo stops the API at start instead of leaving the pool at a size
/// nobody chose (#482, #483).
pub fn pool_size(var: &str, raw: Option<&str>, default: u32) -> Result<u32, String> {
    let raw = raw.map(str::trim).unwrap_or("");
    if raw.is_empty() {
        return Ok(default);
    }
    match raw.parse::<u32>() {
        Ok(size) if size > 0 => Ok(size),
        _ => Err(format!(
            "{var}={raw:?}: expected a whole number of connections, at least 1 \
             (unset means {default})"
        )),
    }
}

/// A pool of its own beside the handlers' pool `handlers`, of `size`
/// connections: on the same server and as the same role, with `options`,
/// opened on first use. What the rate limits wait on with a connection
/// held — an advisory lock per counted subject — waits there, so a burst
/// on one of them cannot take the connections every route needs (#454,
/// #483).
pub fn pool_beside(options: PgPoolOptions, handlers: &PgPool, size: u32) -> PgPool {
    options
        .max_connections(size)
        .connect_lazy_with((*handlers.connect_options()).clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VAR: &str = "SOME_POOL_SIZE";

    #[test]
    fn a_pool_size_defaults_when_unset_or_blank() {
        assert_eq!(pool_size(VAR, None, 3), Ok(3));
        assert_eq!(pool_size(VAR, Some(""), 3), Ok(3));
        assert_eq!(pool_size(VAR, Some("  "), 7), Ok(7));
    }

    #[test]
    fn a_pool_size_is_read_as_a_whole_number() {
        assert_eq!(pool_size(VAR, Some("1"), 3), Ok(1));
        assert_eq!(pool_size(VAR, Some("12"), 3), Ok(12));
        // Surrounding space, as an env file may leave it, is not an error.
        assert_eq!(pool_size(VAR, Some(" 8 "), 3), Ok(8));
    }

    #[test]
    fn an_unusable_pool_size_names_the_variable_the_value_and_the_default() {
        for raw in [
            "0",
            "-1",
            "four",
            "4.5",
            "4 connections",
            "0x4",
            "99999999999",
        ] {
            let err = pool_size(VAR, Some(raw), 3).unwrap_err();
            assert!(err.contains(VAR), "{raw}: {err}");
            assert!(err.contains(raw), "{raw}: {err}");
            assert!(err.contains("unset means 3"), "{raw}: {err}");
        }
    }
}
