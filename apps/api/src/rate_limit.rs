//! Sliding-window rate limits shared by every replica (#428): the recipe
//! import per member (`recipes::import`) and the reads sent to Open Food
//! Facts (`stocks::openfoodfacts`).
//!
//! Until #428 each was a `Mutex` in the process's memory: with N replicas
//! behind the ingress, a member had N times their imports and Open Food
//! Facts got N times the reads its terms allow. They now count in the
//! `rate_limit_log` table (`migrations/0029_rate_limits.sql`), by the
//! mechanism #427 gave the login lock (`auth::throttle`):
//!
//! - one admission is one transaction, under a transaction-level advisory
//!   lock on the counted subject ([`LOG_LOCK`]) — what the mutex did, so
//!   concurrent requests on any replica are decided one after the other
//!   and cannot all read "under the limit";
//! - the clock is the database's (`clock_timestamp()`), so replicas whose
//!   clocks drift agree on when an admission leaves the window;
//! - the subject is stored as a keyed pseudonym
//!   (`auth::throttle::Pseudonyms::limit`), never in clear;
//! - what decides is pure ([`SlidingWindow`]), the store only loads the
//!   subject's admissions and writes back what it decided.
//!
//! **The decision is unchanged**: at most `limit` admissions per `window`,
//! sliding; a refused request is not counted; a subject whose admissions
//! all left the window is forgotten — by the next admission of the same
//! limit, as the in-memory map did, and by the hourly retention purge
//! ([`purge_expired`]) when none comes.
//!
//! The cost the in-memory version was spared: a database round trip and a
//! pool connection per admission. Both limits guard a request that costs
//! far more — an outbound HTTP fetch. That connection is held while the
//! admission waits on its subject's lock, so it comes from a pool the
//! limits share, apart from the handlers' ([`POOL_SIZE`], #483).

use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::auth::throttle::Pseudonyms;

/// How long from `earlier` to `later`, or zero if `later` is not later.
fn since(later: SystemTime, earlier: SystemTime) -> Duration {
    later.duration_since(earlier).unwrap_or(Duration::ZERO)
}

/// At most `limit` admissions per `window`, sliding. The whole decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlidingWindow {
    pub limit: usize,
    pub window: Duration,
}

impl SlidingWindow {
    /// Drops from `log` the admissions that left the window at `now`, then
    /// admits `now` and appends it if fewer than `limit` remain. A refused
    /// request is not appended: it does not count.
    pub fn admit(&self, log: &mut Vec<SystemTime>, now: SystemTime) -> bool {
        log.retain(|&at| since(now, at) < self.window);
        if log.len() >= self.limit {
            return false;
        }
        log.push(now);
        true
    }

    /// Whether every admission of `log` left the window at `now`: nothing
    /// left to remember of its subject.
    pub fn forgets(&self, log: &[SystemTime], now: SystemTime) -> bool {
        log.iter().all(|&at| since(now, at) >= self.window)
    }
}

/// Advisory lock class of one subject's admissions. The two-key form of
/// `pg_advisory_xact_lock`, apart from the single 64-bit keys of the
/// background jobs (#426); the second key is the first four bytes of the
/// subject's pseudonym, and two subjects that collide only wait on each
/// other. The login lock's classes are `mlgs` and `mlgt`.
pub const LOG_LOCK: i32 = i32::from_be_bytes(*b"mrll");

/// The longest window of the limits kept in `rate_limit_log`: the hourly
/// purge deletes a row once its newest admission is that old.
pub const LONGEST_WINDOW: Duration = Duration::from_secs(10 * 60);

/// Connections of the pool the limits of this module share, per replica,
/// when [`POOL_SIZE_VAR`] is unset (#483). `main.rs` opens it beside the
/// handlers' pool (`db::pool_beside`) and gives it to every
/// [`SlidingLimiter`]: an admission holds its connection while it waits on
/// its subject's lock, and must not hold one of the handlers'.
pub const POOL_SIZE: u32 = 2;

/// Environment variable sizing that pool, per replica (#483): every
/// replica opens that many connections on top of its others, so the size
/// is set with the deployment, against Postgres's `max_connections`.
pub const POOL_SIZE_VAR: &str = "RATE_LIMIT_POOL_SIZE";

/// One limit, counted in `rate_limit_log`. Held in `AppState` behind an
/// `Arc`; each replica builds its own, on the same table, with the same
/// secret.
#[derive(Debug, Clone)]
pub struct SlidingLimiter {
    db: PgPool,
    pseudonyms: Pseudonyms,
    /// `rate_limit_log.kind`, and the label of the subjects' pseudonyms.
    kind: &'static str,
    rule: SlidingWindow,
}

impl SlidingLimiter {
    /// `kind` is one the table's CHECK accepts. `db` is the limits' own
    /// pool ([`POOL_SIZE`]), not the handlers'.
    pub fn new(
        db: PgPool,
        pseudonyms: Pseudonyms,
        kind: &'static str,
        rule: SlidingWindow,
    ) -> Self {
        debug_assert!(rule.window <= LONGEST_WINDOW);
        Self {
            db,
            pseudonyms,
            kind,
            rule,
        }
    }

