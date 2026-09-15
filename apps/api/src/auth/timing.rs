//! Cost attribution for `POST /auth/login`.
//!
//! Issue #113 §5 measured the end-to-end login of the e2e helper
//! `registerAndLogin` growing from 2,8 s to 5,5 s as `users` filled up,
//! against a 5 000 ms `toHaveURL` budget — without saying *which* part of
//! the request grew. Three candidates were named: the argon2 verification,
//! the SQL the handler issues, and everything else (deserialization,
//! cookie building, framework overhead).
//!
//! This module holds the arithmetic that splits one request's wall time
//! between those three, so the split can be read off a log line instead of
//! being re-derived by patching the handler each time.

use std::time::Duration;

/// Tracing target the attribution line is emitted on.
///
/// It is deliberately its own target rather than the module path, so one
/// `RUST_LOG=login_timing=debug` switches the measurement on — and nothing
/// else in the crate with it — on an already-built binary.
pub const TARGET: &str = "login_timing";

/// Wall-clock attribution of a single `POST /auth/login`.
///
/// `lookup` and `session` are the handler's two statements (the `users`
/// SELECT, then the `sessions` INSERT); `verify` is the argon2 check.
/// `total` is the whole handler, so whatever it holds beyond the three
/// named phases is [`LoginTiming::other`].
///
/// A login that ends early (unknown email, wrong password, unverified
/// address) leaves the phases it never reached at zero: that is a real
/// measurement of that outcome, not missing data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoginTiming {
    /// `SELECT id, password_hash, email_verified FROM users WHERE email = $1`.
    pub lookup: Duration,
    /// `crypto::verify_password` — argon2id, CPU-bound, no I/O.
    pub verify: Duration,
    /// `session::create_session` — `INSERT INTO sessions ... RETURNING id`.
    pub session: Duration,
    /// The handler body, measured around everything above. It starts once
    /// axum's extractors have already run, so request-body deserialization
    /// sits outside it, not in [`LoginTiming::other`].
    pub total: Duration,
}

impl LoginTiming {
    /// The handler's database time: both statements together.
    pub fn sql(&self) -> Duration {
        self.lookup + self.session
    }

    /// Everything `total` holds that none of the three named phases claims:
    /// request deserialization, cookie construction, pool checkout, the
    /// framework's own work.
    ///
    /// Saturates at zero rather than panicking. The phases are measured
    /// inside `total`, so a negative remainder can only come from a clock
    /// that went backwards, and a monotonic-clock artifact must not be able
    /// to take down a login.
    pub fn other(&self) -> Duration {
        self.total
            .saturating_sub(self.lookup)
            .saturating_sub(self.verify)
            .saturating_sub(self.session)
    }

    /// Emits the attribution as one structured line on [`TARGET`], at
    /// `debug`: every login pays the four `Instant::now()` calls, none pays
    /// the formatting unless the target is enabled.
    ///
    /// `outcome` separates a completed login from a rejected one, whose
    /// phases legitimately stop short.
    pub fn emit(&self, outcome: &'static str) {
        tracing::debug!(
            target: TARGET,
            outcome,
            total_us = micros(self.total),
            lookup_us = micros(self.lookup),
            verify_us = micros(self.verify),
            session_us = micros(self.session),
            sql_us = micros(self.sql()),
            other_us = micros(self.other()),
            "login timing"
        );
    }
}

/// `Duration::as_micros` is `u128`, which `tracing` cannot record: a login
/// phase never comes close to overflowing `u64` microseconds (584 000 years).
fn micros(d: Duration) -> u64 {
    d.as_micros() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn sql_sums_the_two_statements() {
        let t = LoginTiming {
            lookup: ms(3),
            verify: ms(60),
            session: ms(2),
            total: ms(70),
        };
        assert_eq!(t.sql(), ms(5));
    }

    #[test]
    fn other_is_total_minus_the_named_phases() {
        let t = LoginTiming {
            lookup: ms(3),
            verify: ms(60),
            session: ms(2),
            total: ms(70),
        };
        assert_eq!(t.other(), ms(5));
    }

    #[test]
    fn other_is_the_whole_total_when_no_phase_was_reached() {
        // Login rejected on an unknown email before any phase completes:
        // the remainder is the entire request.
        let t = LoginTiming {
            total: ms(4),
            ..Default::default()
        };
        assert_eq!(t.other(), ms(4));
        assert_eq!(t.sql(), Duration::ZERO);
    }

    #[test]
    fn other_saturates_at_zero_instead_of_underflowing() {
        // Cannot happen off a monotonic clock, but `Duration` subtraction
        // panics on underflow and an instrumentation line must never be the
        // thing that fails a login.
        let t = LoginTiming {
            lookup: ms(3),
            verify: ms(60),
            session: ms(2),
            total: ms(1),
        };
        assert_eq!(t.other(), Duration::ZERO);
    }

    #[test]
    fn a_default_timing_attributes_nothing() {
        let t = LoginTiming::default();
        assert_eq!(t.sql(), Duration::ZERO);
        assert_eq!(t.other(), Duration::ZERO);
    }
}
