//! The response-time floor of the routes that mail or not depending on an
//! account the caller must not learn about (#571).
//!
//! `POST /auth/register` (once its fields are valid), `POST
//! /auth/password/forgot` and `POST /auth/verify-email/resend` answer the
//! same whatever account holds the address (#460, AC #4). What they do
//! differs: one sends an email from within the request, another sends
//! nothing. Before this floor the branches that send nothing answered
//! faster, and a stopwatch told whether the address had an account.
//!
//! Each of them now holds its answer until [`ResponseFloor::floor`] has
//! passed since it started, whichever branch it took. For that to hold, the
//! slowest branch must fit under the floor even when something it waits on
//! is slow: the email gets [`SMTP_TIMEOUT`] in all, after which it is given
//! up and logged, and the "account exists" notice cap
//! (`auth::account_exists_notice`) gets [`NOTICE_ADMISSION_TIMEOUT`] to
//! admit a notice, after which none is sent. What is left of the floor,
//! [`WORK_ALLOWANCE`], covers the argon2id hash and the statements.
//!
//! The arbitration of 2026-10-22: "cap respond time so it takes at least 2
//! times the longest expected response time". The longest expected branch
//! is `register` on a new address: the hash, a lookup, a transaction of two
//! inserts, then one email through the relay. The figures behind
//! [`DEFAULT_FLOOR`] are in the body of the PR that added it.

use std::time::{Duration, Instant};

/// What is left to wait once `elapsed` has passed, for an answer not to
/// leave before `floor`: the rest of the floor, or nothing once it is over.
pub fn remaining(elapsed: Duration, floor: Duration) -> Duration {
    floor.saturating_sub(elapsed)
}

/// Deadline on the whole of one email sent by these routes: connection,
/// TLS, dialogue and transfer. Past it the send is dropped and logged, and
/// the request answers as if the email had left. lettre's own timeout
/// (`main.rs`, set to the same value) bounds the connection only, so it
/// cannot stand in for this one.
pub const SMTP_TIMEOUT: Duration = Duration::from_millis(1_500);

/// Deadline on the admission of one "account exists" notice. The cap's
/// pool is one connection unless the deployment says otherwise (#570): a
/// burst of registrations of one address queues the notices of every other
/// verified address behind it. Past the deadline the notice is not sent.
pub const NOTICE_ADMISSION_TIMEOUT: Duration = Duration::from_millis(250);

/// What the slowest branch spends outside its two deadlines: the argon2id
/// hash, the lookup and the transaction that creates an account.
pub const WORK_ALLOWANCE: Duration = Duration::from_millis(250);

/// The smallest floor [`FLOOR_VAR`] may set: the slowest branch with both
/// its deadlines spent in full. Below it, a slow relay or a queue on the
/// notice cap would let one branch answer later than the others.
pub const MIN_FLOOR: Duration = Duration::from_millis(
    WORK_ALLOWANCE.as_millis() as u64
        + NOTICE_ADMISSION_TIMEOUT.as_millis() as u64
        + SMTP_TIMEOUT.as_millis() as u64,
);

/// The floor when [`FLOOR_VAR`] is unset.
pub const DEFAULT_FLOOR: Duration = Duration::from_millis(2_000);

/// Environment variable setting the floor, in milliseconds, per replica.
pub const FLOOR_VAR: &str = "AUTH_RESPONSE_FLOOR_MS";

/// [`FLOOR_VAR`]'s value: blank or unset means [`DEFAULT_FLOOR`]; a whole
/// number of milliseconds from [`MIN_FLOOR`] is taken; anything else is an
/// error, and `main.rs` refuses to start rather than fall back.
pub fn floor_from_env(raw: Option<&str>) -> Result<Duration, String> {
    let raw = raw.map(str::trim).unwrap_or("");
    if raw.is_empty() {
        return Ok(DEFAULT_FLOOR);
    }
    match raw.parse::<u64>().map(Duration::from_millis) {
        Ok(floor) if floor >= MIN_FLOOR => Ok(floor),
        _ => Err(format!(
            "{FLOOR_VAR}={raw:?}: expected a whole number of milliseconds, at least {} \
             (unset means {})",
            MIN_FLOOR.as_millis(),
            DEFAULT_FLOOR.as_millis()
        )),
    }
}

