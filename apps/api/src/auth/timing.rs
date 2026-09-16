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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

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
/// A login that ends early leaves the phases it never reached at zero:
/// that is a real measurement of that outcome, not missing data. Since
/// #178 that means `session` alone on any refusal — `lookup` and `verify`
/// are paid by every attempt that gets past the lock, including one on an
/// email that does not exist, which is exactly the property that closes
/// the enumeration oracle. A line with all four phases at zero is a login
/// the throttle stopped before it did anything.
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
    /// `debug`. A login pays only the `Instant::now()` calls of the phases
    /// it reaches — a request the throttle stops pays almost none of them
    /// — and none pays the formatting unless the target is enabled.
    ///
    /// `outcome` comes from [`outcome_label`]: it says which of the four
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
/// Four endings, not two. A 401 and a 500 both leave phases at zero, but
/// for opposite reasons: the refusal never needed them, the failure could
/// not finish the one it was in. Collapsing them would make the attribution
/// report a crashed `INSERT` as a wrong password and hide the phase that
/// actually broke.
///
/// Scope of each label, precisely: `"rejected"` is `AppError::Unauthorized`
/// alone, which is the only authentication refusal `login` produces.
/// `"throttled"` is `AppError::TooManyRequests`, the 429 the per-(address,
/// email) lock returns *before* any work — its phases are all zero because
/// none of them ran, which is what distinguishes it from a refusal that
/// paid a full argon2id. `"error"` is every other `AppError` — so it covers
/// the 500s `login` can reach today (`Sqlx` from either statement,
/// `Internal` from the hashing) but is not limited to them: the signature
/// is generic, and any future error variant on this path lands in
/// `"error"` rather than being mislabelled a refusal. The variants that
/// are not reachable from `login` (`NotFound`, `Conflict`, …) would also
/// read as `"error"`; none of them is a 500, so read the label as "not a
/// refusal and not a lock", not as a status code.
///
/// **What this label deliberately does not say is *why* a login was
/// refused** (#178 bis). Whether the email was unknown, the account was
/// Google-only, the password was wrong or the address was unverified is
/// counted in [`BranchCounters`] and published as an aggregate; a
/// per-request line naming the branch would not close the enumeration
/// oracle, it would move it to whoever can read the journal.
pub fn outcome_label<T>(result: &Result<T, AppError>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(AppError::Unauthorized) => "rejected",
        Err(AppError::TooManyRequests) => "throttled",
        Err(_) => "error",
    }
}

/// Which ending a login took, in enough detail to watch the #178 fix hold.
///
/// The three regimes the issue is about — unknown email, Google-only
/// account, known account with a hash — are separate variants here and
/// nowhere else. They must stay indistinguishable *in the response and in
/// its timing*; counting them in aggregate is how an operator checks that
/// they still are (the three refusal counters should all move, and the
/// `verify_us` of the per-request lines should not separate into two
/// populations).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginBranch {
    /// The login completed.
    Ok,
    /// No `users` row for this email.
    UnknownEmail,
    /// The row exists with `password_hash = NULL` — a Google-only account.
    NoPassword,
    /// The row exists, the hash is there, the password did not match.
    WrongPassword,
    /// Correct password on an address that has never been verified.
    Unverified,
    /// Refused by the per-(address, email) lock, before any work.
    Throttled,
    /// A statement or the hashing failed.
    Error,
}

impl LoginBranch {
    const ALL: [LoginBranch; 7] = [
        LoginBranch::Ok,
        LoginBranch::UnknownEmail,
        LoginBranch::NoPassword,
        LoginBranch::WrongPassword,
        LoginBranch::Unverified,
        LoginBranch::Throttled,
        LoginBranch::Error,
    ];

    /// Field name this branch is counted under on the aggregate line.
    pub fn label(self) -> &'static str {
        match self {
            LoginBranch::Ok => "ok",
            LoginBranch::UnknownEmail => "unknown_email",
            LoginBranch::NoPassword => "no_password",
            LoginBranch::WrongPassword => "wrong_password",
            LoginBranch::Unverified => "unverified",
            LoginBranch::Throttled => "throttled",
            LoginBranch::Error => "error",
        }
    }

    fn index(self) -> usize {
        match self {
            LoginBranch::Ok => 0,
            LoginBranch::UnknownEmail => 1,
            LoginBranch::NoPassword => 2,
            LoginBranch::WrongPassword => 3,
            LoginBranch::Unverified => 4,
            LoginBranch::Throttled => 5,
            LoginBranch::Error => 6,
        }
    }
}

/// How often the aggregate line is emitted, at most. The counters are
/// cumulative, so a longer interval loses resolution, never totals.
pub const AGGREGATE_INTERVAL: Duration = Duration::from_secs(60);

/// A reading of [`BranchCounters`]: how many logins have taken each
/// ending since this process started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BranchTotals([u64; 7]);

impl BranchTotals {
    pub fn get(&self, branch: LoginBranch) -> u64 {
        self.0[branch.index()]
    }

