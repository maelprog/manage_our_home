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
//! [`LoginThrottle::admit`] checks the lock and records the attempt in one
//! transaction, under one lock, before the caller does any work; a login that
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
//! **The counters live in Postgres (#427), the decision does not.** Until
//! #427 they lived in this process's memory: with N replicas behind the
//! ingress an attacker had N times [`MAX_FAILURES`], and every deployment
//! reset them. They are now rows of the `login_throttle` table
//! (`migrations/0028_login_throttle.sql`), shared by every replica and
//! kept across restarts. What decides is unchanged and pure: [`Table`],
//! the same structure the process used to hold, now filled with the rows
//! one admission needs, asked, and diffed back ([`Changes`]).
//!
//! One admission is one transaction:
//!
//! - a transaction-level advisory lock on the address ([`SCOPE_LOCK`])
//!   does what the mutex did, per address: two attempts from one address,
//!   whichever replica takes them, are decided one after the other — so a
//!   burst still gets [`MAX_FAILURES`] admissions and not one per request,
//!   and a 51st email cannot slip past the share;
//! - a pair the table does not hold yet also takes [`TABLE_LOCK`], since
//!   inserting it counts against [`MAX_TRACKED`] and may evict another
//!   address's pair. Under it, rows past [`purge_cutoff`] are deleted, the
//!   table is counted, and only when it is full is all of it loaded — the
//!   linear path the in-memory table already had;
//! - the clock is the database's (`clock_timestamp()`), so replicas whose
//!   clocks drift still agree on when a window ends.
//!
//! The cost the in-memory version was spared: a database round trip per
//! attempt, and a pool connection held for it. Counter writes stay
//! bounded: a refused attempt changes no counter, and an admitted one
//! writes one row — at most [`MAX_FAILURES`] per pair per window, each in
//! front of an argon2id that costs far more than the write. (A pair the
//! table does not hold yet also runs the purge, which deletes only rows
//! already past [`purge_cutoff`].)

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};

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
/// attacker picks how many rows `login_throttle` holds.
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

/// Rows whose window started at or before this instant are stale whatever
/// happened in them, and can go without being read.
///
/// A window is fresh until [`WINDOW`] has elapsed, so the latest a pair can
/// lock is at the very end of it, and that lock lifts [`LOCKOUT`] later:
/// `window_start + WINDOW + LOCKOUT` bounds every entry's life. This is a
/// bound, not [`Entry::is_stale`]: a pair that never locked goes stale
/// sooner, and is then reclaimed by the table as before.
pub fn purge_cutoff(now: SystemTime) -> SystemTime {
    now - (WINDOW + LOCKOUT)
}

/// Instants are wall-clock (`SystemTime`), not `Instant`, since #427: they
/// are written to the database and read back by another process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    attempts: u32,
    window_start: SystemTime,
    locked_until: Option<SystemTime>,
}

/// How long from `earlier` to `later`, or zero if `later` is not later.
fn since(later: SystemTime, earlier: SystemTime) -> Duration {
    later.duration_since(earlier).unwrap_or(Duration::ZERO)
}

impl Entry {
    fn fresh(now: SystemTime) -> Self {
        Entry {
            attempts: 0,
            window_start: now,
            locked_until: None,
        }
    }

    fn locked_at(&self, now: SystemTime) -> Option<SystemTime> {
        self.locked_until.filter(|until| *until > now)
    }

    /// Nothing left to remember: not locked, and its window is over.
    fn is_stale(&self, now: SystemTime) -> bool {
        self.locked_at(now).is_none()
            && (self.locked_until.is_some() || since(now, self.window_start) > WINDOW)
    }

    /// When this entry stops holding its address's share, at the earliest.
    fn frees_at(&self, now: SystemTime) -> SystemTime {
        self.locked_at(now).unwrap_or(self.window_start + WINDOW)
    }
}

/// Pairs grouped by address, so an address's share is one `len()` away.
///
/// The whole decision. [`LoginThrottle`] fills one with the rows an
/// admission needs — the address's own pairs, or every pair when the
/// table is full — asks it, and writes back what changed.
#[derive(Debug, Default, Clone)]
struct Table {
    by_ip: HashMap<IpAddr, HashMap<String, Entry>>,
    total: usize,
}