/// The floor and the two deadlines that keep every branch under it, as one
/// value of `AppState`. `main.rs` builds it with [`ResponseFloor::new`]; a
/// test sets the fields it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponseFloor {
    pub floor: Duration,
    pub smtp_timeout: Duration,
    pub notice_admission_timeout: Duration,
}

impl ResponseFloor {
    /// `floor` with the production deadlines.
    pub const fn new(floor: Duration) -> Self {
        Self {
            floor,
            smtp_timeout: SMTP_TIMEOUT,
            notice_admission_timeout: NOTICE_ADMISSION_TIMEOUT,
        }
    }

    /// Waits until the floor has passed since `started`.
    pub async fn hold(&self, started: Instant) {
        tokio::time::sleep(remaining(started.elapsed(), self.floor)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: Duration = Duration::from_millis(2_000);

    #[test]
    fn waits_what_is_left_of_the_floor() {
        assert_eq!(
            remaining(Duration::from_millis(300), F),
            Duration::from_millis(1_700)
        );
        assert_eq!(remaining(Duration::ZERO, F), F);
        assert_eq!(
            remaining(Duration::from_millis(1_999), F),
            Duration::from_millis(1)
        );
    }

    #[test]
    fn waits_nothing_once_the_floor_has_passed() {
        assert_eq!(remaining(F, F), Duration::ZERO);
        assert_eq!(remaining(Duration::from_secs(60), F), Duration::ZERO);
    }

    #[test]
    fn a_zero_floor_never_waits() {
        assert_eq!(remaining(Duration::ZERO, Duration::ZERO), Duration::ZERO);
        assert_eq!(
            remaining(Duration::from_millis(5), Duration::ZERO),
            Duration::ZERO
        );
    }

    #[test]
    fn unset_or_blank_means_the_default() {
        assert_eq!(floor_from_env(None), Ok(DEFAULT_FLOOR));
        assert_eq!(floor_from_env(Some("")), Ok(DEFAULT_FLOOR));
        assert_eq!(floor_from_env(Some("  ")), Ok(DEFAULT_FLOOR));
    }

    #[test]
    fn reads_whole_milliseconds() {
        assert_eq!(
            floor_from_env(Some("2500")),
            Ok(Duration::from_millis(2_500))
        );
        assert_eq!(
            floor_from_env(Some(" 3000 ")),
            Ok(Duration::from_millis(3_000))
        );
        let min = MIN_FLOOR.as_millis().to_string();
        assert_eq!(floor_from_env(Some(&min)), Ok(MIN_FLOOR));
    }

    #[test]
    fn refuses_a_floor_below_what_the_slowest_branch_may_take() {
        let below = (MIN_FLOOR.as_millis() - 1).to_string();
        let err = floor_from_env(Some(&below)).unwrap_err();
        assert!(err.contains(FLOOR_VAR), "{err}");
        assert!(floor_from_env(Some("0")).is_err());
    }

    #[test]
    fn refuses_anything_but_a_whole_number() {
        for raw in [
            "2s",
            "2000ms",
            "-2000",
            "2000.5",
            "abc",
            "99999999999999999999999",
        ] {
            let err = floor_from_env(Some(raw)).unwrap_err();
            assert!(err.contains(FLOOR_VAR), "{raw}: {err}");
        }
    }

    #[test]
    fn the_default_is_at_least_the_minimum() {
        assert!(DEFAULT_FLOOR >= MIN_FLOOR);
    }

    #[test]
    fn the_minimum_holds_the_slowest_branch_with_both_deadlines_spent() {
        assert!(MIN_FLOOR >= WORK_ALLOWANCE + NOTICE_ADMISSION_TIMEOUT + SMTP_TIMEOUT);
    }

    #[test]
    fn new_carries_the_production_deadlines() {
        let floor = ResponseFloor::new(F);
        assert_eq!(floor.floor, F);
        assert_eq!(floor.smtp_timeout, SMTP_TIMEOUT);
        assert_eq!(floor.notice_admission_timeout, NOTICE_ADMISSION_TIMEOUT);
    }
}
