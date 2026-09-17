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
//! attacker chooses, not [`MAX_FAILURES`]. The price is that an attempt
//! that ends in a 500 counts as a failure too; it is rare, and not counting
//! it would reopen the window this closes.
//!
//! **Key: (address, email), never the email alone.** A per-account lock is
//! a denial of service anyone can aim at any address they know, which on a
//! household application means locking a family member out of their own
//! home. The pair bounds mass exploitation — the thing the oracle enables
//! — without handing out that weapon. That property only holds if the
//! address is really the client's: see `apps/api/README.md` for the
//! deployments where every client arrives with the same address.
//!
//! **"Address" means the client, not the source address** (#198). In IPv6
//! a household is handed a whole /64 and picks any of the 2^64 addresses
//! in it, so the /128 on the socket is a handle the attacker renews at
//! will: keyed on it, both the lock and the share below bound nothing.
//! [`address_scope`] reduces an address to what identifies the client —
//! its /64 in IPv6, itself in IPv4 — and [`key`] is built on that.
//!
//! **Each address holds at most [`MAX_PAIRS_PER_IP`] pairs.** A new email
//! from an address already at its share is refused (a 429, like a lock)
//! rather than counted by pushing out someone's pair. Without the share, a
//! single address could fill the table and then churn new emails to evict
//! — and so reset — the pair it was attacking: 9 000 attempts admitted on
//! one pair for 1 000 new emails, measured against the previous version.
//! Refusing at the share only ever costs the address that asked.
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
use std::net::{IpAddr, Ipv6Addr};
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

/// Pairs one address may hold at once — distinct emails tried from it
/// that have neither succeeded nor gone stale. 50 per household address is
/// a user decision (#178): far above what a household behind one NAT
/// address types in a quarter of an hour, and a bound on what one address
/// can try — 50 emails, 10 attempts each, per window.
///
/// "One address" is one [`address_scope`], so an IPv6 household spends its
/// 50 from the whole of its /64 rather than 50 per source address it mints.
///
/// It also means a full table ([`MAX_TRACKED`]) spans at least
/// `MAX_TRACKED / MAX_PAIRS_PER_IP` = 200 distinct addresses.
pub const MAX_PAIRS_PER_IP: usize = 50;

/// Longest email kept in a key: RFC 5321's maximum path length. Anything
/// longer cannot be a deliverable address, and truncating bounds the key
/// rather than the map alone.
const MAX_KEY_EMAIL_LEN: usize = 320;

/// What [`LoginThrottle::admit`] says about a pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Admitted, and already counted — the caller may do the expensive work.
    Allow,
    /// Refused before any work: the pair is locked, or its address already
    /// holds its share of pairs. `retry_after` is when that stops being
    /// true at the earliest.
    Locked { retry_after: Duration },
}

/// Prefix an IPv6 client is grouped on (#198). A /64 is the smallest
/// block an end site is delegated — RFC 6177 recommends handing every
/// subscriber at least that, and the address autoconfiguration the
/// household's own router runs (RFC 4862) requires exactly 64 host bits.
/// So the /64 is the subscriber; the 64 bits under it are theirs to pick.
///
/// Wider would be wrong in the other direction: /56 and /48 blocks are
/// handed to *different* subscribers by the same ISP, and grouping there
/// would let one of them lock the others out — the denial of service the
/// (address, email) pair exists to avoid.
///
/// So this does not close rotation entirely. A subscriber delegated a /56
/// still holds 256 distinct /64s and a /48 holds 65 536, and each of them
/// is a separate key here: at least 2 560 and 655 360 attempts per window
/// on one email — a floor, not a ceiling, since eviction from a full table
/// (`Table::make_room`) resets pairs and is itself unbounded. What this
/// removes is the 2^64 the /128 gave away for free. Capping the rotation
/// itself needs a bound above the key, which this file does not have.
///
/// It also does not group two subscribers of the *global unicast* space
/// together, which is the property that matters here. It is not an
/// absolute over the whole address space: `64:ff9b::/96` (NAT64) and the
/// deprecated `::a.b.c.d` both carry an IPv4 address in their low bits, so
/// a source in either form collapses onto one key — `64:ff9b::` or `::` —
/// for every IPv4 client behind the translator. Neither can arrive at the
/// `0.0.0.0` listener this API ships, and neither is canonicalised here;
/// a deployment that puts a translator in front of a `::` listener would
/// have to be handled like `::ffff:` below.
pub const IPV6_GROUP_PREFIX: u32 = 64;

