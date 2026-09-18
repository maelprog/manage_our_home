//! The three bounds on reading a request body, as pure arithmetic on
//! durations and a byte count (#219). No clock is read here: the body
//! wrapper in `crate::body` measures, this module only judges.

use std::time::Duration;

/// Bounds on how a client may send a request body.
///
/// Measured from the first time the handler asks for the body, not from
/// the arrival of the request: time a handler spends before reading (its
/// authorization query, say) is the server's, not the client's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyReadLimits {
    /// Longest silence between two chunks of body. The equivalent of
    /// nginx's `client_body_timeout`.
    pub idle: Duration,
    /// Average rate the body must keep, in bytes per second, counted from
    /// the first read, once `grace` has passed. Without it a client sending
    /// one byte every `idle - 1` seconds holds its request (and, on an
    /// upload route, its permit) for the whole of `total`.
    pub min_bytes_per_sec: u64,
    /// How long the rate is left unchecked, so a connection that starts
    /// slowly is not cut in its first second.
    pub grace: Duration,
    /// Longest time the body may take altogether, however it is paced.
    pub total: Duration,
}

impl BodyReadLimits {
    /// The values settled on #219: 30 s of silence, 1 kB/s on average after
    /// 10 s, 15 min in all. A 20 MiB attachment fits in the total from
    /// about 190 kbit/s sustained. Caddy's `read_body` sits one minute
    /// above `total` (infra/Caddyfile), so it is the application that
    /// answers the 408 and Caddy is only a backstop.
    pub const PRODUCTION: Self = Self {
        idle: Duration::from_secs(30),
        min_bytes_per_sec: 1_000,
        grace: Duration::from_secs(10),
        total: Duration::from_secs(15 * 60),
    };
}

/// Request body limit of the two attachment upload routes, apps/api's
/// `POST /groups/:id/events/:event_id/attachments` and apps/web's
/// `POST /agenda/:id/attachments`: the 20 MiB attachment cap
/// (`MAX_ATTACHMENT_SIZE_BYTES` in apps/api's `storage` and in
/// apps/shared) plus 64 KiB of multipart framing, the same 21 037 056
/// bytes as Caddy's `max_size` on those routes (infra/Caddyfile).
///
/// Both applications set it as the route's `DefaultBodyLimit`, which is
/// otherwise 2 MiB: apps/web inherited that default until #243, so a 3 MB
/// file died in its multipart read as `upload_failed` though apps/api
/// would have taken it. One constant for both, so they cannot drift apart.
pub const MAX_UPLOAD_BODY_BYTES: usize = 20 * 1024 * 1024 + 64 * 1024;

/// Which bound a body broke. All three answer 408; the reason is for logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breach {
    Idle,
    TooSlow,
    Total,
}

/// What `BodyReadLimits::judge` concludes about a body still being read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Within bounds. Nothing can change that before `recheck_at`
    /// (measured from the first read) unless more bytes arrive.
    Within {
        recheck_at: Duration,
    },
    Breached(Breach),
}

