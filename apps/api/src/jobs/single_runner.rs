//! One process at a time per background job (#426).
//!
//! Every api process starts the same background loops (`main.rs`). With
//! several replicas on one database, each pass ran once per replica: the
//! reminder pass read the due notifications, sent them, and only then
//! marked them sent, so every replica sent every reminder; the purges and
//! the attachment sweep ran side by side.
//!
//! Each pass now runs under a Postgres session advisory lock, one key per
//! job ([`Job::lock_key`]), taken with `pg_try_advisory_lock`: the replica
//! that gets it runs the pass, the others skip theirs until their next
//! tick. No other service is involved.
//!
//! The lock is held on a connection of its own, opened for the pass outside
//! the pool and closed after it. Closing the session is what releases the
//! lock, so nothing has to unlock it by hand, and a session that ends —
//! the process killed, the pass's future dropped — releases it too:
//! another replica takes it on its next tick. A connection cut silently (a
//! network partition, a node lost) ends its session only once the server
//! notices, which the lock's session bounds to a minute
//! ([`session_settings`], [`silent_cut_release_bound`]). Opening it outside
//! the pool also keeps it from holding one of the admin pool's five
//! connections while the pass itself needs them.

use std::future::Future;
use std::time::Duration;

use sqlx::postgres::PgConnectOptions;
use sqlx::{ConnectOptions, Connection, PgPool};

/// The background jobs that must not run in two processes at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    /// `jobs::attachment_reconcile`'s daily sweep.
    AttachmentReconcile,
    /// `jobs::account_purge`'s pass: deactivation notices, purge, ownership
    /// notices.
    AccountPurge,
    /// `jobs::retention_purge`'s hourly pass.
    RetentionPurge,
    /// `jobs::scheduled_notifications`'s sending pass.
    ReminderSend,
    /// `jobs::scheduled_notifications`'s refill of recurring reminders.
    ReminderRefill,
    /// `jobs::stock_expiry`'s hourly pass (#534).
    StockExpiry,
}

/// The high bytes of every key, `mom-job` in ASCII: the advisory lock
/// space is shared with whatever else takes such locks on the database
/// (sqlx's migrator does), and a key of `1` is the first anyone picks.
pub const LOCK_NAMESPACE: i64 = i64::from_be_bytes(*b"mom-job\0");

impl Job {
    pub const ALL: [Job; 6] = [
        Job::AttachmentReconcile,
        Job::AccountPurge,
        Job::RetentionPurge,
        Job::ReminderSend,
        Job::ReminderRefill,
        Job::StockExpiry,
    ];

    /// The advisory lock key of this job.
    ///
    /// The keys are part of the deployed contract: two versions running
    /// side by side during a rolling deploy must agree on them.
    pub fn lock_key(self) -> i64 {
        let id = match self {
            Job::AttachmentReconcile => 1,
            Job::AccountPurge => 2,
            Job::RetentionPurge => 3,
            Job::ReminderSend => 4,
            Job::ReminderRefill => 5,
            Job::StockExpiry => 6,
        };
        LOCK_NAMESPACE | id
    }
}

/// Seconds of silence on the lock's connection before the server probes it.
pub const KEEPALIVE_IDLE_SECS: u64 = 30;
/// Seconds between two unanswered probes.
pub const KEEPALIVE_INTERVAL_SECS: u64 = 10;
/// Unanswered probes after which the server drops the session.
pub const KEEPALIVE_COUNT: u64 = 3;
/// Milliseconds the server waits for data it sent to be acknowledged
/// before it drops the session.
pub const USER_TIMEOUT_MS: u64 = 60_000;

