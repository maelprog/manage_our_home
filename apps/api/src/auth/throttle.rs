//! Per-(address, email) throttle on failed logins (#178, piste 3).
//!
//! The decoy hash that closes the enumeration oracle makes *every* invalid
//! login pay a full argon2id — around 256 ms of CPU on the measurement in
//! #178, where an unknown email used to cost 0,22 ms. That is the remedy
//! and the new exposure at once: the cheapest possible request now buys
//! the most expensive possible work. So the throttle is consulted
//! **before** the hashing, never after: a lock that fires after the CPU
//! has been spent protects nothing.
//!
//! **Key: (address, email), never the email alone.** A per-account lock is
//! a denial of service anyone can aim at any address they know, which on a
//! household application means locking a family member out of their own
//! home. The pair bounds mass exploitation — the thing the oracle enables
//! — without handing out that weapon.
//!
//! **The counter lives in this process's memory.** `infra/docker-compose.yml`
//! declares a single `api` service with no `replicas` and no `deploy:`, so
//! process-local state is the whole picture and costs nothing.
//!
//! > **If `api` ever runs as more than one instance, this must move to a
//! > shared store (Redis/Valkey — not Postgres).** With N instances the
//! > effective threshold is N times what [`MAX_FAILURES`] says, and an
//! > attacker spread across them is throttled N times less. Postgres is
//! > the wrong destination even so: one write per failed attempt is itself
//! > an amplification — the attacker would be choosing how often this
//! > service writes to its own database.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Failures within [`WINDOW`], on one (address, email) pair, before the
/// pair is locked. Ten leaves a household member room to mistype — the
/// pair is *their* address and *their* address alone — while capping an
/// attacker at ten argon2id runs per address per email per window.
pub const MAX_FAILURES: u32 = 10;

/// How long failures accumulate. A pair that fails nine times and then
/// goes quiet for this long starts from zero.
pub const WINDOW: Duration = Duration::from_secs(15 * 60);

/// How long a pair stays locked once [`MAX_FAILURES`] is reached.
pub const LOCKOUT: Duration = Duration::from_secs(15 * 60);

/// Ceiling on tracked pairs. The email half of the key is caller-supplied
/// and `login` does not validate its shape, so without a ceiling an
/// attacker picks how much memory this process holds.
pub const MAX_TRACKED: usize = 10_000;

/// Longest email kept in a key: RFC 5321's maximum path length. Anything
/// longer cannot be a deliverable address, and truncating bounds the key
/// rather than the map alone.
const MAX_KEY_EMAIL_LEN: usize = 320;

/// What [`LoginThrottle::check`] says about a pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Not locked — the caller may do the expensive work.
    Allow,
    /// Locked; `retry_after` is what is left of [`LOCKOUT`].
    Locked { retry_after: Duration },
}

/// Key for one (address, email) pair, with the email normalised so
/// `Alice@Example.test` and `alice@example.test` cannot be used as two
/// separate budgets against the same account.
pub fn key(ip: IpAddr, email: &str) -> (IpAddr, String) {
    let mut email = email.trim().to_lowercase();
    let mut cut = MAX_KEY_EMAIL_LEN.min(email.len());
    while cut > 0 && !email.is_char_boundary(cut) {
        cut -= 1;
    }
    email.truncate(cut);
    (ip, email)
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    failures: u32,
    window_start: Instant,
    locked_until: Option<Instant>,
}

impl Entry {
    fn is_stale(&self, now: Instant) -> bool {
        let unlocked = match self.locked_until {
            Some(until) => until <= now,
            None => true,
        };
        unlocked && now.duration_since(self.window_start) > WINDOW
    }
}

/// The counter itself. Cloneable only behind an `Arc` (it is held in
/// `AppState`), and every method takes `&self`.
#[derive(Debug, Default)]
pub struct LoginThrottle {
    entries: Mutex<HashMap<(IpAddr, String), Entry>>,
}

