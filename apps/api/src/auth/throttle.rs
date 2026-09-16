//! Per-(address, email) throttle on login attempts (#178, piste 3).
//!
//! The decoy hash that closes the enumeration oracle makes *every* invalid
//! login pay a full argon2id — around 256 ms of CPU on the measurement in
//! #178, where an unknown email used to cost 0,22 ms. That is the remedy
//! and the new exposure at once: the cheapest possible request now buys
//! the most expensive possible work. So the throttle decides **before** the
//! hashing, never after: a lock that fires after the CPU has been spent
//! protects nothing.
//!
//! **An attempt is counted when it is admitted, not when it fails.**
//! [`LoginThrottle::admit`] checks the lock and records the attempt under
//! one mutex acquisition, before the caller does any work; a login that
//! then succeeds clears the pair ([`LoginThrottle::record_success`]).
//! Counting failures only once they are known would let a burst of
//! concurrent requests all read "not locked" before the first of them
//! finished its argon2id — the bound would then be the concurrency the
//! attacker chooses, not [`MAX_FAILURES`]. Counted at admission, a pair
//! gets at most `MAX_FAILURES` hashes per window however many requests
//! arrive at once. The price is that an attempt that ends in a 500 counts
//! as a failure too; it is rare, and not counting it would reopen the
//! window this closes.
//!
//! **Key: (address, email), never the email alone.** A per-account lock is
//! a denial of service anyone can aim at any address they know, which on a
//! household application means locking a family member out of their own
//! home. The pair bounds mass exploitation — the thing the oracle enables
//! — without handing out that weapon. That property only holds if the
//! address is really the client's: see `apps/api/README.md` for the
//! deployments where every client arrives with the same address.
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

/// Attempts admitted within [`WINDOW`], on one (address, email) pair, with
/// no success in between, before the pair is locked. Ten leaves a household
/// member room to mistype — the pair is *their* address and *their* address
/// alone — while capping an attacker at ten argon2id runs per address per
/// email per window, concurrent requests included.
pub const MAX_FAILURES: u32 = 10;

/// How long attempts accumulate. A pair that fails nine times and then
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

/// What [`LoginThrottle::admit`] says about a pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Admitted, and already counted — the caller may do the expensive work.
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
    attempts: u32,
    window_start: Instant,
    locked_until: Option<Instant>,
}

impl Entry {
    fn fresh(now: Instant) -> Self {
        Entry {
            attempts: 0,
            window_start: now,
            locked_until: None,
        }
    }

    fn locked_at(&self, now: Instant) -> Option<Instant> {
        self.locked_until.filter(|until| *until > now)
    }

    /// Nothing left to remember: not locked, and its window is over.
    fn is_stale(&self, now: Instant) -> bool {
        self.locked_at(now).is_none()
            && (self.locked_until.is_some() || now.duration_since(self.window_start) > WINDOW)
    }
}

/// The counter itself. Held in `AppState` behind an `Arc`; every method
/// takes `&self`.
#[derive(Debug, Default)]
pub struct LoginThrottle {
    entries: Mutex<HashMap<(IpAddr, String), Entry>>,
}

impl LoginThrottle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks the lock **and** counts this attempt, atomically. Called
    /// before the lookup and before argon2id; see the module doc for why
    /// counting cannot wait for the outcome.
    ///
    /// The attempt that reaches [`MAX_FAILURES`] is still admitted and sets
    /// the lock for the ones after it.
    pub fn admit(&self, key: &(IpAddr, String), now: Instant) -> Decision {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());

        if !entries.contains_key(key) {
            make_room(&mut entries, now);
            entries.insert(key.clone(), Entry::fresh(now));
        }
        let entry = entries.get_mut(key).expect("inserted above");

        if let Some(until) = entry.locked_at(now) {
            return Decision::Locked {
                retry_after: until.duration_since(now),
            };
        }
        if entry.is_stale(now) {
            *entry = Entry::fresh(now);
        }
        entry.attempts += 1;
        if entry.attempts >= MAX_FAILURES {
            entry.locked_until = Some(now + LOCKOUT);
        }
        Decision::Allow
    }

    /// Clears the pair: a successful login means the address is not the
    /// one the throttle is there for, and the attempts it made — this one
    /// included — were not an attack.
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

