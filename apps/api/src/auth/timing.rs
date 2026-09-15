//! Cost attribution for `POST /auth/login`.
//!
//! Issue #113 §5 measured the end-to-end login of the e2e helper
//! `registerAndLogin` growing from 2,8 s to 5,5 s as `users` filled up,
//! against a 5 000 ms `toHaveURL` budget — without saying *which* part of
//! the request grew. The three candidates it names are the password
//! hashing, an unindexed query, and the seeding.
//!
//! This module holds the arithmetic that splits one request's wall time
//! between the argon2 verification, the SQL the handler issues, and the
//! remainder, so the split can be read off a log line instead of being
//! re-derived by patching the handler each time.
//!
//! That covers two of the three candidates and not the seeding: seeding
//! happens before the request, outside the handler, and nothing this module
//! measures can see it. What it can do is say whether the seeded rows make
//! the phases it *does* measure any slower.

use std::time::Duration;

use crate::error::AppError;

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
    /// `SELECT id, password_hash, email_verified FROM users WHERE email = $1`
    /// — **and the pool checkout it waits for first.** Both statements are
    /// issued against the pool, not a held connection, so acquiring one is
    /// inside the phase that asks for it, never in
    /// [`LoginTiming::other`]. Kill the pool's connections and this is the
    /// phase that grows — by orders of magnitude — while `other` does not
    /// move at all.
    ///
    /// So a `lookup` of a few hundred microseconds is not a few hundred
    /// microseconds of query: the `SELECT` itself is an index scan costing a
    /// few tens of microseconds, and the rest is the checkout.
    pub lookup: Duration,
    /// `crypto::verify_password` — argon2id, CPU-bound, no I/O.
    pub verify: Duration,
    /// `session::create_session` — `INSERT INTO sessions ... RETURNING id`,
    /// its commit, and its own pool checkout (same reasoning as
    /// [`LoginTiming::lookup`]).
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
    /// cookie construction, the branching between the phases, the handler's
    /// own bookkeeping. It is the *thinnest* of the four, and measured as
    /// tens of microseconds — nothing that blocks lives here.
    ///
    /// Two things that sound like they belong here do not:
    ///
    /// - **Pool checkout.** Waiting for a connection happens inside the
    ///   phase that needs one, `lookup` or `session` — see
    ///   [`LoginTiming::lookup`]. Starve the pool and `lookup` explodes
    ///   while `other` stays flat.
    /// - **Request-body deserialization.** Axum's extractor runs it before
    ///   `total` starts, so it is outside `total` altogether (see
    ///   [`LoginTiming::total`]).
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
    /// `debug`. A login pays only the `Instant::now()` calls it reaches —
    /// four for a completed one, two for a refusal on an unknown email —
    /// and none pays the formatting unless the target is enabled.
    ///
    /// `outcome` comes from [`outcome_label`]: it says which of the three
    /// endings produced these phases, so a zero can be read as "never
    /// reached" and not as "instantaneous".
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