/// The server-side session settings of the lock's connection.
///
/// A connection cut without a word — a network partition, a node lost
/// with its process — sends the server nothing: it learns of it from TCP
/// alone. With the server's defaults (`0`, the OS's: two hours of silence
/// before the first keepalive probe), the lock stayed held that long, and
/// no replica ran the job meanwhile. These are user-context settings, so
/// the lock's own session sets them, whatever the server's configuration.
pub fn session_settings() -> [(&'static str, String); 4] {
    [
        ("tcp_keepalives_idle", KEEPALIVE_IDLE_SECS.to_string()),
        (
            "tcp_keepalives_interval",
            KEEPALIVE_INTERVAL_SECS.to_string(),
        ),
        ("tcp_keepalives_count", KEEPALIVE_COUNT.to_string()),
        ("tcp_user_timeout", USER_TIMEOUT_MS.to_string()),
    ]
}

/// The longest a silently cut lock connection keeps its lock, per
/// [`session_settings`]: idle, the lock's session is dropped once its
/// probes went unanswered; in the middle of a send, once the data went
/// unacknowledged for `USER_TIMEOUT_MS` (which on Linux also caps the
/// probes).
pub fn silent_cut_release_bound() -> Duration {
    let probes =
        Duration::from_secs(KEEPALIVE_IDLE_SECS + KEEPALIVE_INTERVAL_SECS * KEEPALIVE_COUNT);
    probes.max(Duration::from_millis(USER_TIMEOUT_MS))
}

/// The connection options of the lock's connection: the pool's, plus
/// [`session_settings`].
pub fn lock_connect_options(pool: &PgPool) -> PgConnectOptions {
    (*pool.connect_options())
        .clone()
        .options(session_settings())
}

/// Runs `pass` if no other process is running `job`, and returns its
/// result; returns `Ok(None)` without running it otherwise.
///
/// An error opening the lock's connection or asking for the lock is
/// returned as is, and the pass is not run.
pub async fn exclusively<T, Fut>(pool: &PgPool, job: Job, pass: Fut) -> anyhow::Result<Option<T>>
where
    Fut: Future<Output = anyhow::Result<T>>,
{
    let mut lock = lock_connect_options(pool).connect().await?;
    let held: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(job.lock_key())
        .fetch_one(&mut lock)
        .await?;
    if !held {
        lock.close().await.ok();
        return Ok(None);
    }

    let outcome = pass.await;

    // Closing the session releases the lock. A connection lost meanwhile
    // already released it: the server dropped the session with it.
    if let Err(e) = lock.close().await {
        tracing::warn!(error = ?e, ?job, "closing the job lock's connection failed");
    }
    outcome.map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two jobs on one key would hold each other back.
    #[test]
    fn every_job_has_its_own_key() {
        let mut keys: Vec<i64> = Job::ALL.iter().map(|j| j.lock_key()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), Job::ALL.len());
    }

    /// A key changed between two versions lets the old replica and the new
    /// one run the same pass during a rolling deploy: the keys are pinned.
    #[test]
    fn the_keys_do_not_move_between_versions() {
        assert_eq!(
            Job::ALL.map(Job::lock_key),
            [
                0x6D6F_6D2D_6A6F_6201,
                0x6D6F_6D2D_6A6F_6202,
                0x6D6F_6D2D_6A6F_6203,
                0x6D6F_6D2D_6A6F_6204,
                0x6D6F_6D2D_6A6F_6205,
                0x6D6F_6D2D_6A6F_6206,
            ]
        );
    }

    /// A connection cut without a word (a network partition, a node gone)
    /// is noticed by the server only through its TCP settings; with the
    /// server's defaults (0: the OS's, two hours of silence before the
    /// first probe) the lock stayed held that long and no reminder went
    /// out. Bounded under two minutes: two missed reminder ticks at most.
    #[test]
    fn a_silently_cut_lock_is_released_within_two_minutes() {
        assert!(silent_cut_release_bound() <= Duration::from_secs(120));
        assert_eq!(silent_cut_release_bound(), Duration::from_secs(60));
    }

    /// Probing must not drop a live session over one lost packet.
    #[test]
    fn the_keepalive_probes_and_does_not_give_up_on_one_miss() {
        const {
            assert!(KEEPALIVE_IDLE_SECS > 0 && KEEPALIVE_INTERVAL_SECS > 0);
            assert!(KEEPALIVE_COUNT >= 3);
            assert!(USER_TIMEOUT_MS > 0);
        }
    }

    /// The names are Postgres's own GUCs, in its units (seconds, and
    /// milliseconds for `tcp_user_timeout`): a typo is refused at connect
    /// time, a unit slip would not be.
    #[test]
    fn the_session_settings_name_the_server_gucs_in_their_units() {
        assert_eq!(
            session_settings(),
            [
                ("tcp_keepalives_idle", "30".to_owned()),
                ("tcp_keepalives_interval", "10".to_owned()),
                ("tcp_keepalives_count", "3".to_owned()),
                ("tcp_user_timeout", "60000".to_owned()),
            ]
        );
    }

    /// Every key sits in the namespace, clear of small keys and of
    /// whatever else the database's lock space holds.
    #[test]
    fn every_key_is_in_the_namespace() {
        assert_eq!(LOCK_NAMESPACE.to_be_bytes(), *b"mom-job\0");
        for job in Job::ALL {
            assert_eq!(job.lock_key() & !0xFF, LOCK_NAMESPACE, "{job:?}");
            assert_ne!(job.lock_key(), LOCK_NAMESPACE, "{job:?}");
        }
    }
}