impl LoginThrottle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this pair may attempt a login now. Consulted **before** the
    /// argon2 work, which is the entire point of the throttle.
    pub fn check(&self, key: &(IpAddr, String), now: Instant) -> Decision {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        match entries.get(key).and_then(|e| e.locked_until) {
            Some(until) if until > now => Decision::Locked {
                retry_after: until.duration_since(now),
            },
            _ => Decision::Allow,
        }
    }

    /// Records one failed attempt, locking the pair when it reaches
    /// [`MAX_FAILURES`] inside [`WINDOW`].
    pub fn record_failure(&self, key: &(IpAddr, String), now: Instant) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(entry) = entries.get_mut(key) {
            if entry.is_stale(now) {
                *entry = Entry {
                    failures: 0,
                    window_start: now,
                    locked_until: None,
                };
            }
            entry.failures += 1;
            if entry.failures >= MAX_FAILURES {
                entry.locked_until = Some(now + LOCKOUT);
                entry.failures = 0;
                entry.window_start = now;
            }
            return;
        }

        if entries.len() >= MAX_TRACKED {
            entries.retain(|_, e| !e.is_stale(now));
        }
        if entries.len() >= MAX_TRACKED {
            // Full of live entries: stop tracking new pairs rather than
            // grow without bound. Failing open is deliberate — failing
            // closed would turn a full table into a lockout of everyone,
            // which is the outcome the throttle exists to prevent.
            tracing::warn!(
                tracked = entries.len(),
                "login throttle at capacity, new pairs are not counted"
            );
            return;
        }
        entries.insert(
            key.clone(),
            Entry {
                failures: 1,
                window_start: now,
                locked_until: None,
            },
        );
    }

    /// Clears the pair: a successful login means the address is not the
    /// one the throttle is there for.
    pub fn record_success(&self, key: &(IpAddr, String)) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.remove(key);
    }

    /// Pairs currently held. Exists for the memory-ceiling tests and for
    /// an operator reading a heap dump; nothing in the handler calls it.
    pub fn tracked(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn a_pair_with_no_history_is_allowed() {
        let throttle = LoginThrottle::new();
        let k = key(ip("192.168.1.42"), "alice@example.test");
        assert_eq!(throttle.check(&k, t0()), Decision::Allow);
    }

    #[test]
    fn failures_below_the_threshold_do_not_lock() {
        let throttle = LoginThrottle::new();
        let now = t0();
        let k = key(ip("192.168.1.42"), "alice@example.test");
        for _ in 0..(MAX_FAILURES - 1) {
            throttle.record_failure(&k, now);
        }
        assert_eq!(throttle.check(&k, now), Decision::Allow);
    }

    #[test]
    fn the_threshold_failure_locks_the_pair() {
        let throttle = LoginThrottle::new();
        let now = t0();
        let k = key(ip("192.168.1.42"), "alice@example.test");
        for _ in 0..MAX_FAILURES {
            throttle.record_failure(&k, now);
        }
        match throttle.check(&k, now) {
            Decision::Locked { retry_after } => assert_eq!(retry_after, LOCKOUT),
            Decision::Allow => panic!("expected the pair to be locked"),
        }
    }

    #[test]
    fn the_lock_lifts_once_the_lockout_has_run_out() {
        let throttle = LoginThrottle::new();
        let now = t0();
        let k = key(ip("192.168.1.42"), "alice@example.test");
        for _ in 0..MAX_FAILURES {
            throttle.record_failure(&k, now);
        }
        assert!(matches!(
            throttle.check(&k, now + LOCKOUT - Duration::from_secs(1)),
            Decision::Locked { .. }
        ));
        assert_eq!(throttle.check(&k, now + LOCKOUT), Decision::Allow);
    }

    #[test]
    fn failures_spread_wider_than_the_window_never_add_up() {
        let throttle = LoginThrottle::new();
        let mut now = t0();
        let k = key(ip("192.168.1.42"), "alice@example.test");
        for _ in 0..(MAX_FAILURES * 3) {
            throttle.record_failure(&k, now);
            now += WINDOW + Duration::from_secs(1);
            assert_eq!(throttle.check(&k, now), Decision::Allow);
        }
    }

    #[test]
    fn a_locked_pair_does_not_lock_the_same_address_on_another_email() {
        // The throttle bounds mass exploitation; it must not let one
        // mistyped account take the address down for the whole household.
        let throttle = LoginThrottle::new();
        let now = t0();
        let locked = key(ip("192.168.1.42"), "alice@example.test");
        let other = key(ip("192.168.1.42"), "bob@example.test");
        for _ in 0..MAX_FAILURES {
            throttle.record_failure(&locked, now);
        }
        assert!(matches!(
            throttle.check(&locked, now),
            Decision::Locked { .. }
        ));
        assert_eq!(throttle.check(&other, now), Decision::Allow);
    }

    #[test]
    fn a_locked_pair_does_not_lock_the_same_email_from_another_address() {
        // This is why the key is not the email alone: otherwise anyone who
        // knows an address locks its owner out from anywhere.
        let throttle = LoginThrottle::new();
        let now = t0();
        let attacker = key(ip("203.0.113.9"), "alice@example.test");
        let owner = key(ip("192.168.1.42"), "alice@example.test");
        for _ in 0..MAX_FAILURES {
            throttle.record_failure(&attacker, now);
        }
        assert!(matches!(
            throttle.check(&attacker, now),
            Decision::Locked { .. }
        ));
        assert_eq!(throttle.check(&owner, now), Decision::Allow);
    }

    #[test]
    fn a_success_clears_the_failures_that_came_before_it() {
        let throttle = LoginThrottle::new();
        let now = t0();
        let k = key(ip("192.168.1.42"), "alice@example.test");
        for _ in 0..(MAX_FAILURES - 1) {
            throttle.record_failure(&k, now);
        }
        throttle.record_success(&k);
        for _ in 0..(MAX_FAILURES - 1) {
            throttle.record_failure(&k, now);
        }
        assert_eq!(throttle.check(&k, now), Decision::Allow);
    }

    #[test]
    fn the_key_folds_case_and_surrounding_space() {
        assert_eq!(
            key(ip("192.168.1.42"), "  Alice@Example.TEST "),
            key(ip("192.168.1.42"), "alice@example.test")
        );
    }

    #[test]
    fn the_key_bounds_an_email_the_caller_made_arbitrarily_long() {
        // `login` does not validate the email's shape before looking it
        // up, so the key must bound itself.
        let (_, email) = key(ip("192.168.1.42"), &"a".repeat(100_000));
        assert_eq!(email.len(), MAX_KEY_EMAIL_LEN);
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let (_, email) = key(ip("192.168.1.42"), &"é".repeat(1_000));
        assert!(email.len() <= MAX_KEY_EMAIL_LEN);
        assert!(email.chars().all(|c| c == 'é'));
    }

    #[test]
    fn stale_pairs_are_reclaimed_rather_than_accumulated() {
        let throttle = LoginThrottle::new();
        let now = t0();
        for i in 0..MAX_TRACKED {
            throttle.record_failure(&key(ip("203.0.113.9"), &format!("{i}@example.test")), now);
        }
        assert_eq!(throttle.tracked(), MAX_TRACKED);

        // One more, long after every entry above went stale.
        let later = now + WINDOW + Duration::from_secs(1);
        throttle.record_failure(&key(ip("203.0.113.9"), "fresh@example.test"), later);
        assert_eq!(throttle.tracked(), 1);
    }

    #[test]
    fn the_map_never_grows_past_its_ceiling() {
        let throttle = LoginThrottle::new();
        let now = t0();
        for i in 0..(MAX_TRACKED + 500) {
            throttle.record_failure(&key(ip("203.0.113.9"), &format!("{i}@example.test")), now);
        }
        assert_eq!(throttle.tracked(), MAX_TRACKED);
    }
}