/// The address half of a key: the thing the throttle counts against.
///
/// An IPv4 address stands for itself — a client behind NAT cannot change
/// it, and the blocks above it (a /24, say) span unrelated subscribers.
///
/// An IPv6 address does not: a residential line is routinely delegated a
/// whole [`IPV6_GROUP_PREFIX`] and the source address inside it is the
/// client's own choice, remade as often as it likes. Keyed on the /128 the
/// socket carries, both the lock and the per-address share of pairs are
/// bounded by nothing at all. So the host bits are dropped.
///
/// `::ffff:a.b.c.d` is canonicalised **before** the mask, not after: those
/// addresses all share the `::/64` prefix, so masking first would fold the
/// entire IPv4 internet into a single key — one client's ten wrong
/// passwords would then lock out everyone else. It is unreachable with the
/// `0.0.0.0` listener this API ships (an IPv4 peer arrives as plain IPv4),
/// but a dual-stack listener on `::` would deliver it, and the two forms
/// name the same client either way.
pub fn address_scope(ip: IpAddr) -> IpAddr {
    let IpAddr::V6(v6) = ip else {
        return ip;
    };
    if let Some(v4) = v6.to_ipv4_mapped() {
        return IpAddr::V4(v4);
    }
    let host_bits = 128 - IPV6_GROUP_PREFIX;
    IpAddr::V6(Ipv6Addr::from((u128::from(v6) >> host_bits) << host_bits))
}

/// Key for one (address, email) pair, with the address reduced to the
/// client it really identifies ([`address_scope`]) and the email
/// normalised so `Alice@Example.test` and `alice@example.test` cannot be
/// used as two separate budgets against the same account.
pub fn key(ip: IpAddr, email: &str) -> (IpAddr, String) {
    let ip = address_scope(ip);
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

    /// When this entry stops holding its address's share, at the earliest.
    fn frees_at(&self, now: Instant) -> Instant {
        self.locked_at(now).unwrap_or(self.window_start + WINDOW)
    }
}

/// Pairs grouped by address, so an address's share is one `len()` away.
#[derive(Debug, Default)]
struct Table {
    by_ip: HashMap<IpAddr, HashMap<String, Entry>>,
    total: usize,
}

impl Table {
    fn held_by(&self, ip: IpAddr) -> usize {
        self.by_ip.get(&ip).map_or(0, HashMap::len)
    }

    fn remove(&mut self, ip: IpAddr, email: &str) {
        if let Some(pairs) = self.by_ip.get_mut(&ip) {
            if pairs.remove(email).is_some() {
                self.total -= 1;
            }
            if pairs.is_empty() {
                self.by_ip.remove(&ip);
            }
        }
    }

    fn reclaim_address(&mut self, ip: IpAddr, now: Instant) {
        if let Some(pairs) = self.by_ip.get_mut(&ip) {
            let before = pairs.len();
            pairs.retain(|_, e| !e.is_stale(now));
            self.total -= before - pairs.len();
            if pairs.is_empty() {
                self.by_ip.remove(&ip);
            }
        }
    }

    fn address_frees_up_in(&self, ip: IpAddr, now: Instant) -> Duration {
        self.by_ip
            .get(&ip)
            .and_then(|pairs| pairs.values().map(|e| e.frees_at(now)).min())
            .map_or(Duration::ZERO, |at| at.saturating_duration_since(now))
    }