    /// Emits the aggregate on [`TARGET`], at `debug`, as one line.
    ///
    /// **Seven counts and nothing else** — no email, no address, no
    /// timestamp of an individual attempt. That is the whole compromise of
    /// #178 bis: the split is visible to whoever watches the service, and
    /// invisible per request, so reading the journal tells nobody which
    /// addresses have an account here.
    pub fn emit(&self) {
        tracing::debug!(
            target: TARGET,
            ok = self.get(LoginBranch::Ok),
            unknown_email = self.get(LoginBranch::UnknownEmail),
            no_password = self.get(LoginBranch::NoPassword),
            wrong_password = self.get(LoginBranch::WrongPassword),
            unverified = self.get(LoginBranch::Unverified),
            throttled = self.get(LoginBranch::Throttled),
            error = self.get(LoginBranch::Error),
            "login branches"
        );
    }
}

/// Cumulative per-branch counters for `POST /auth/login`.
///
/// Held in `AppState` behind an `Arc`, shared by every request.
#[derive(Debug)]
pub struct BranchCounters {
    counts: [AtomicU64; 7],
    schedule: Mutex<Schedule>,
}

impl BranchCounters {
    pub fn new(now: Instant) -> Self {
        Self {
            counts: Default::default(),
            schedule: Mutex::new(Schedule {
                next_emit: now + AGGREGATE_INTERVAL,
                published_logins: 0,
            }),
        }
    }

    pub fn record(&self, branch: LoginBranch) {
        self.counts[branch.index()].fetch_add(1, Ordering::Relaxed);
    }

    pub fn totals(&self) -> BranchTotals {
        let mut totals = BranchTotals::default();
        for branch in LoginBranch::ALL {
            totals.0[branch.index()] = self.counts[branch.index()].load(Ordering::Relaxed);
        }
        totals
    }

    /// The totals to publish now, if a line is due: at most once per
    /// [`AGGREGATE_INTERVAL`], only once at least [`MIN_BATCH`] logins have
    /// happened since the previous line, and only for the caller that got
    /// there first — the deadline and the watermark move under the same
    /// lock, so concurrent logins produce one line between them.
    pub fn take_due(&self, now: Instant) -> Option<BranchTotals> {
        let mut schedule = self.schedule.lock().unwrap_or_else(|e| e.into_inner());
        if now < schedule.next_emit {
            return None;
        }
        let totals = self.totals();
        let logins: u64 = totals.0.iter().sum();
        if logins < schedule.published_logins + MIN_BATCH {
            // Deadline kept: the line goes out as soon as the batch fills.
            return None;
        }
        schedule.next_emit = now + AGGREGATE_INTERVAL;
        schedule.published_logins = logins;
        Some(totals)
    }
}

/// Fewest new logins a published line must cover.
///
/// The counts are cumulative, so the difference between two lines is the
/// branches taken by the logins in between. With one login in between, that
/// difference names its branch — "unknown email" or "Google-only" — which is
/// the per-request fact #178 bis keeps out of the journal. Requiring a batch
/// means a passive reader of the journal can only ever see branches mixed
/// over at least this many logins. It does not stop someone who *also* sends
/// the other logins of the batch with emails they know to be unknown: they
/// can still isolate one foreign login. That residual is stated in the PR;
/// the aggregate is `debug`-level and off unless an operator enables it.
pub const MIN_BATCH: u64 = 20;

#[derive(Debug)]
struct Schedule {
    next_emit: Instant,
    published_logins: u64,
}

