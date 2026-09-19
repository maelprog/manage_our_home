//! How many uploads one process holds in memory at once (#219).
//!
//! A time bound on the body does not bound memory: every upload in flight
//! keeps up to `MAX_UPLOAD_BODY_BYTES` buffered, however quickly it
//! arrives. So an upload route takes two permits before it reads a byte of
//! body — one for its account, then one from a process-wide pool — and
//! refuses with 503 at once, without queueing, when either is gone. Both
//! are released when the permit is dropped: at the end of the request,
//! on an error, or when the client disconnects and the handler's future is
//! dropped with it.
//!
//! The per-account limit is what keeps the pool from being one account's
//! to empty: without it, a single session opening eight slow uploads would
//! answer 503 to every other upload for as long as the bodies last.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Uploads one process holds at once. At `MAX_UPLOAD_BODY_BYTES` (20 MiB
/// plus framing) that is about 160 MiB at worst, on a 4 GB host — if each
/// upload holds one copy of its body. apps/web does, and its test
/// `a_full_pool_of_uploads_at_the_cap_holds_one_copy_of_each` measures a
/// full pool at the cap (#246). apps/api is not measured.
pub const GLOBAL_UPLOADS: usize = 8;

/// Uploads one account may have in flight at once.
pub const UPLOADS_PER_ACCOUNT: usize = 2;

/// `Retry-After` on a 503 from a full gate, in seconds.
pub const RETRY_AFTER_SECS: u64 = 30;

/// Why an upload was turned away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    /// This account already has its uploads in flight.
    Account,
    /// The process-wide pool is empty.
    Process,
}

/// Counts in-flight uploads per key. Takes a slot if the key is under
/// `limit`; a key with nothing in flight has no entry, so the map holds
/// only the accounts uploading right now.
fn take_slot<K: Hash + Eq + Clone>(counts: &mut HashMap<K, usize>, key: &K, limit: usize) -> bool {
    let held = counts.get(key).copied().unwrap_or(0);
    if held >= limit {
        return false;
    }
    counts.insert(key.clone(), held + 1);
    true
}

fn release_slot<K: Hash + Eq>(counts: &mut HashMap<K, usize>, key: &K) {
    match counts.get_mut(key) {
        Some(held) if *held > 1 => *held -= 1,
        Some(_) => {
            counts.remove(key);
        }
        None => {}
    }
}

/// Both limits, shared by every request of one process.
pub struct UploadGate<K> {
    process: Arc<Semaphore>,
    process_limit: usize,
    per_key_limit: usize,
    per_key: Mutex<HashMap<K, usize>>,
}

/// Held for as long as the upload is in flight; dropping it gives both
/// slots back.
pub struct UploadPermit<K: Hash + Eq> {
    gate: Arc<UploadGate<K>>,
    key: K,
    _process: OwnedSemaphorePermit,
}

impl<K: Hash + Eq> Drop for UploadPermit<K> {
    fn drop(&mut self) {
        let mut counts = self.gate.per_key.lock().unwrap_or_else(|p| p.into_inner());
        release_slot(&mut counts, &self.key);
    }
}

impl<K: Hash + Eq + Clone> UploadGate<K> {
    pub fn new(process_limit: usize, per_key_limit: usize) -> Arc<Self> {
        Arc::new(Self {
            process: Arc::new(Semaphore::new(process_limit)),
            process_limit,
            per_key_limit,
            per_key: Mutex::new(HashMap::new()),
        })
    }

    /// `GLOBAL_UPLOADS` and `UPLOADS_PER_ACCOUNT`.
    pub fn production() -> Arc<Self> {
        Self::new(GLOBAL_UPLOADS, UPLOADS_PER_ACCOUNT)
    }