/// Makes room for one more pair without ever answering "not counted".
///
/// Stale entries go first. If the table is still full of live pairs, one is
/// evicted rather than the newcomer being let through untracked — a table
/// that opened the lock when full would be an off switch any single address
/// can flip by cycling through 10 000 emails. The victim is the unlocked
/// pair whose window started earliest; locked pairs are evicted only when
/// nothing else is left, soonest-to-expire first. Filling the table with
/// locked pairs means paying [`MAX_FAILURES`] argon2id runs for each of
/// them, so evicting one costs an attacker far more than it frees.
///
/// Linear in the table size, and only on the path that inserts a new pair
/// into a full table — a path that goes on to an argon2id, next to which a
/// scan of 10 000 entries does not register.
fn make_room(entries: &mut HashMap<(IpAddr, String), Entry>, now: Instant) {
    if entries.len() < MAX_TRACKED {
        return;
    }
    entries.retain(|_, e| !e.is_stale(now));
    if entries.len() < MAX_TRACKED {
        return;
    }
    let victim = entries
        .iter()
        .min_by_key(|(_, e)| match e.locked_at(now) {
            None => (0, e.window_start),
            Some(until) => (1, until),
        })
        .map(|(k, _)| k.clone());
    if let Some(victim) = victim {
        entries.remove(&victim);
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

    fn admit_n(throttle: &LoginThrottle, k: &(IpAddr, String), n: u32, now: Instant) {
        for _ in 0..n {
            assert_eq!(throttle.admit(k, now), Decision::Allow);
        }
    }

    #[test]
    fn a_pair_with_no_history_is_admitted() {
        let throttle = LoginThrottle::new();
        let k = key(ip("192.168.1.42"), "alice@example.test");
        assert_eq!(throttle.admit(&k, t0()), Decision::Allow);
    }

    #[test]
    fn attempts_are_counted_on_admission_not_on_outcome() {
        // Nothing is ever reported back here, exactly like a burst of
        // requests still inside their argon2id: the eleventh is refused
        // all the same.
        let throttle = LoginThrottle::new();
        let now = t0();
        let k = key(ip("192.168.1.42"), "alice@example.test");
        admit_n(&throttle, &k, MAX_FAILURES, now);
        assert_eq!(
            throttle.admit(&k, now),
            Decision::Locked {
                retry_after: LOCKOUT
            }
        );
    }

    #[test]
    fn concurrent_admissions_never_exceed_the_threshold() {
        let throttle = std::sync::Arc::new(LoginThrottle::new());
        let now = t0();
        let k = key(ip("192.168.1.42"), "alice@example.test");
        let handles: Vec<_> = (0..64)
            .map(|_| {
                let throttle = throttle.clone();
                let k = k.clone();
                std::thread::spawn(move || throttle.admit(&k, now) == Decision::Allow)
            })
            .collect();
        let admitted = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|allowed| *allowed)
            .count();
        assert_eq!(admitted, MAX_FAILURES as usize);
    }

    #[test]
    fn the_lock_lifts_once_the_lockout_has_run_out() {
        let throttle = LoginThrottle::new();
        let now = t0();
        let k = key(ip("192.168.1.42"), "alice@example.test");
        admit_n(&throttle, &k, MAX_FAILURES, now);
        assert!(matches!(
            throttle.admit(&k, now + LOCKOUT - Duration::from_secs(1)),
            Decision::Locked { .. }
        ));
        assert_eq!(throttle.admit(&k, now + LOCKOUT), Decision::Allow);
        // And it starts again from a clean count, not one short of a lock.
        admit_n(&throttle, &k, MAX_FAILURES - 1, now + LOCKOUT);
    }

    #[test]
    fn attempts_spread_wider_than_the_window_never_add_up() {
        let throttle = LoginThrottle::new();
        let mut now = t0();
        let k = key(ip("192.168.1.42"), "alice@example.test");
        for _ in 0..(MAX_FAILURES * 3) {
            assert_eq!(throttle.admit(&k, now), Decision::Allow);
            now += WINDOW + Duration::from_secs(1);
        }
    }

    #[test]
    fn a_locked_pair_does_not_lock_the_same_address_on_another_email() {
        let throttle = LoginThrottle::new();
        let now = t0();
        let locked = key(ip("192.168.1.42"), "alice@example.test");
        let other = key(ip("192.168.1.42"), "bob@example.test");
        admit_n(&throttle, &locked, MAX_FAILURES, now);
        assert!(matches!(
            throttle.admit(&locked, now),
            Decision::Locked { .. }
        ));
        assert_eq!(throttle.admit(&other, now), Decision::Allow);
    }

    #[test]
    fn a_locked_pair_does_not_lock_the_same_email_from_another_address() {
        // This is why the key is not the email alone: otherwise anyone who
        // knows an address locks its owner out from anywhere.
        let throttle = LoginThrottle::new();
        let now = t0();
        let attacker = key(ip("203.0.113.9"), "alice@example.test");
        let owner = key(ip("192.168.1.42"), "alice@example.test");
        admit_n(&throttle, &attacker, MAX_FAILURES, now);
        assert!(matches!(
            throttle.admit(&attacker, now),
            Decision::Locked { .. }
        ));
        assert_eq!(throttle.admit(&owner, now), Decision::Allow);
    }

    #[test]
    fn a_success_clears_the_attempts_that_came_before_it() {
        let throttle = LoginThrottle::new();
        let now = t0();
        let k = key(ip("192.168.1.42"), "alice@example.test");
        admit_n(&throttle, &k, MAX_FAILURES - 1, now);
        throttle.record_success(&k);
        admit_n(&throttle, &k, MAX_FAILURES - 1, now);
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
        let (_, email) = key(ip("192.168.1.42"), &"a".repeat(100_000));
        assert_eq!(email.len(), MAX_KEY_EMAIL_LEN);
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let (_, email) = key(ip("192.168.1.42"), &"é".repeat(1_000));
        assert!(email.len() <= MAX_KEY_EMAIL_LEN);
        assert!(email.chars().all(|c| c == 'é'));
    }

    fn fill(throttle: &LoginThrottle, now: Instant) {
        for i in 0..MAX_TRACKED {
            throttle.admit(&key(ip("203.0.113.9"), &format!("{i}@example.test")), now);
        }
        assert_eq!(throttle.tracked(), MAX_TRACKED);
    }

    #[test]
    fn stale_pairs_are_reclaimed_rather_than_accumulated() {
        let throttle = LoginThrottle::new();
        let now = t0();
        fill(&throttle, now);
        let later = now + WINDOW + Duration::from_secs(1);
        throttle.admit(&key(ip("203.0.113.9"), "fresh@example.test"), later);
        assert_eq!(throttle.tracked(), 1);
    }

    #[test]
    fn a_full_table_of_live_pairs_never_grows_past_its_ceiling() {
        let throttle = LoginThrottle::new();
        let now = t0();
        fill(&throttle, now);
        for i in 0..500 {
            throttle.admit(
                &key(ip("203.0.113.9"), &format!("more-{i}@example.test")),
                now,
            );
        }
        assert_eq!(throttle.tracked(), MAX_TRACKED);
    }

    #[test]
    fn a_full_table_does_not_open_the_lock_for_a_new_pair() {
        // The off switch the review found: once 10 000 live pairs were
        // tracked, a new pair was never counted and stayed admitted after
        // any number of attempts.
        let throttle = LoginThrottle::new();
        let now = t0();
        fill(&throttle, now);
        let k = key(ip("203.0.113.9"), "target@example.test");
        admit_n(&throttle, &k, MAX_FAILURES, now);
        assert!(matches!(throttle.admit(&k, now), Decision::Locked { .. }));
    }

    #[test]
    fn eviction_takes_an_unlocked_pair_before_a_locked_one() {
        let throttle = LoginThrottle::new();
        let now = t0();
        let locked = key(ip("203.0.113.9"), "locked@example.test");
        admit_n(&throttle, &locked, MAX_FAILURES, now);
        // Every other pair is younger than the locked one: an eviction by
        // age alone would pick the locked pair and lift its lock.
        let later = now + Duration::from_secs(1);
        for i in 0..(MAX_TRACKED - 1) {
            throttle.admit(&key(ip("203.0.113.9"), &format!("{i}@example.test")), later);
        }
        for i in 0..500 {
            throttle.admit(
                &key(ip("203.0.113.9"), &format!("more-{i}@example.test")),
                later,
            );
        }
        assert!(matches!(
            throttle.admit(&locked, later),
            Decision::Locked { .. }
        ));
    }
}
