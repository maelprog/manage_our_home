//! Scheduled orphaned-attachment sweep (#215).
//!
//! `reconcile-attachments` (#58) finds and deletes objects no
//! `event_attachments` row points at, but only when someone runs it. The
//! leak it cleans up never stops (`upload_attachment` can still leave an
//! object behind when `tx.commit()` fails or the request dies after
//! `put_object`, see `crate::attachment_reconcile`), and an orphan outlives
//! every erasure path: deleting an account or a group removes the objects
//! rows point at, never one without a row. Without a schedule a user's
//! photo or PDF could stay in the bucket indefinitely.
//!
//! This runs the same pass — same RLS guard, same age window, same INFO
//! line per key before it goes — once a day, with deletion on.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio::time::{interval, MissedTickBehavior};

use crate::attachment_reconcile::{reconcile, Options, ReconcileOutcome, DEFAULT_MIN_AGE_HOURS};
use crate::storage::Storage;

/// Once a day. The leak is a handful of objects a year at household scale
/// (#62), so the cadence only bounds how long an orphan survives: at most
/// one age window plus one interval, under two days with the defaults.
pub const SWEEP_INTERVAL: StdDuration = StdDuration::from_secs(24 * 3600);

/// What each scheduled pass does: the manual pass's defaults, except that
/// it deletes. A dry run on a timer would only log what nobody removes.
pub fn scheduled_options() -> Options {
    Options {
        apply: true,
        min_age_hours: DEFAULT_MIN_AGE_HOURS,
        prefix: None,
    }
}

/// Polling worker, `account_purge`'s shape. `pool` must be the `BYPASSRLS`
/// admin pool (`AppState.admin_db`): on any other role `reconcile` refuses
/// before listing the bucket, and every tick logs that refusal at ERROR
/// instead of deleting anything.
///
/// The first tick fires at startup rather than one interval in, so a
/// process restarted more often than daily still sweeps.
pub async fn run(pool: PgPool, storage: Storage) {
    let mut ticker = interval(SWEEP_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if let Err(e) = sweep(&pool, &storage, &scheduled_options(), Utc::now()).await {
            tracing::error!(error = ?e, "attachment reconcile job failed");
        }
    }
}

/// One pass. `opts` and `now` are parameters so the flow tests can scope
/// the pass to their own prefix (the test bucket is shared) and move the
/// clock past the window instead of waiting a day.
pub async fn sweep(
    pool: &PgPool,
    storage: &Storage,
    opts: &Options,
    now: DateTime<Utc>,
) -> anyhow::Result<ReconcileOutcome> {
    let mut conn = pool.acquire().await?;
    let outcome = reconcile(storage, &mut conn, opts, now).await?;

    let scan = &outcome.scan;
    if !scan.unknown_age.is_empty() {
        tracing::warn!(
            keys = ?scan.unknown_age,
            "attachment objects with no row and no LastModified kept: their age is unknown"
        );
    }
    tracing::info!(
        matched = scan.matched,
        in_flight = scan.in_flight,
        unknown_age = scan.unknown_age.len(),
        orphaned = scan.orphans.len(),
        orphaned_bytes = scan.orphan_bytes(),
        deleted = outcome.deleted.len(),
        "attachment reconcile pass done"
    );

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The binary defaults to a dry run; a scheduled dry run would report
    /// orphans into a log nobody reads and delete none of them (#215).
    #[test]
    fn the_scheduled_pass_deletes() {
        assert!(scheduled_options().apply);
    }

    /// Same in-flight guard as the manual pass: an object with no row
    /// younger than the window may be an upload still running.
    #[test]
    fn the_scheduled_pass_keeps_the_default_age_window() {
        assert_eq!(scheduled_options().min_age_hours, DEFAULT_MIN_AGE_HOURS);
        assert!(scheduled_options().min_age_hours > 0);
    }

    /// The job owns the whole bucket: a prefix would leave every other
    /// family's orphans on disk for good.
    #[test]
    fn the_scheduled_pass_walks_the_whole_bucket() {
        assert_eq!(scheduled_options().prefix, None);
    }

    /// An orphan lives at most one window plus one interval. Daily keeps
    /// that under two days with the 24h default.
    #[test]
    fn the_sweep_runs_at_least_daily() {
        assert!(SWEEP_INTERVAL <= StdDuration::from_secs(24 * 3600));
        assert!(SWEEP_INTERVAL > StdDuration::ZERO);
    }
}