    /// The account's slot first, then the process's: an account at its
    /// limit is refused without touching the shared pool.
    pub fn try_acquire(self: &Arc<Self>, key: K) -> Result<UploadPermit<K>, Busy> {
        {
            let mut counts = self.per_key.lock().unwrap_or_else(|p| p.into_inner());
            if !take_slot(&mut counts, &key, self.per_key_limit) {
                return Err(Busy::Account);
            }
        }
        match self.process.clone().try_acquire_owned() {
            Ok(permit) => Ok(UploadPermit {
                gate: self.clone(),
                key,
                _process: permit,
            }),
            Err(_) => {
                let mut counts = self.per_key.lock().unwrap_or_else(|p| p.into_inner());
                release_slot(&mut counts, &key);
                Err(Busy::Process)
            }
        }
    }

    /// Uploads in flight in this process right now.
    pub fn in_flight(&self) -> usize {
        self.process_limit - self.process.available_permits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_taken_up_to_the_limit_and_no_further() {
        let mut counts = HashMap::new();
        assert!(take_slot(&mut counts, &"a", 2));
        assert!(take_slot(&mut counts, &"a", 2));
        assert!(!take_slot(&mut counts, &"a", 2));
        assert_eq!(counts.get("a"), Some(&2));
    }

    #[test]
    fn one_key_at_its_limit_leaves_the_others_free() {
        let mut counts = HashMap::new();
        assert!(take_slot(&mut counts, &"a", 1));
        assert!(!take_slot(&mut counts, &"a", 1));
        assert!(take_slot(&mut counts, &"b", 1));
    }

    #[test]
    fn a_released_slot_can_be_taken_again() {
        let mut counts = HashMap::new();
        assert!(take_slot(&mut counts, &"a", 1));
        release_slot(&mut counts, &"a");
        assert!(take_slot(&mut counts, &"a", 1));
    }

    #[test]
    fn a_key_with_nothing_in_flight_leaves_no_entry() {
        let mut counts = HashMap::new();
        take_slot(&mut counts, &"a", 2);
        take_slot(&mut counts, &"a", 2);
        release_slot(&mut counts, &"a");
        assert_eq!(counts.get("a"), Some(&1));
        release_slot(&mut counts, &"a");
        assert!(counts.is_empty());
    }

    #[test]
    fn releasing_an_unknown_key_changes_nothing() {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        release_slot(&mut counts, &"a");
        assert!(counts.is_empty());
    }

    #[test]
    fn the_gate_refuses_an_account_past_its_limit_but_not_the_next_account() {
        let gate = UploadGate::new(8, 2);
        let _a1 = gate.try_acquire("a").unwrap();
        let _a2 = gate.try_acquire("a").unwrap();
        assert_eq!(gate.try_acquire("a").err(), Some(Busy::Account));
        assert!(gate.try_acquire("b").is_ok());
    }

    #[test]
    fn the_gate_refuses_everyone_once_the_process_pool_is_empty() {
        let gate = UploadGate::new(2, 2);
        let _a = gate.try_acquire("a").unwrap();
        let _b = gate.try_acquire("b").unwrap();
        assert_eq!(gate.try_acquire("c").err(), Some(Busy::Process));
        assert_eq!(gate.in_flight(), 2);
    }

    #[test]
    fn a_refusal_from_the_pool_gives_the_account_slot_back() {
        let gate = UploadGate::new(1, 1);
        let held = gate.try_acquire("a").unwrap();
        // "b" is under its own limit but the pool is empty…
        assert_eq!(gate.try_acquire("b").err(), Some(Busy::Process));
        drop(held);
        // …and that refusal did not leave "b" holding its only slot.
        assert!(gate.try_acquire("b").is_ok());
    }

    #[test]
    fn dropping_a_permit_frees_both_slots() {
        let gate = UploadGate::new(1, 1);
        let held = gate.try_acquire("a").unwrap();
        assert_eq!(gate.in_flight(), 1);
        drop(held);
        assert_eq!(gate.in_flight(), 0);
        assert!(gate.try_acquire("a").is_ok());
    }

    #[test]
    fn production_limits_are_the_ones_settled_on_the_issue() {
        assert_eq!(GLOBAL_UPLOADS, 8);
        assert_eq!(UPLOADS_PER_ACCOUNT, 2);
    }
}