impl Table {
    fn get(&self, ip: IpAddr, email: &str) -> Option<&Entry> {
        self.by_ip.get(&ip).and_then(|pairs| pairs.get(email))
    }

    /// Adds a pair read from the store.
    fn load(&mut self, ip: IpAddr, email: String, entry: Entry) {
        if self
            .by_ip
            .entry(ip)
            .or_default()
            .insert(email, entry)
            .is_none()
        {
            self.total += 1;
        }
    }

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

    fn reclaim_address(&mut self, ip: IpAddr, now: SystemTime) {
        if let Some(pairs) = self.by_ip.get_mut(&ip) {
            let before = pairs.len();
            pairs.retain(|_, e| !e.is_stale(now));
            self.total -= before - pairs.len();
            if pairs.is_empty() {
                self.by_ip.remove(&ip);
            }
        }
    }

    fn address_frees_up_in(&self, ip: IpAddr, now: SystemTime) -> Duration {
        self.by_ip
            .get(&ip)
            .and_then(|pairs| pairs.values().map(|e| e.frees_at(now)).min())
            .map_or(Duration::ZERO, |at| since(at, now))
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
    fn make_room(&mut self, now: SystemTime) {
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

impl Table {
    /// Checks the lock **and** counts this attempt, in one step. Called
    /// before the lookup and before argon2id; see the module doc for why
    /// counting cannot wait for the outcome.
    ///
    /// The attempt that reaches [`MAX_FAILURES`] is still admitted and sets
    /// the lock for the ones after it.
    fn admit(&mut self, ip: IpAddr, email: &str, now: SystemTime) -> Decision {
        if self.get(ip, email).is_none() {
            self.reclaim_address(ip, now);
            if self.held_by(ip) >= MAX_PAIRS_PER_IP {
                return Decision::Locked {
                    retry_after: self.address_frees_up_in(ip, now),
                };
            }
            self.make_room(now);
            self.by_ip
                .entry(ip)
                .or_default()
                .insert(email.to_owned(), Entry::fresh(now));
            self.total += 1;
        }
        let entry = self
            .by_ip
            .get_mut(&ip)
            .and_then(|pairs| pairs.get_mut(email))
            .expect("present or inserted above");

        if let Some(until) = entry.locked_at(now) {
            return Decision::Locked {
                retry_after: since(until, now),
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
}

/// What one admission changed in the rows it was handed: what the store
/// has to delete, and what it has to write.
#[derive(Debug, Default, PartialEq, Eq)]
struct Changes {
    removed: Vec<(IpAddr, String)>,
    written: Vec<(IpAddr, String, Entry)>,
}

impl Changes {
    /// `before` is the table as loaded, `after` the same table once asked.
    fn between(before: &Table, after: &Table) -> Self {
        let mut changes = Changes::default();
        for (ip, pairs) in &before.by_ip {
            for email in pairs.keys() {
                if after.get(*ip, email).is_none() {
                    changes.removed.push((*ip, email.clone()));
                }
            }
        }
        for (ip, pairs) in &after.by_ip {
            for (email, entry) in pairs {
                if before.get(*ip, email) != Some(entry) {
                    changes.written.push((*ip, email.clone(), *entry));
                }
            }
        }
        changes
    }
}

/// Advisory lock class of one address's pairs. The two-key form of
/// `pg_advisory_xact_lock` lives in a key space of its own, apart from the
/// single 64-bit keys of the background jobs (#426) and of sqlx's
/// migrator; the second key is `hashtext` of the address, and two
/// addresses that collide only wait on each other.
pub const SCOPE_LOCK: i32 = i32::from_be_bytes(*b"mlgs");

/// Advisory lock class of the table as a whole, taken (with second key 0)
/// by an admission that inserts a pair: the ceiling, the purge and the
/// eviction are about every address at once. Always taken after
/// [`SCOPE_LOCK`], never before, so the two cannot deadlock.
pub const TABLE_LOCK: i32 = i32::from_be_bytes(*b"mlgt");

/// The counters, in Postgres. Held in `AppState` behind an `Arc`; every
/// method takes `&self`. Each replica builds its own, on the same table.
#[derive(Debug, Clone)]
pub struct LoginThrottle {
    db: PgPool,
}

/// A stored row, as the decision reads it.
fn entry_from_row(
    attempts: i32,
    window_start: DateTime<Utc>,
    locked_until: Option<DateTime<Utc>>,
) -> Entry {
    Entry {
        attempts: u32::try_from(attempts).unwrap_or(0),
        window_start: window_start.into(),
        locked_until: locked_until.map(Into::into),
    }
}

/// Takes [`SCOPE_LOCK`] on one address, until the transaction ends.
async fn lock_address(
    tx: &mut Transaction<'static, Postgres>,
    ip: IpAddr,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))")
        .bind(SCOPE_LOCK)
        .bind(ip.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Takes [`TABLE_LOCK`], until the transaction ends.
async fn lock_table(tx: &mut Transaction<'static, Postgres>) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock($1, 0)")
        .bind(TABLE_LOCK)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn load_address(
    tx: &mut Transaction<'static, Postgres>,
    ip: IpAddr,
    table: &mut Table,
) -> Result<(), sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT email, attempts, window_start, locked_until
           FROM login_throttle WHERE scope = $1::text::inet",
        ip.to_string()
    )
    .fetch_all(&mut **tx)
    .await?;
    for r in rows {
        table.load(
            ip,
            r.email,
            entry_from_row(r.attempts, r.window_start, r.locked_until),
        );
    }
    Ok(())
}

async fn load_all(
    tx: &mut Transaction<'static, Postgres>,
    table: &mut Table,
) -> Result<(), sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT host(scope) AS "scope!", email, attempts, window_start, locked_until
             FROM login_throttle"#
    )
    .fetch_all(&mut **tx)
    .await?;
    for r in rows {
        let ip: IpAddr = r
            .scope
            .parse()
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
        table.load(
            ip,
            r.email,
            entry_from_row(r.attempts, r.window_start, r.locked_until),
        );
    }
    Ok(())
}

async fn write(
    tx: &mut Transaction<'static, Postgres>,
    changes: Changes,
) -> Result<(), sqlx::Error> {
    if !changes.removed.is_empty() {
        let (scopes, emails): (Vec<String>, Vec<String>) = changes
            .removed
            .into_iter()
            .map(|(ip, email)| (ip.to_string(), email))
            .unzip();
        sqlx::query!(
            "DELETE FROM login_throttle t
              USING unnest($1::text[], $2::text[]) AS gone(scope, email)
              WHERE t.scope = gone.scope::inet AND t.email = gone.email",
            &scopes,
            &emails
        )
        .execute(&mut **tx)
        .await?;
    }
    for (ip, email, entry) in changes.written {
        sqlx::query!(
            "INSERT INTO login_throttle (scope, email, attempts, window_start, locked_until)
             VALUES ($1::text::inet, $2, $3, $4, $5)
             ON CONFLICT (scope, email) DO UPDATE
                SET attempts = EXCLUDED.attempts,
                    window_start = EXCLUDED.window_start,
                    locked_until = EXCLUDED.locked_until",
            ip.to_string(),
            email,
            i32::try_from(entry.attempts).unwrap_or(i32::MAX),
            DateTime::<Utc>::from(entry.window_start),
            entry.locked_until.map(DateTime::<Utc>::from),
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

impl LoginThrottle {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// Checks the lock **and** counts this attempt, in one transaction:
    /// see [`Table::admit`] for the decision and the module doc for the
    /// locks. An error leaves nothing counted and nothing admitted; the
    /// caller refuses the attempt.
    pub async fn admit(&self, key: &(IpAddr, String)) -> Result<Decision, sqlx::Error> {
        let (ip, email) = (key.0, key.1.as_str());
        let mut tx = crate::db::begin(&self.db).await?;
        lock_address(&mut tx, ip).await?;

        let known = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM login_throttle WHERE scope = $1::text::inet AND email = $2
               ) AS "known!""#,
            ip.to_string(),
            email
        )
        .fetch_one(&mut *tx)
        .await?;
        if !known {
            lock_table(&mut tx).await?;
        }
        // Read once every lock is held: an admission decided before ours
        // never carries a later instant than ours.
        let now: SystemTime = sqlx::query_scalar!(r#"SELECT clock_timestamp() AS "now!""#)
            .fetch_one(&mut *tx)
            .await?
            .into();

        let mut table = Table::default();
        if known {
            load_address(&mut tx, ip, &mut table).await?;
        } else {
            sqlx::query!(
                "DELETE FROM login_throttle WHERE window_start <= $1",
                DateTime::<Utc>::from(purge_cutoff(now))
            )
            .execute(&mut *tx)
            .await?;
            let total = sqlx::query_scalar!(r#"SELECT count(*) AS "total!" FROM login_throttle"#)
                .fetch_one(&mut *tx)
                .await?;
            let total = usize::try_from(total).unwrap_or(usize::MAX);
            if total >= MAX_TRACKED {
                load_all(&mut tx, &mut table).await?;
            } else {
                load_address(&mut tx, ip, &mut table).await?;
                table.total = total;
            }
        }

        let before = table.clone();
        let decision = table.admit(ip, email, now);
        write(&mut tx, Changes::between(&before, &table)).await?;
        tx.commit().await?;
        Ok(decision)
    }

    /// Clears the pair: a successful login means the address is not the
    /// one the throttle is there for, and the attempts it made — this one
    /// included — were not an attack.
    pub async fn record_success(&self, key: &(IpAddr, String)) -> Result<(), sqlx::Error> {
        let mut tx = crate::db::begin(&self.db).await?;
        lock_address(&mut tx, key.0).await?;
        sqlx::query!(
            "DELETE FROM login_throttle WHERE scope = $1::text::inet AND email = $2",
            key.0.to_string(),
            key.1
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::sync::Mutex;

    /// The decision alone, held in memory behind a mutex — what
    /// `LoginThrottle` was before #427, and what it still asks once it has
    /// loaded the rows under its locks.
    #[derive(Debug, Default)]
    struct InMemory(Mutex<Table>);

    impl InMemory {
        fn new() -> Self {
            Self::default()
        }

        fn admit(&self, k: &(IpAddr, String), now: SystemTime) -> Decision {
            self.0.lock().unwrap().admit(k.0, &k.1, now)
        }

        fn record_success(&self, k: &(IpAddr, String)) {
            self.0.lock().unwrap().remove(k.0, &k.1);
        }

        fn tracked(&self) -> usize {
            self.0.lock().unwrap().total
        }
    }

    // --- #427: what the store relies on ---

    #[test]
    fn a_pair_that_locks_at_the_end_of_its_window_is_live_just_after_the_cutoff() {
        // The latest a pair can lock is the last instant of its window; its
        // lock then runs LOCKOUT more. A purge reaching past that would
        // lift a lock early.
        let start = t0();
        let mut table = Table::default();
        let k = key(ip("192.168.1.42"), "alice@example.test");
        table.admit(k.0, &k.1, start);
        let end_of_window = start + WINDOW;
        for _ in 1..MAX_FAILURES {
            assert_eq!(table.admit(k.0, &k.1, end_of_window), Decision::Allow);
        }
        let entry = *table.get(k.0, &k.1).unwrap();
        assert!(entry.locked_at(end_of_window).is_some());

        let just_before = start + WINDOW + LOCKOUT - Duration::from_secs(1);
        assert!(entry.window_start > purge_cutoff(just_before));
        assert!(!entry.is_stale(just_before));

        let at_cutoff = start + WINDOW + LOCKOUT;
        assert!(entry.window_start <= purge_cutoff(at_cutoff));
        assert!(entry.is_stale(at_cutoff));
    }

    #[test]
    fn a_refused_attempt_changes_no_row() {
        // A locked pair asked again writes nothing: the store's writes are
        // bounded by admissions, not by attempts.
        let now = t0();
        let mut table = Table::default();
        let k = key(ip("192.168.1.42"), "alice@example.test");
        for _ in 0..MAX_FAILURES {
            table.admit(k.0, &k.1, now);
        }
        let before = table.clone();
        assert!(matches!(
            table.admit(k.0, &k.1, now),
            Decision::Locked { .. }
        ));
        assert_eq!(Changes::between(&before, &table), Changes::default());
    }

    #[test]
    fn an_admission_writes_the_pair_it_counted() {
        let now = t0();
        let mut table = Table::default();
        let k = key(ip("192.168.1.42"), "alice@example.test");
        let before = table.clone();
        table.admit(k.0, &k.1, now);
        let changes = Changes::between(&before, &table);
        assert!(changes.removed.is_empty());
        assert_eq!(
            changes.written,
            vec![(
                k.0,
                k.1.clone(),
                Entry {
                    attempts: 1,
                    window_start: now,
                    locked_until: None
                }
            )]
        );
    }

    #[test]
    fn the_pairs_an_admission_reclaims_are_removed_from_the_store() {
        // A new email from an address reclaims that address's stale pairs:
        // they have to go from the table, not only from the decision.
        let now = t0();
        let mut table = Table::default();
        let attacker = ip("203.0.113.9");
        table.admit(attacker, "old@example.test", now);
        let later = now + WINDOW + Duration::from_secs(1);
        let before = table.clone();
        table.admit(attacker, "new@example.test", later);
        let changes = Changes::between(&before, &table);
        assert_eq!(
            changes.removed,
            vec![(attacker, "old@example.test".to_owned())]
        );
        assert_eq!(changes.written.len(), 1);
        assert_eq!(changes.written[0].1, "new@example.test");
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn t0() -> SystemTime {
        SystemTime::now()
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

    fn admit_n(throttle: &InMemory, k: &(IpAddr, String), n: u32, now: SystemTime) {
        for _ in 0..n {
            assert_eq!(throttle.admit(k, now), Decision::Allow);
        }
    }

    #[test]
    fn a_pair_with_no_history_is_admitted() {
        let throttle = InMemory::new();
        let k = key(ip("192.168.1.42"), "alice@example.test");
        assert_eq!(throttle.admit(&k, t0()), Decision::Allow);
    }

    #[test]
    fn attempts_are_counted_on_admission_not_on_outcome() {
        // Nothing is ever reported back here, exactly like a burst of
        // requests still inside their argon2id: the eleventh is refused
        // all the same.
        let throttle = InMemory::new();
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
        let throttle = std::sync::Arc::new(InMemory::new());
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
        let throttle = InMemory::new();
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
        let throttle = InMemory::new();
        let mut now = t0();
        let k = key(ip("192.168.1.42"), "alice@example.test");
        for _ in 0..(MAX_FAILURES * 3) {
            assert_eq!(throttle.admit(&k, now), Decision::Allow);
            now += WINDOW + Duration::from_secs(1);
        }
    }

    #[test]
    fn a_locked_pair_does_not_lock_the_same_address_on_another_email() {
        let throttle = InMemory::new();
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
        let throttle = InMemory::new();
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
        let throttle = InMemory::new();
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
        let throttle = InMemory::new();
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
        let throttle = InMemory::new();
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
        let throttle = InMemory::new();
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
        let throttle = InMemory::new();
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
        let throttle = InMemory::new();
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
        let throttle = InMemory::new();
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
        let throttle = InMemory::new();
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
        let throttle = InMemory::new();
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
        let throttle = InMemory::new();
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

    fn fill(throttle: &InMemory, now: SystemTime) {
        for i in 0..MAX_TRACKED {
            throttle.admit(&key(bulk_ip(i), &format!("{i}@example.test")), now);
        }
        assert_eq!(throttle.tracked(), MAX_TRACKED);
    }

    #[test]
    fn stale_pairs_are_reclaimed_rather_than_accumulated() {
        let throttle = InMemory::new();
        let now = t0();
        fill(&throttle, now);
        let later = now + WINDOW + Duration::from_secs(1);
        throttle.admit(&key(fresh_ip(0), "fresh@example.test"), later);
        assert_eq!(throttle.tracked(), 1);
    }

    #[test]
    fn a_full_table_of_live_pairs_never_grows_past_its_ceiling() {
        let throttle = InMemory::new();
        let now = t0();
        fill(&throttle, now);
        for j in 0..500 {
            throttle.admit(&key(fresh_ip(j), "more@example.test"), now);
        }
        assert_eq!(throttle.tracked(), MAX_TRACKED);
    }

    #[test]
    fn a_full_table_does_not_open_the_lock_for_a_new_pair() {
        let throttle = InMemory::new();
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
        let throttle = InMemory::new();
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
        let throttle = InMemory::new();
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
