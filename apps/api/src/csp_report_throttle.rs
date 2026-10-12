//! Per-address limit on `POST /csp-report` (#375).
//!
//! The route answers anyone — no session, no origin guard (see `lib.rs`) —
//! and every report it reads becomes up to
//! [`crate::csp_report::MAX_VIOLATIONS_PER_REQUEST`] log lines. The body
//! limit bounds one request; this bounds how many requests one client gets
//! to send: at most [`MAX_REPORTS`] per [`WINDOW`], after which it is
//! answered 429 until the window ends, **before its body is read**.
//!
//! It counts the same client as the login lock (`auth::throttle`), by the
//! same rule and the same mechanism:
//!
//! - the address is the one `client_ip::ClientIp` resolves behind the
//!   trusted proxies, reduced by `auth::throttle::address_scope` — an IPv6
//!   client is its /64, not the /128 it can remake at will;
//! - the counters live in Postgres (#428), in `csp_report_throttle`
//!   (`migrations/0029_rate_limits.sql`), shared by every replica — until
//!   #428 each process counted alone, and behind N replicas a client had N
//!   times its budget. A row holds the address's keyed pseudonym
//!   (`auth::throttle::Pseudonyms::limit`), never the address;
//! - a request is counted when it is admitted, in one transaction under a
//!   Postgres advisory lock on the address ([`SCOPE_LOCK`]), so concurrent
//!   requests, on any replica, cannot all read "under the limit". An
//!   address the table does not hold yet also takes [`TABLE_LOCK`], since
//!   its row counts against [`MAX_TRACKED`]. The clock is the database's;
//! - what decides is pure ([`Entry::admit`], [`refuse_newcomer`]) and
//!   unchanged since #375.
//!
//! **A full table refuses newcomers rather than evicting anyone.** Evicting
//! would reset the evicted address's count, which is the very thing an
//! attacker spread over many addresses would want. Stale addresses are
//! reclaimed first.
//!
//! The price: an attacker can keep the table full for as long as it keeps
//! sending — one request per address per window renews every entry, about
//! 11 requests a second across [`MAX_TRACKED`] addresses, and a single
//! IPv6 /48 holds 65 536 /64s. For as long as that lasts, every address not
//! already tracked is refused, and its reports are lost. What it cannot
//! do is raise the log lines or the table past their bounds.
//!
//! Since #428 every request also costs a transaction and a pool
//! connection, refused or not: the route answers anyone, so a flood of
//! reports competes for the pool with the other routes, as the login's
//! attempts do (#454). A database error answers 500, still before the body
//! is read.
//!
//! The address is kept as a pseudonym for one window, and at most until
//! the hourly retention purge that follows ([`purge_expired`]). It is never
//! logged: the report handler itself still reads the body and nothing else.

use std::net::IpAddr;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};

use crate::auth::throttle::{address_scope, Pseudonyms};

/// Requests admitted per address within one [`WINDOW`]. A page view
/// raises one report per violation, sent through one mechanism only: the
/// policy names both `report-to` and `report-uri`, but a browser that
/// supports `report-to` ignores `report-uri` (CSP3 §5.5), and the Reporting
/// API may batch several reports into one request. A household shares one
/// address: 100 leaves room for a browser extension that trips the policy
/// on every page, while bounding one address at
/// `100 × MAX_VIOLATIONS_PER_REQUEST` = 1 000 log lines per window.
pub const MAX_REPORTS: u32 = 100;

/// How long requests accumulate, and so how long an address that reached
/// [`MAX_REPORTS`] stays refused at most: the login lock's quarter hour.
pub const WINDOW: Duration = Duration::from_secs(15 * 60);

/// Ceiling on tracked addresses. The same figure as the login lock's
/// `MAX_TRACKED`, which counts (address, email) pairs, not addresses.
pub const MAX_TRACKED: usize = 10_000;

/// What [`ReportThrottle::admit`] says about a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Admitted, and already counted.
    Allow,
    /// Refused before the body is read. `retry_after` is when that stops
    /// being true at the earliest.
    Refused { retry_after: Duration },
}

/// One address's window, as stored. Wall-clock instants since #428: they
/// are written to the database and read back by another process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub requests: u32,
    pub window_start: SystemTime,
}

/// How long from `earlier` to `later`, or zero if `later` is not later.
fn since(later: SystemTime, earlier: SystemTime) -> Duration {
    later.duration_since(earlier).unwrap_or(Duration::ZERO)
}