impl BodyReadLimits {
    /// Judges a body `elapsed` after its first read, `idle_for` after the
    /// last chunk arrived (or after the first read, if none has), with
    /// `received` bytes so far.
    pub fn judge(&self, elapsed: Duration, idle_for: Duration, received: u64) -> Verdict {
        if elapsed >= self.total {
            return Verdict::Breached(Breach::Total);
        }
        if idle_for >= self.idle {
            return Verdict::Breached(Breach::Idle);
        }
        // The average falls short once received / elapsed < rate, that is
        // once elapsed > received / rate. Integer nanoseconds throughout, so
        // "exactly the rate" is exact rather than a float comparison.
        let rate = u128::from(self.min_bytes_per_sec.max(1));
        let covered_ns = u128::from(received) * 1_000_000_000 / rate;
        if elapsed >= self.grace && elapsed.as_nanos() > covered_ns {
            return Verdict::Breached(Breach::TooSlow);
        }
        // The first nanosecond past what the bytes cover, never before
        // grace ends.
        let rate_deadline =
            Duration::from_nanos(u64::try_from(covered_ns + 1).unwrap_or(u64::MAX)).max(self.grace);
        let idle_deadline = elapsed - idle_for.min(elapsed) + self.idle;
        Verdict::Within {
            recheck_at: self.total.min(idle_deadline).min(rate_deadline),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    fn limits() -> BodyReadLimits {
        BodyReadLimits::PRODUCTION
    }

    #[test]
    fn production_values_are_the_ones_settled_on_the_issue() {
        let l = limits();
        assert_eq!(l.idle, S(30));
        assert_eq!(l.min_bytes_per_sec, 1_000);
        assert_eq!(l.grace, S(10));
        assert_eq!(l.total, S(900));
    }

    #[test]
    fn the_upload_body_limit_is_caddys_max_size() {
        // infra/Caddyfile `request_body { max_size 21037056 }`: 20 MiB of
        // attachment plus 64 KiB of multipart framing.
        assert_eq!(MAX_UPLOAD_BODY_BYTES, 21_037_056);
        assert_eq!(MAX_UPLOAD_BODY_BYTES, 20 * 1024 * 1024 + 64 * 1024);
    }

    #[test]
    fn a_fresh_body_is_within_bounds_and_rechecked_at_the_end_of_grace() {
        assert_eq!(
            limits().judge(Duration::ZERO, Duration::ZERO, 0),
            Verdict::Within { recheck_at: S(10) }
        );
    }

    #[test]
    fn nothing_during_grace_is_fine_even_with_no_byte_at_all() {
        assert!(matches!(
            limits().judge(S(9), S(9), 0),
            Verdict::Within { .. }
        ));
    }

    #[test]
    fn the_rate_is_checked_from_the_end_of_grace_inclusive() {
        // 1 kB/s for 10 s is exactly 10 000 bytes: exactly the rate passes…
        assert!(matches!(
            limits().judge(S(10), Duration::ZERO, 10_000),
            Verdict::Within { .. }
        ));
        // …and one byte short of it does not.
        assert_eq!(
            limits().judge(S(10), Duration::ZERO, 9_999),
            Verdict::Breached(Breach::TooSlow)
        );
    }

    #[test]
    fn the_rate_is_an_average_since_the_first_read() {
        // 120 kB in two minutes is 1 kB/s on average, however it was spread.
        assert!(matches!(
            limits().judge(S(120), S(5), 120_000),
            Verdict::Within { .. }
        ));
        assert_eq!(
            limits().judge(S(120), S(5), 119_999),
            Verdict::Breached(Breach::TooSlow)
        );
    }

    #[test]
    fn a_drip_just_under_the_idle_bound_is_cut_by_the_rate() {
        // One byte every 29 s never trips `idle`; the rate catches it as
        // soon as grace ends.
        assert_eq!(
            limits().judge(S(10), S(10), 1),
            Verdict::Breached(Breach::TooSlow)
        );
    }

    #[test]
    fn silence_reaching_idle_is_a_breach_even_with_the_rate_well_met() {
        // 1 MB up front covers the rate for 1 000 s; 30 s of silence does not.
        assert_eq!(
            limits().judge(S(31), S(30), 1_000_000),
            Verdict::Breached(Breach::Idle)
        );
        assert!(matches!(
            limits().judge(S(30), Duration::from_millis(29_999), 1_000_000),
            Verdict::Within { .. }
        ));
    }

    #[test]
    fn total_is_a_breach_however_fast_the_body_came() {
        assert_eq!(
            limits().judge(S(900), Duration::ZERO, u64::MAX / 2),
            Verdict::Breached(Breach::Total)
        );
        assert!(matches!(
            limits().judge(S(899), Duration::ZERO, u64::MAX / 2),
            Verdict::Within { .. }
        ));
    }

    #[test]
    fn recheck_is_the_idle_deadline_when_it_comes_first() {
        // Last chunk 5 s ago at t = 20 s: silence reaches 30 s at t = 45 s.
        // The rate holds until t = 1 000 s, the total until 900 s.
        assert_eq!(
            limits().judge(S(20), S(5), 1_000_000),
            Verdict::Within { recheck_at: S(45) }
        );
    }

    #[test]
    fn recheck_is_just_past_the_instant_the_average_falls_under_the_rate() {
        // 15 000 bytes meet 1 kB/s up to t = 15 s exactly; the first instant
        // they fall short is one nanosecond later.
        assert_eq!(
            limits().judge(S(12), Duration::ZERO, 15_000),
            Verdict::Within {
                recheck_at: S(15) + Duration::from_nanos(1)
            }
        );
    }

    #[test]
    fn recheck_is_the_total_when_nothing_else_comes_first() {
        assert_eq!(
            limits().judge(S(880), Duration::ZERO, 10_000_000),
            Verdict::Within { recheck_at: S(900) }
        );
    }

    #[test]
    fn rechecking_at_the_announced_instant_with_no_new_byte_is_a_breach() {
        // Whatever deadline `judge` announces, reaching it with nothing new
        // must not come back `Within` again — or the body wrapper would
        // re-arm the same timer forever.
        let l = limits();
        for (elapsed, idle_for, received) in [
            (0, 0, 0),
            (12, 0, 15_000),
            (20, 5, 1_000_000),
            (880, 0, 10_000_000),
        ] {
            let Verdict::Within { recheck_at } = l.judge(S(elapsed), S(idle_for), received) else {
                panic!("expected Within at t={elapsed}");
            };
            let later = recheck_at - S(elapsed);
            assert!(
                matches!(
                    l.judge(recheck_at, S(idle_for) + later, received),
                    Verdict::Breached(_)
                ),
                "t={elapsed}: recheck at {recheck_at:?} is not a breach"
            );
        }
    }
}