    /// Makes room for one more pair when the table is full, without ever
    /// letting the newcomer through uncounted.
    ///
    /// Stale entries go first. If the table is still full of live pairs,
    /// one is evicted **from the addresses holding the most pairs**: among
    /// all the pairs of those addresses taken together, the unlocked one
    /// whose window started earliest; a locked one, soonest-to-expire
    /// first, only if none of those addresses holds an unlocked pair. A pair can
    /// therefore only be evicted once no address holds more pairs than its
    /// own: for a pair whose address holds `k` pairs, that takes the table
    /// spread over at least `MAX_TRACKED / k` addresses — 200 at the very
    /// least, 10 000 for an address holding a single pair. One address, or
    /// a handful, cannot evict anyone else's pair.
    ///
    /// **An eviction is not a one-off.** Each one resets up to
    /// [`MAX_FAILURES`] attempts on the evicted pair, its lock included: a
    /// locked pair can be evicted, and then admits ten fresh attempts. An
    /// attacker
    /// who controls enough addresses to keep the table full can replay it
    /// as often as they like within one window: nothing here bounds the
    /// total. The review of #178 measured 27 000 attempts admitted on one
    /// pair for 3 000 cycles (figure from the review, not reproduced here).
    /// The share raises the price of every cycle — a full table of live
    /// pairs, spread over at least 200 addresses — it does not cap the
    /// number of cycles.
    ///
    /// Linear in the table size, and only on the path that inserts a new
    /// pair into a full table.
    fn make_room(&mut self, now: Instant) {
        if self.total < MAX_TRACKED {
            return;
        }
        let mut total = 0;
        self.by_ip.retain(|_, pairs| {
            pairs.retain(|_, e| !e.is_stale(now));
            total += pairs.len();
            !pairs.is_empty()
        });
        self.total = total;
        if self.total < MAX_TRACKED {
            return;
        }
        let most = self.by_ip.values().map(HashMap::len).max().unwrap_or(0);
        let victim = self
            .by_ip
            .iter()
            .filter(|(_, pairs)| pairs.len() == most)
            .flat_map(|(ip, pairs)| pairs.iter().map(move |(email, e)| (*ip, email, e)))
            .min_by_key(|(_, _, e)| match e.locked_at(now) {
                None => (0, e.window_start),
                Some(until) => (1, until),
            })
            .map(|(ip, email, _)| (ip, email.clone()));
        if let Some((ip, email)) = victim {
            self.remove(ip, &email);
        }
    }
}

/// The counter itself. Held in `AppState` behind an `Arc`; every method
/// takes `&self`.
#[derive(Debug, Default)]
pub struct LoginThrottle {
    table: Mutex<Table>,
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
        let mut table = self.table.lock().unwrap_or_else(|e| e.into_inner());
        let (ip, email) = (key.0, key.1.as_str());

