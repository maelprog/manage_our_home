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
//! lock, so nothing has to unlock it by hand, and a session that dies — the
//! connection cut, the process killed, the pass's future dropped — releases
//! it too: another replica takes it on its next tick. Opening it outside
//! the pool also keeps it from holding one of the admin pool's five
//! connections while the pass itself needs them.

use std::future::Future;

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
}

/// The high bytes of every key, `mom-job` in ASCII: the advisory lock
/// space is shared with whatever else takes such locks on the database
/// (sqlx's migrator does), and a key of `1` is the first anyone picks.
pub const LOCK_NAMESPACE: i64 = i64::from_be_bytes(*b"mom-job\0");

impl Job {
    pub const ALL: [Job; 5] = [
        Job::AttachmentReconcile,
        Job::AccountPurge,
        Job::RetentionPurge,
        Job::ReminderSend,
        Job::ReminderRefill,
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
        };
        LOCK_NAMESPACE | id
    }
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
    let options = pool.connect_options();
    let mut lock = options.connect().await?;
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