    /// Checks `subject`'s budget **and** spends one admission of it, in
    /// one transaction. An error leaves nothing counted; the caller does
    /// not let the request through.
    pub async fn admit(&self, subject: &[u8]) -> Result<bool, sqlx::Error> {
        let bucket = self.pseudonyms.limit(self.kind, subject);
        let mut tx = crate::db::begin(&self.db).await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
            .bind(LOG_LOCK)
            .bind(i32::from_be_bytes([
                bucket[0], bucket[1], bucket[2], bucket[3],
            ]))
            .execute(&mut *tx)
            .await?;
        let now: SystemTime = sqlx::query_scalar!(r#"SELECT clock_timestamp() AS "now!""#)
            .fetch_one(&mut *tx)
            .await?
            .into();

        let mut log: Vec<SystemTime> = sqlx::query_scalar!(
            "SELECT admitted FROM rate_limit_log WHERE kind = $1 AND bucket = $2",
            self.kind,
            &bucket[..]
        )
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or_default()
        .into_iter()
        .map(SystemTime::from)
        .collect();
        let before = log.clone();
        let admitted = self.rule.admit(&mut log, now);

        if log.is_empty() {
            // Refused with an empty log: only a zero limit gets here.
            sqlx::query!(
                "DELETE FROM rate_limit_log WHERE kind = $1 AND bucket = $2",
                self.kind,
                &bucket[..]
            )
            .execute(&mut *tx)
            .await?;
        } else if log != before {
            let stored: Vec<DateTime<Utc>> = log.iter().copied().map(DateTime::from).collect();
            sqlx::query!(
                "INSERT INTO rate_limit_log (kind, bucket, admitted) VALUES ($1, $2, $3)
                 ON CONFLICT (kind, bucket) DO UPDATE SET admitted = EXCLUDED.admitted",
                self.kind,
                &bucket[..],
                &stored
            )
            .execute(&mut *tx)
            .await?;
        }

        // Every other subject of this limit whose admissions all left the
        // window is forgotten, as the in-memory map dropped them. A row
        // another admission holds is skipped rather than waited on: two
        // admissions deleting each other's rows would otherwise deadlock.
        let horizon = DateTime::<Utc>::from(now - self.rule.window);
        sqlx::query!(
            "DELETE FROM rate_limit_log
              WHERE (kind, bucket) IN (
                    SELECT kind, bucket FROM rate_limit_log
                     WHERE kind = $1
                       AND (SELECT max(a) FROM unnest(admitted) AS a) <= $2
                       FOR UPDATE SKIP LOCKED)",
            self.kind,
            horizon
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(admitted)
    }
}

/// Deletes every row whose newest admission is [`LONGEST_WINDOW`] old at
/// `now`, and says how many. Run by the hourly retention purge
/// (`jobs::retention_purge`), so a subject that never comes back is not
/// kept past the hour that follows its last window.
pub async fn purge_expired(
    conn: &mut sqlx::PgConnection,
    now: SystemTime,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query!(
        "DELETE FROM rate_limit_log
          WHERE (SELECT max(a) FROM unnest(admitted) AS a) <= $1",
        DateTime::<Utc>::from(now - LONGEST_WINDOW)
    )
    .execute(conn)
    .await?
    .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: SlidingWindow = SlidingWindow {
        limit: 3,
        window: Duration::from_secs(60),
    };

    fn at(t0: SystemTime, secs: u64) -> SystemTime {
        t0 + Duration::from_secs(secs)
    }

    #[test]
    fn up_to_the_limit_is_admitted_and_counted_then_refused() {
        let t0 = SystemTime::now();
        let mut log = Vec::new();
        for i in 0..3 {
            assert!(RULE.admit(&mut log, at(t0, i)), "{i}");
        }
        assert_eq!(log, vec![at(t0, 0), at(t0, 1), at(t0, 2)]);
        assert!(!RULE.admit(&mut log, at(t0, 3)));
        assert_eq!(log.len(), 3, "a refused request is not counted");
    }

    #[test]
    fn an_admission_leaves_the_window_exactly_one_window_later() {
        let t0 = SystemTime::now();
        let mut log = vec![at(t0, 0), at(t0, 1), at(t0, 2)];
        assert!(!RULE.admit(&mut log, at(t0, 59)));
        assert!(RULE.admit(&mut log, at(t0, 60)));
        assert_eq!(log, vec![at(t0, 1), at(t0, 2), at(t0, 60)]);
        assert!(!RULE.admit(&mut log, at(t0, 60)));
    }

    #[test]
    fn a_refusal_still_drops_what_left_the_window() {
        let t0 = SystemTime::now();
        let mut log = vec![at(t0, 0), at(t0, 30), at(t0, 30), at(t0, 30)];
        assert!(!RULE.admit(&mut log, at(t0, 61)));
        assert_eq!(log, vec![at(t0, 30), at(t0, 30), at(t0, 30)]);
    }

    /// Admissions come from the database's clock, read by whichever
    /// replica: the log is not assumed sorted.
    #[test]
    fn the_log_need_not_be_sorted() {
        let t0 = SystemTime::now();
        let mut log = vec![at(t0, 50), at(t0, 0), at(t0, 40)];
        assert!(RULE.admit(&mut log, at(t0, 70)));
        assert_eq!(log, vec![at(t0, 50), at(t0, 40), at(t0, 70)]);
    }

    #[test]
    fn a_subject_is_forgotten_once_its_newest_admission_left_the_window() {
        let t0 = SystemTime::now();
        let log = vec![at(t0, 0), at(t0, 10)];
        assert!(!RULE.forgets(&log, at(t0, 69)));
        assert!(RULE.forgets(&log, at(t0, 70)));
        assert!(RULE.forgets(&[], t0));
    }
}