        let known = table
            .by_ip
            .get(&ip)
            .is_some_and(|pairs| pairs.contains_key(email));
        if !known {
            table.reclaim_address(ip, now);
            if table.held_by(ip) >= MAX_PAIRS_PER_IP {
                return Decision::Locked {
                    retry_after: table.address_frees_up_in(ip, now),
                };
            }
            table.make_room(now);
            table
                .by_ip
                .entry(ip)
                .or_default()
                .insert(email.to_owned(), Entry::fresh(now));
            table.total += 1;
        }
        let entry = table
            .by_ip
            .get_mut(&ip)
            .and_then(|pairs| pairs.get_mut(email))
            .expect("present or inserted above");

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
        let mut table = self.table.lock().unwrap_or_else(|e| e.into_inner());
        table.remove(key.0, &key.1);
    }

    /// Pairs currently held. Exists for the memory-ceiling tests and for
    /// an operator reading a heap dump; nothing in the handler calls it.
    pub fn tracked(&self) -> usize {
        self.table.lock().unwrap_or_else(|e| e.into_inner()).total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn t0() -> Instant {
        Instant::now()
    }

    /// The `i`-th of the addresses a table is filled from, each holding at
    /// most its share of pairs.
    fn bulk_ip(i: usize) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(0x0A00_0000 + (i / MAX_PAIRS_PER_IP) as u32))
    }

    /// A fresh address, disjoint from every `bulk_ip`.
    fn fresh_ip(j: usize) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(0x0B00_0000 + j as u32))
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

    // --- #198: what counts as "one address" ---

    #[test]
    fn an_ipv6_client_cannot_lift_its_lock_by_moving_inside_its_block() {
        // A residential IPv6 line is handed a whole /64. Keyed on the /128
        // the socket happens to carry, the client picks a new source
        // address and starts from zero, as often as it likes.
        let throttle = LoginThrottle::new();
        let now = t0();
        let first = key(ip("2001:db8:1:2::1"), "alice@example.test");
        admit_n(&throttle, &first, MAX_FAILURES, now);
        assert!(matches!(
            throttle.admit(&first, now),
            Decision::Locked { .. }
        ));

        for host in ["2001:db8:1:2::2", "2001:db8:1:2:ffff:ffff:ffff:ffff"] {
            assert!(
                matches!(
                    throttle.admit(&key(ip(host), "alice@example.test"), now),
                    Decision::Locked { .. }
                ),
                "{host} bought a fresh budget"
            );
        }
    }

    #[test]
    fn a_neighbouring_ipv6_block_is_a_different_client() {
        // The other half of the grouping: it must not reach past the /64
        // and lock the subscriber next door, which is the denial of
        // service the (address, email) pair exists to avoid.
        let throttle = LoginThrottle::new();
        let now = t0();
        let attacker = key(ip("2001:db8:1:2::1"), "alice@example.test");
        admit_n(&throttle, &attacker, MAX_FAILURES, now);
        assert_eq!(
            throttle.admit(&key(ip("2001:db8:1:3::1"), "alice@example.test"), now),
            Decision::Allow
        );
    }

    #[test]
    fn an_ipv6_client_cannot_renew_its_share_by_moving_inside_its_block() {
        // The share is held by the block too: 50 emails per /64, not 50
        // per address the client is free to mint.
        let throttle = LoginThrottle::new();
        let now = t0();
        for i in 0..MAX_PAIRS_PER_IP {
            let k = key(
                ip(&format!("2001:db8:1:2::{i:x}")),
                &format!("{i}@example.test"),
            );
            assert_eq!(throttle.admit(&k, now), Decision::Allow, "email {i}");
        }
        let extra = key(ip("2001:db8:1:2:ffff::1"), "one-more@example.test");
        assert!(matches!(
            throttle.admit(&extra, now),
            Decision::Locked { .. }
        ));
    }

    #[test]
    fn an_ipv4_mapped_address_is_the_same_client_as_the_plain_ipv4() {
        assert_eq!(
            key(ip("::ffff:192.168.1.42"), "alice@example.test"),
            key(ip("192.168.1.42"), "alice@example.test")
        );
        let throttle = LoginThrottle::new();
        let now = t0();
        let mapped = key(ip("::ffff:192.168.1.42"), "alice@example.test");
        admit_n(&throttle, &mapped, MAX_FAILURES, now);
        assert!(matches!(
            throttle.admit(&key(ip("192.168.1.42"), "alice@example.test"), now),
            Decision::Locked { .. }
        ));
    }

    #[test]
    fn two_ipv4_mapped_addresses_are_still_two_clients() {
        // The trap of masking before canonicalising: every `::ffff:a.b.c.d`
        // shares the `::/64` prefix, so a /64 mask applied first would fold
        // the entire IPv4 internet into one key — one household's mistyped
        // passwords would lock every other household out.
        let throttle = LoginThrottle::new();
        let now = t0();
        let attacker = key(ip("::ffff:203.0.113.9"), "alice@example.test");
        admit_n(&throttle, &attacker, MAX_FAILURES, now);
        assert_eq!(
            throttle.admit(&key(ip("::ffff:192.168.1.42"), "alice@example.test"), now),
            Decision::Allow
        );
    }

    #[test]
    fn the_scope_keeps_an_ipv4_address_whole() {
        // No grouping on IPv4: a /24 there spans different subscribers.
        assert_eq!(address_scope(ip("203.0.113.9")), ip("203.0.113.9"));
    }

    #[test]
    fn the_scope_is_the_ipv6_block_and_nothing_of_the_host_part() {
        assert_eq!(
            address_scope(ip("2001:db8:1:2:3:4:5:6")),
            ip("2001:db8:1:2::")
        );
        assert_eq!(address_scope(ip("::1")), ip("::"));
    }

    #[test]
    fn an_address_tracks_at_most_its_share_of_pairs() {
        let throttle = LoginThrottle::new();
        let now = t0();
        let attacker = ip("203.0.113.9");
        for i in 0..MAX_PAIRS_PER_IP {
            let k = key(attacker, &format!("{i}@example.test"));
            assert_eq!(throttle.admit(&k, now), Decision::Allow);
        }
        // One email more is refused rather than counted somewhere else...
        let extra = key(attacker, "one-more@example.test");
        assert!(matches!(
            throttle.admit(&extra, now),
            Decision::Locked { .. }
        ));
        // ...the pairs it already holds keep their own budget...
        assert_eq!(
            throttle.admit(&key(attacker, "0@example.test"), now),
            Decision::Allow
        );
        // ...and nobody else pays for it.
        assert_eq!(
            throttle.admit(&key(ip("192.168.1.42"), "one-more@example.test"), now),
            Decision::Allow
        );
    }

    #[test]
    fn a_household_address_may_try_fifty_emails_and_not_one_more() {
        // The share is a user decision (#178, "50 emails par foyer"), so it
        // is pinned in literals here rather than read back from the
        // constant: changing it has to change this test too.
        let throttle = LoginThrottle::new();
        let now = t0();
        let household = ip("192.168.1.42");
        for i in 0..50 {
            let k = key(household, &format!("member-{i}@example.test"));
            assert_eq!(throttle.admit(&k, now), Decision::Allow, "email {i}");
        }
        assert!(matches!(
            throttle.admit(&key(household, "member-50@example.test"), now),
            Decision::Locked { .. }
        ));
        // And the per-(address, email) lock still holds inside the share.
        let k = key(household, "member-0@example.test");
        admit_n(&throttle, &k, MAX_FAILURES - 1, now);
        assert!(matches!(throttle.admit(&k, now), Decision::Locked { .. }));
    }

    #[test]
    fn the_share_frees_up_as_the_address_pairs_go_stale() {
        let throttle = LoginThrottle::new();
        let now = t0();
        let attacker = ip("203.0.113.9");
        for i in 0..MAX_PAIRS_PER_IP {
            throttle.admit(&key(attacker, &format!("{i}@example.test")), now);
        }
        let later = now + WINDOW + Duration::from_secs(1);
        assert_eq!(
            throttle.admit(&key(attacker, "next@example.test"), later),
            Decision::Allow
        );
    }

    #[test]
    fn one_address_cannot_reset_a_pair_by_churning_new_emails() {
        // The probe from review round 3, against the previous eviction:
        // one address locks as many pairs as it can, then alternates nine
        // attempts on its target with one brand-new email. Evicting the
        // oldest unlocked pair reset the target on every new email — 9 000
        // attempts admitted for 1 000 new emails.
        let throttle = LoginThrottle::new();
        let now = t0();
        let attacker = ip("203.0.113.9");
        for i in 0..(MAX_TRACKED - 1) {
            let k = key(attacker, &format!("filler-{i}@example.test"));
            for _ in 0..MAX_FAILURES {
                throttle.admit(&k, now);
            }
        }
        let target = key(attacker, "victim@example.test");
        let mut admitted_on_target = 0;
        for c in 0..1_000 {
            for _ in 0..(MAX_FAILURES - 1) {
                if throttle.admit(&target, now) == Decision::Allow {
                    admitted_on_target += 1;
                }
            }
            throttle.admit(&key(attacker, &format!("new-{c}@example.test")), now);
        }
        assert!(
            admitted_on_target <= MAX_FAILURES,
            "{admitted_on_target} attempts admitted on one pair"
        );
    }

    fn fill(throttle: &LoginThrottle, now: Instant) {
        for i in 0..MAX_TRACKED {
            throttle.admit(&key(bulk_ip(i), &format!("{i}@example.test")), now);
        }
        assert_eq!(throttle.tracked(), MAX_TRACKED);
    }

    #[test]
    fn stale_pairs_are_reclaimed_rather_than_accumulated() {
        let throttle = LoginThrottle::new();
        let now = t0();
        fill(&throttle, now);
        let later = now + WINDOW + Duration::from_secs(1);
        throttle.admit(&key(fresh_ip(0), "fresh@example.test"), later);
        assert_eq!(throttle.tracked(), 1);
    }

    #[test]
    fn a_full_table_of_live_pairs_never_grows_past_its_ceiling() {
        let throttle = LoginThrottle::new();
        let now = t0();
        fill(&throttle, now);
        for j in 0..500 {
            throttle.admit(&key(fresh_ip(j), "more@example.test"), now);
        }
        assert_eq!(throttle.tracked(), MAX_TRACKED);
    }

    #[test]
    fn a_full_table_does_not_open_the_lock_for_a_new_pair() {
        let throttle = LoginThrottle::new();
        let now = t0();
        fill(&throttle, now);
        let k = key(fresh_ip(0), "target@example.test");
        admit_n(&throttle, &k, MAX_FAILURES, now);
        assert!(matches!(throttle.admit(&k, now), Decision::Locked { .. }));
    }

    #[test]
    fn eviction_takes_from_the_addresses_holding_the_most_pairs() {
        // The target pair is the oldest in the table and unlocked — the
        // first to go under eviction by age. Its address holds one pair;
        // the bulk addresses hold up to their share each. A thousand new
        // addresses arriving must not evict it.
        let throttle = LoginThrottle::new();
        let now = t0();
        let target = key(ip("192.168.1.42"), "victim@example.test");
        admit_n(&throttle, &target, MAX_FAILURES - 1, now);

        let later = now + Duration::from_secs(1);
        fill(&throttle, later);
        for j in 0..1_000 {
            throttle.admit(&key(fresh_ip(j), "new@example.test"), later);
        }

        assert_eq!(throttle.admit(&target, later), Decision::Allow);
        assert!(matches!(
            throttle.admit(&target, later),
            Decision::Locked { .. }
        ));
    }

    #[test]
    fn eviction_takes_an_unlocked_pair_before_a_locked_one() {
        let throttle = LoginThrottle::new();
        let now = t0();
        let locked = key(bulk_ip(0), "locked@example.test");
        admit_n(&throttle, &locked, MAX_FAILURES, now);
        let later = now + Duration::from_secs(1);
        for i in 1..MAX_TRACKED {
            throttle.admit(&key(bulk_ip(i), &format!("{i}@example.test")), later);
        }
        for j in 0..500 {
            throttle.admit(&key(fresh_ip(j), "more@example.test"), later);
        }
        assert!(matches!(
            throttle.admit(&locked, later),
            Decision::Locked { .. }
        ));
    }
}