/// The `outcome` label for a login, from what the handler returned.
///
/// Three endings, not two. A 401 and a 500 both leave phases at zero, but
/// for opposite reasons: the refusal never needed them, the failure could
/// not finish the one it was in. Collapsing them would make the attribution
/// report a crashed `INSERT` as a wrong password and hide the phase that
/// actually broke.
///
/// Scope of each label, precisely: `"rejected"` is `AppError::Unauthorized`
/// alone, which is the only rejection `login` produces. `"error"` is every
/// other `AppError` — so it covers the 500s `login` can reach today
/// (`Sqlx` from either statement, `Internal` from the hashing) but is not
/// limited to them: the signature is generic, and any future error variant
/// on this path lands in `"error"` rather than being mislabelled a refusal.
/// The variants that are not reachable from `login` (`NotFound`,
/// `Conflict`, …) would also read as `"error"`; none of them is a 500, so
/// read the label as "not a refusal", not as a status code.
pub fn outcome_label<T>(result: &Result<T, AppError>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(AppError::Unauthorized) => "rejected",
        Err(_) => "error",
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

    #[test]
    fn a_completed_login_is_labelled_ok() {
        assert_eq!(outcome_label::<()>(&Ok(())), "ok");
    }

    #[test]
    fn an_authentication_refusal_is_labelled_rejected() {
        let refused: Result<(), AppError> = Err(AppError::Unauthorized);
        assert_eq!(outcome_label(&refused), "rejected");
    }

    #[test]
    fn an_internal_failure_is_not_labelled_as_a_refusal() {
        // A crashed statement and a wrong password both leave phases at
        // zero. If they shared a label the attribution would report the
        // failure as a refusal and hide the phase that broke.
        let sql: Result<(), AppError> = Err(AppError::Sqlx(sqlx::Error::PoolClosed));
        assert_eq!(outcome_label(&sql), "error");

        let hashing: Result<(), AppError> = Err(AppError::Internal(anyhow::anyhow!("argon2")));
        assert_eq!(outcome_label(&hashing), "error");
    }

    #[test]
    fn the_target_is_the_one_rust_log_is_documented_with() {
        // `RUST_LOG=login_timing=debug` is what apps/api/README.md tells an
        // operator to set. Renaming the target silently would leave that
        // instruction pointing at nothing.
        assert_eq!(TARGET, "login_timing");
    }

    #[test]
    fn emit_writes_one_line_on_that_target_with_every_phase_in_microseconds() {
        let t = LoginTiming {
            lookup: ms(3),
            verify: ms(60),
            session: ms(2),
            total: ms(70),
        };
        let lines = capture(|| t.emit("ok"));

        assert_eq!(lines.len(), 1, "exactly one line per request");
        let line = &lines[0];
        assert_eq!(line.target, TARGET);
        assert_eq!(line.outcome, "ok");
        assert_eq!(line.field("total_us"), Some(70_000));
        assert_eq!(line.field("lookup_us"), Some(3_000));
        assert_eq!(line.field("verify_us"), Some(60_000));
        assert_eq!(line.field("session_us"), Some(2_000));
        assert_eq!(line.field("sql_us"), Some(5_000));
        assert_eq!(line.field("other_us"), Some(5_000));
    }

    #[test]
    fn emit_reports_the_phases_a_refused_login_never_reached_as_zero() {
        let t = LoginTiming {
            lookup: Duration::from_micros(256),
            total: Duration::from_micros(269),
            ..Default::default()
        };
        let lines = capture(|| t.emit("rejected"));

        assert_eq!(lines[0].outcome, "rejected");
        assert_eq!(lines[0].field("lookup_us"), Some(256));
        assert_eq!(lines[0].field("verify_us"), Some(0));
        assert_eq!(lines[0].field("session_us"), Some(0));
    }

    // --- a subscriber that keeps the events, so the log line is asserted on
    // as data rather than eyeballed in a terminal ---

    #[derive(Debug, Default)]
    struct Line {
        target: String,
        outcome: String,
        fields: Vec<(String, u64)>,
    }

    impl Line {
        fn field(&self, name: &str) -> Option<u64> {
            self.fields.iter().find(|(n, _)| n == name).map(|(_, v)| *v)
        }
    }

    impl tracing::field::Visit for Line {
        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            self.fields.push((field.name().to_string(), value));
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "outcome" {
                self.outcome = value.to_string();
            }
        }

        fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
    }

    #[derive(Clone, Default)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<Line>>>);

    impl tracing::Subscriber for Capture {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut line = Line {
                target: event.metadata().target().to_string(),
                ..Default::default()
            };
            event.record(&mut line);
            self.0.lock().unwrap().push(line);
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    fn capture(f: impl FnOnce()) -> Vec<Line> {
        let sub = Capture::default();
        let sink = sub.0.clone();
        tracing::subscriber::with_default(sub, f);
        std::sync::Arc::try_unwrap(sink)
            .unwrap()
            .into_inner()
            .unwrap()
    }
}
