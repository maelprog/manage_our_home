//! Per-address limit on `POST /csp-report` (#375).
//!
//! The route answers anyone — no session, no origin guard (see `lib.rs`) —
//! and every report it reads becomes up to
//! [`crate::csp_report::MAX_VIOLATIONS_PER_REQUEST`] log lines. The body
//! limit bounds one request; this bounds how many requests one client gets
//! to send: at most [`MAX_REPORTS`] per [`WINDOW`], after which it is
//! answered 429 until the window ends, **before its body is read**.
//!
//! Built like the login lock (`auth::throttle`), so that both count the
//! same client:
//!
//! - the address is the one `client_ip::ClientIp` resolves behind the
//!   trusted proxies, reduced by `auth::throttle::address_scope` — an IPv6
//!   client is its /64, not the /128 it can remake at will;
//! - a request is counted when it is admitted, under one mutex acquisition,
//!   so concurrent requests cannot all read "under the limit";
//! - the counter lives in this process's memory, for the reason and with
//!   the caveat `auth::throttle` gives (one `api` instance), and holds at
//!   most [`MAX_TRACKED`] addresses.
//!
//! **A full table refuses newcomers rather than evicting anyone.** Evicting
//! would reset the evicted address's count, which is the very thing an
//! attacker spread over many addresses would want; refusing only costs
//! reports, which a browser never resends and which the next page view
//! raises again. Stale addresses are reclaimed first.
//!
//! The address is kept for one window at most and never logged: the
//! report handler itself still reads the body and nothing else.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::auth::throttle::address_scope;

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

/// Ceiling on tracked addresses, the login lock's.
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

#[derive(Debug, Clone, Copy)]
struct Entry {
    count: u32,
    window_start: Instant,
}

impl Entry {
    fn ends_at(&self) -> Instant {
        self.window_start + WINDOW
    }

    fn is_stale(&self, now: Instant) -> bool {
        now >= self.ends_at()
    }
}

/// The counter. Held in `AppState` behind an `Arc`.
#[derive(Debug, Default)]
pub struct ReportThrottle {
    table: Mutex<HashMap<IpAddr, Entry>>,
}

impl ReportThrottle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks the address's budget **and** spends one request of it,
    /// atomically.
    pub fn admit(&self, ip: IpAddr, now: Instant) -> Decision {
        let ip = address_scope(ip);
        let mut table = self.table.lock().unwrap_or_else(|e| e.into_inner());

        if !table.contains_key(&ip) && table.len() >= MAX_TRACKED {
            table.retain(|_, e| !e.is_stale(now));
            if table.len() >= MAX_TRACKED {
                let frees_at = table.values().map(Entry::ends_at).min();
                return Decision::Refused {
                    retry_after: frees_at
                        .map_or(Duration::ZERO, |at| at.saturating_duration_since(now)),
                };
            }
        }
        let entry = table.entry(ip).or_insert(Entry {
            count: 0,
            window_start: now,
        });
        if entry.is_stale(now) {
            *entry = Entry {
                count: 0,
                window_start: now,
            };
        }
        if entry.count >= MAX_REPORTS {
            return Decision::Refused {
                retry_after: entry.ends_at().saturating_duration_since(now),
            };
        }
        entry.count += 1;
        Decision::Allow
    }

    /// Addresses currently held. For the memory-ceiling tests.
    pub fn tracked(&self) -> usize {
        self.table.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn admit_n(t: &ReportThrottle, ip: IpAddr, n: u32, now: Instant) {
        for i in 0..n {
            assert_eq!(t.admit(ip, now), Decision::Allow, "request {i}");
        }
    }

    #[test]
    fn requests_up_to_the_limit_are_admitted_and_the_next_is_refused() {
        let t = ReportThrottle::new();
        let now = Instant::now();
        admit_n(&t, ip("203.0.113.7"), MAX_REPORTS, now);
        assert_eq!(
            t.admit(ip("203.0.113.7"), now),
            Decision::Refused {
                retry_after: WINDOW
            }
        );
    }

    #[test]
    fn retry_after_counts_down_to_the_end_of_the_window() {
        let t = ReportThrottle::new();
        let t0 = Instant::now();
        admit_n(&t, ip("203.0.113.7"), MAX_REPORTS, t0);
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
        let t = ReportThrottle::new();
        let t0 = Instant::now();
        admit_n(&t, ip("203.0.113.7"), MAX_REPORTS, t0);
        for s in [1, 300, 899] {
            assert!(matches!(
                t.admit(ip("203.0.113.7"), t0 + Duration::from_secs(s)),
                Decision::Refused { .. }
            ));
        }
        admit_n(&t, ip("203.0.113.7"), MAX_REPORTS, t0 + WINDOW);
    }

    #[test]
    fn one_address_spending_its_budget_costs_no_other_address() {
        let t = ReportThrottle::new();
        let now = Instant::now();
        admit_n(&t, ip("203.0.113.7"), MAX_REPORTS, now);
        admit_n(&t, ip("203.0.113.8"), MAX_REPORTS, now);
        admit_n(&t, ip("2001:db8:1::1"), MAX_REPORTS, now);
    }

    /// The login lock's client: an IPv6 household is its /64, whichever
    /// source address it picks inside it.
    #[test]
    fn an_ipv6_client_spends_one_budget_across_its_slash_64() {
        let t = ReportThrottle::new();
        let now = Instant::now();
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
        let t = ReportThrottle::new();
        let t0 = Instant::now();
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
        admit_n(&t, bulk_ip(0), MAX_REPORTS - 1, later);
        assert!(matches!(
            t.admit(bulk_ip(0), later),
            Decision::Refused { .. }
        ));
    }

    #[test]
    fn a_full_table_of_stale_addresses_makes_room() {
        let t = ReportThrottle::new();
        let t0 = Instant::now();
        for i in 0..MAX_TRACKED {
            t.admit(bulk_ip(i), t0);
        }
        assert_eq!(t.admit(ip("198.51.100.1"), t0 + WINDOW), Decision::Allow);
        assert_eq!(t.tracked(), 1);
    }
}