impl Entry {
    /// The entry of an address admitted for the first time at `now`: its
    /// first request, already counted.
    pub fn first(now: SystemTime) -> Self {
        Entry {
            requests: 1,
            window_start: now,
        }
    }

    fn ends_at(&self) -> SystemTime {
        self.window_start + WINDOW
    }

    /// Its window is over: nothing left to remember.
    pub fn is_stale(&self, now: SystemTime) -> bool {
        now >= self.ends_at()
    }

    /// A request from an address the table holds: a window that is over
    /// starts again at `now`; at [`MAX_REPORTS`], refused until the window
    /// ends, and not counted; otherwise counted.
    pub fn admit(&mut self, now: SystemTime) -> Decision {
        if self.is_stale(now) {
            *self = Entry {
                requests: 0,
                window_start: now,
            };
        }
        if self.requests >= MAX_REPORTS {
            return Decision::Refused {
                retry_after: since(self.ends_at(), now),
            };
        }
        self.requests += 1;
        Decision::Allow
    }
}

/// A request from an address the table does not hold. `held` is how many
/// addresses the table holds once its stale ones are reclaimed, `earliest`
/// the oldest window start among them. Refused while the table is full,
/// until that first window ends; `None` when there is room.
pub fn refuse_newcomer(
    held: usize,
    earliest: Option<SystemTime>,
    now: SystemTime,
) -> Option<Decision> {
    (held >= MAX_TRACKED).then(|| Decision::Refused {
        retry_after: earliest.map_or(Duration::ZERO, |start| since(start + WINDOW, now)),
    })
}

/// Advisory lock class of one address, as the login lock's `SCOPE_LOCK`;
/// the second key is the first four bytes of the address's pseudonym.
pub const SCOPE_LOCK: i32 = i32::from_be_bytes(*b"mcss");

/// Advisory lock class of the table as a whole, taken (with second key 0)
/// by the admission of an address the table does not hold. Always taken
/// after [`SCOPE_LOCK`], never before, so the two cannot deadlock.
pub const TABLE_LOCK: i32 = i32::from_be_bytes(*b"mcst");

/// The label of the addresses' pseudonyms.
const LABEL: &str = "csp_report";

/// The counter, in Postgres. Held in `AppState` behind an `Arc`; each
/// replica builds its own, on the same table, with the same secret.
#[derive(Debug, Clone)]
pub struct ReportThrottle {
    db: PgPool,
    pseudonyms: Pseudonyms,
}