impl Default for BranchCounters {
    fn default() -> Self {
        Self::new(Instant::now())
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
    fn a_login_stopped_by_the_lock_is_labelled_throttled() {
        // Its phases are all zero because none of them ran — the opposite
        // of a refusal, which now pays a full argon2id before answering.
        let locked: Result<(), AppError> = Err(AppError::TooManyRequests);
        assert_eq!(outcome_label(&locked), "throttled");
    }

    #[test]
    fn the_per_request_line_never_names_which_branch_refused() {
        // #178 bis: the branch split is an aggregate, never a per-request
        // line. A journal line saying "this address has an account" does
        // not close the enumeration oracle, it hands it to whoever reads
        // the journal.
        let refused: Result<(), AppError> = Err(AppError::Unauthorized);
        let lines = capture(|| {
            LoginTiming {
                lookup: ms(1),
                verify: ms(250),
                total: ms(252),
                ..Default::default()
            }
            .emit(outcome_label(&refused))
        });

        assert_eq!(lines[0].outcome, "rejected");
        for branch in [
            LoginBranch::UnknownEmail,
            LoginBranch::NoPassword,
            LoginBranch::WrongPassword,
            LoginBranch::Unverified,
        ] {
            assert_ne!(lines[0].outcome, branch.label());
            assert_eq!(
                lines[0].field(branch.label()),
                None,
                "{} must not appear on a per-request line",
                branch.label()
            );
        }
    }

    #[test]
    fn counters_start_at_zero_and_count_only_the_branch_recorded() {
        let counters = BranchCounters::new(Instant::now());
        assert_eq!(counters.totals(), BranchTotals::default());

        counters.record(LoginBranch::UnknownEmail);
        counters.record(LoginBranch::UnknownEmail);
        counters.record(LoginBranch::NoPassword);

        let totals = counters.totals();
        assert_eq!(totals.get(LoginBranch::UnknownEmail), 2);
        assert_eq!(totals.get(LoginBranch::NoPassword), 1);
        assert_eq!(totals.get(LoginBranch::WrongPassword), 0);
        assert_eq!(totals.get(LoginBranch::Ok), 0);
    }

    #[test]
    fn every_branch_has_its_own_slot() {
        let counters = BranchCounters::new(Instant::now());
        for branch in LoginBranch::ALL {
            counters.record(branch);
        }
        let totals = counters.totals();
        for branch in LoginBranch::ALL {
            assert_eq!(totals.get(branch), 1, "{}", branch.label());
        }
    }

    #[test]
    fn branch_labels_are_all_distinct() {
        // They are field names on one line: two branches sharing a label
        // would silently merge into one count.
        let mut labels: Vec<&str> = LoginBranch::ALL.iter().map(|b| b.label()).collect();
        labels.sort_unstable();
        let before = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), before);
    }

    fn record_n(counters: &BranchCounters, branch: LoginBranch, n: u64) {
        for _ in 0..n {
            counters.record(branch);
        }
    }

    #[test]
    fn the_aggregate_is_due_once_per_interval_and_not_before() {
        let start = Instant::now();
        let counters = BranchCounters::new(start);
        record_n(&counters, LoginBranch::Ok, MIN_BATCH);

        assert!(counters.take_due(start).is_none());
        assert!(counters
            .take_due(start + AGGREGATE_INTERVAL - Duration::from_millis(1))
            .is_none());
        assert!(counters.take_due(start + AGGREGATE_INTERVAL).is_some());
        // The deadline moved: the next login in the same second does not
        // get a second line.
        record_n(&counters, LoginBranch::Ok, MIN_BATCH);
        assert!(counters.take_due(start + AGGREGATE_INTERVAL).is_none());
        assert!(counters.take_due(start + AGGREGATE_INTERVAL * 2).is_some());
    }

    #[test]
    fn a_quiet_interval_publishes_nothing_until_a_batch_has_accumulated() {
        // Cumulative counts read twice around a single login say which
        // branch that login took. At low traffic that is one request
        // isolated, so a line is only published once it covers at least
        // `MIN_BATCH` logins the previous line did not.
        let start = Instant::now();
        let counters = BranchCounters::new(start);
        let later = start + AGGREGATE_INTERVAL * 3;

        counters.record(LoginBranch::UnknownEmail);
        assert!(counters.take_due(later).is_none());

        record_n(&counters, LoginBranch::Ok, MIN_BATCH - 1);
        let published = counters.take_due(later).expect("a full batch");
        assert_eq!(published.get(LoginBranch::UnknownEmail), 1);
        assert_eq!(published.get(LoginBranch::Ok), MIN_BATCH - 1);
    }

    #[test]
    fn consecutive_lines_are_always_a_batch_apart() {
        let start = Instant::now();
        let counters = BranchCounters::new(start);
        record_n(&counters, LoginBranch::Ok, MIN_BATCH);
        assert!(counters.take_due(start + AGGREGATE_INTERVAL).is_some());

        counters.record(LoginBranch::NoPassword);
        assert!(counters.take_due(start + AGGREGATE_INTERVAL * 5).is_none());
        record_n(&counters, LoginBranch::Ok, MIN_BATCH - 2);
        assert!(counters.take_due(start + AGGREGATE_INTERVAL * 5).is_none());
        counters.record(LoginBranch::Ok);
        assert!(counters.take_due(start + AGGREGATE_INTERVAL * 5).is_some());
    }

    #[test]
    fn the_aggregate_line_carries_one_count_per_branch_and_nothing_identifying() {
        let counters = BranchCounters::new(Instant::now());
        counters.record(LoginBranch::Ok);
        counters.record(LoginBranch::UnknownEmail);
        counters.record(LoginBranch::Throttled);
        counters.record(LoginBranch::Throttled);

        let lines = capture(|| counters.totals().emit());
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!(line.target, TARGET);
        assert_eq!(line.field("ok"), Some(1));
        assert_eq!(line.field("unknown_email"), Some(1));
        assert_eq!(line.field("throttled"), Some(2));
        assert_eq!(line.field("wrong_password"), Some(0));

        let names: Vec<&str> = line.fields.iter().map(|(n, _)| n.as_str()).collect();
        let mut expected: Vec<&str> = LoginBranch::ALL.iter().map(|b| b.label()).collect();
        let mut got = names.clone();
        got.sort_unstable();
        expected.sort_unstable();
        assert_eq!(got, expected, "no field beyond the seven counts");
        assert_eq!(line.outcome, "", "no per-attempt string on the aggregate");
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