async fn database_now(tx: &mut Transaction<'static, Postgres>) -> Result<SystemTime, sqlx::Error> {
    Ok(sqlx::query_scalar!(r#"SELECT clock_timestamp() AS "now!""#)
        .fetch_one(&mut **tx)
        .await?
        .into())
}

impl ReportThrottle {
    pub fn new(db: PgPool, pseudonyms: Pseudonyms) -> Self {
        Self { db, pseudonyms }
    }

    /// Checks the address's budget **and** spends one request of it, in
    /// one transaction. An error leaves nothing counted; the caller does
    /// not read the body.
    pub async fn admit(&self, ip: IpAddr) -> Result<Decision, sqlx::Error> {
        let scope = self
            .pseudonyms
            .limit(LABEL, address_scope(ip).to_string().as_bytes());
        let mut tx = crate::db::begin(&self.db).await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
            .bind(SCOPE_LOCK)
            .bind(i32::from_be_bytes([scope[0], scope[1], scope[2], scope[3]]))
            .execute(&mut *tx)
            .await?;
        let now = database_now(&mut tx).await?;

        let held = sqlx::query!(
            "SELECT requests, window_start FROM csp_report_throttle WHERE scope = $1",
            &scope[..]
        )
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(row) = held {
            // A held address: decided on its own row, updated in place. If
            // the row went meanwhile — reclaimed by a newcomer's admission
            // on a full table — it is a newcomer again.
            let loaded = Entry {
                requests: u32::try_from(row.requests).unwrap_or(0),
                window_start: row.window_start.into(),
            };
            let mut entry = loaded;
            let decision = entry.admit(now);
            if entry == loaded {
                tx.commit().await?;
                return Ok(decision);
            }
            let updated = sqlx::query!(
                "UPDATE csp_report_throttle SET requests = $2, window_start = $3
                  WHERE scope = $1",
                &scope[..],
                i32::try_from(entry.requests).unwrap_or(i32::MAX),
                DateTime::<Utc>::from(entry.window_start)
            )
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if updated == 1 {
                tx.commit().await?;
                return Ok(decision);
            }
        }

        // A newcomer counts against the ceiling: the table's lock, and a
        // clock read once it is held.
        sqlx::query("SELECT pg_advisory_xact_lock($1, 0)")
            .bind(TABLE_LOCK)
            .execute(&mut *tx)
            .await?;
        let now = database_now(&mut tx).await?;
        let total = sqlx::query_scalar!(r#"SELECT count(*) AS "total!" FROM csp_report_throttle"#)
            .fetch_one(&mut *tx)
            .await?;
        if usize::try_from(total).unwrap_or(usize::MAX) >= MAX_TRACKED {
            purge_expired(&mut tx, now).await?;
            let live = sqlx::query!(
                r#"SELECT count(*) AS "held!", min(window_start) AS earliest
                     FROM csp_report_throttle"#
            )
            .fetch_one(&mut *tx)
            .await?;
            if let Some(refused) = refuse_newcomer(
                usize::try_from(live.held).unwrap_or(usize::MAX),
                live.earliest.map(Into::into),
                now,
            ) {
                tx.commit().await?;
                return Ok(refused);
            }
        }
        let first = Entry::first(now);
        sqlx::query!(
            "INSERT INTO csp_report_throttle (scope, requests, window_start) VALUES ($1, $2, $3)",
            &scope[..],
            i32::try_from(first.requests).unwrap_or(i32::MAX),
            DateTime::<Utc>::from(first.window_start)
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Decision::Allow)
    }
}

/// Deletes every address whose window is over at `now`, and says how many.
/// Run by a newcomer's admission on a full table, and by the hourly
/// retention purge (`jobs::retention_purge`), so a pseudonym does not
/// outlive its window by more than the hour that follows.
pub async fn purge_expired(
    conn: &mut sqlx::PgConnection,
    now: SystemTime,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query!(
        "DELETE FROM csp_report_throttle WHERE window_start <= $1",
        DateTime::<Utc>::from(now - WINDOW)
    )
    .execute(conn)
    .await?
    .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::Ipv4Addr;

    /// The table as the process used to hold it before #428, composed of
    /// the same pure decisions the store makes on its rows.
    #[derive(Default)]
    struct ReportThrottle {
        table: HashMap<IpAddr, Entry>,
    }

    impl ReportThrottle {
        fn new() -> Self {
            Self::default()
        }

        fn admit(&mut self, ip: IpAddr, now: SystemTime) -> Decision {
            let ip = address_scope(ip);
            if let Some(entry) = self.table.get_mut(&ip) {
                return entry.admit(now);
            }
            if self.table.len() >= MAX_TRACKED {
                self.table.retain(|_, e| !e.is_stale(now));
                let earliest = self.table.values().map(|e| e.window_start).min();
                if let Some(refused) = refuse_newcomer(self.table.len(), earliest, now) {
                    return refused;
                }
            }
            self.table.insert(ip, Entry::first(now));
            Decision::Allow
        }

        fn tracked(&self) -> usize {
            self.table.len()
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn admit_n(t: &mut ReportThrottle, ip: IpAddr, n: u32, now: SystemTime) {
        for i in 0..n {
            assert_eq!(t.admit(ip, now), Decision::Allow, "request {i}");
        }
    }

    #[test]
    fn requests_up_to_the_limit_are_admitted_and_the_next_is_refused() {
        let mut t = ReportThrottle::new();
        let now = SystemTime::now();
        admit_n(&mut t, ip("203.0.113.7"), MAX_REPORTS, now);
        assert_eq!(
            t.admit(ip("203.0.113.7"), now),
            Decision::Refused {
                retry_after: WINDOW
            }
        );
    }

    #[test]
    fn retry_after_counts_down_to_the_end_of_the_window() {
        let mut t = ReportThrottle::new();
        let t0 = SystemTime::now();
        admit_n(&mut t, ip("203.0.113.7"), MAX_REPORTS, t0);
        let later = t0 + Duration::from_secs(60);
        assert_eq!(
            t.admit(ip("203.0.113.7"), later),
            Decision::Refused {
                retry_after: WINDOW - Duration::from_secs(60)
            }
        );
    }

    /// A refused request is not counted: it cannot push the end of the
    /// window further away.
    #[test]
    fn the_budget_comes_back_once_the_window_is_over() {
        let mut t = ReportThrottle::new();
        let t0 = SystemTime::now();
        admit_n(&mut t, ip("203.0.113.7"), MAX_REPORTS, t0);
        for s in [1, 300, 899] {
            assert!(matches!(
                t.admit(ip("203.0.113.7"), t0 + Duration::from_secs(s)),
                Decision::Refused { .. }
            ));
        }
        admit_n(&mut t, ip("203.0.113.7"), MAX_REPORTS, t0 + WINDOW);
    }

    #[test]
    fn one_address_spending_its_budget_costs_no_other_address() {
        let mut t = ReportThrottle::new();
        let now = SystemTime::now();
        admit_n(&mut t, ip("203.0.113.7"), MAX_REPORTS, now);
        admit_n(&mut t, ip("203.0.113.8"), MAX_REPORTS, now);
        admit_n(&mut t, ip("2001:db8:1::1"), MAX_REPORTS, now);
    }

    /// The login lock's client: an IPv6 household is its /64, whichever
    /// source address it picks inside it.
    #[test]
    fn an_ipv6_client_spends_one_budget_across_its_slash_64() {
        let mut t = ReportThrottle::new();
        let now = SystemTime::now();
        for i in 0..MAX_REPORTS {
            let source = IpAddr::V6(std::net::Ipv6Addr::new(
                0x2001, 0xdb8, 0, 1, 0, 0, 0, i as u16,
            ));
            assert_eq!(t.admit(source, now), Decision::Allow);
        }
        assert!(matches!(
            t.admit(ip("2001:db8:0:1:ffff::1"), now),
            Decision::Refused { .. }
        ));
        assert_eq!(t.admit(ip("2001:db8:0:2::1"), now), Decision::Allow);
        assert_eq!(t.tracked(), 2);
    }

    fn bulk_ip(i: usize) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(0x0A00_0000 + i as u32))
    }

    #[test]
    fn a_full_table_refuses_a_new_address_and_evicts_nobody() {
        let mut t = ReportThrottle::new();
        let t0 = SystemTime::now();
        for i in 0..MAX_TRACKED {
            t.admit(bulk_ip(i), t0);
        }
        assert_eq!(t.tracked(), MAX_TRACKED);
        let later = t0 + Duration::from_secs(60);
        assert_eq!(
            t.admit(ip("198.51.100.1"), later),
            Decision::Refused {
                retry_after: WINDOW - Duration::from_secs(60)
            }
        );
        assert_eq!(t.tracked(), MAX_TRACKED);
        // Addresses already held keep their count, and their budget.
        admit_n(&mut t, bulk_ip(0), MAX_REPORTS - 1, later);
        assert!(matches!(
            t.admit(bulk_ip(0), later),
            Decision::Refused { .. }
        ));
    }

    #[test]
    fn a_full_table_of_stale_addresses_makes_room() {
        let mut t = ReportThrottle::new();
        let t0 = SystemTime::now();
        for i in 0..MAX_TRACKED {
            t.admit(bulk_ip(i), t0);
        }
        assert_eq!(t.admit(ip("198.51.100.1"), t0 + WINDOW), Decision::Allow);
        assert_eq!(t.tracked(), 1);
    }

    /// #428: the two pure halves the store calls, on their own.
    #[test]
    fn a_held_entry_is_counted_until_the_limit_and_restarts_once_stale() {
        let t0 = SystemTime::now();
        let mut entry = Entry::first(t0);
        assert_eq!(entry.admit(t0), Decision::Allow);
        assert_eq!(entry.requests, 2);
        entry.requests = MAX_REPORTS;
        let full = entry;
        assert_eq!(
            entry.admit(t0 + Duration::from_secs(1)),
            Decision::Refused {
                retry_after: WINDOW - Duration::from_secs(1)
            }
        );
        assert_eq!(entry, full, "a refusal changes nothing to store");
        assert_eq!(entry.admit(t0 + WINDOW), Decision::Allow);
        assert_eq!(entry, Entry::first(t0 + WINDOW));
    }

    #[test]
    fn a_newcomer_is_refused_only_on_a_full_table_until_its_first_window_ends() {
        let t0 = SystemTime::now();
        let now = t0 + Duration::from_secs(60);
        assert_eq!(refuse_newcomer(0, None, now), None);
        assert_eq!(refuse_newcomer(MAX_TRACKED - 1, Some(t0), now), None);
        assert_eq!(
            refuse_newcomer(MAX_TRACKED, Some(t0), now),
            Some(Decision::Refused {
                retry_after: WINDOW - Duration::from_secs(60)
            })
        );
    }
}
