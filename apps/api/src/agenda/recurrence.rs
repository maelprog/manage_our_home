use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use manage_our_home_shared::validation::agenda::{paris_date, paris_start_of_day};
use rrule::{RRule, RRuleSet, Tz, Unvalidated};

/// Cap on occurrences expanded per request — a window query is always
/// date-bounded, but an unbounded RRULE (no COUNT/UNTIL) combined with a
/// huge `[from, to]` window could otherwise generate an unreasonable
/// number of rows. 1000 comfortably covers "daily for 2+ years".
const MAX_OCCURRENCES: u16 = 1000;

/// The fixed v1 display timezone, the one every form parses into and every
/// page renders back from (`apps/web/src/routes/agenda/mod.rs`'s
/// `DISPLAY_TZ`, `apps/shared`'s `paris_date`/`paris_start_of_day`). There
/// is no per-family timezone in v1.
const PARIS: Tz = Tz::Europe__Paris;

/// One expanded occurrence: the instant it starts, and the instant it ends.
/// Named because an all-day occurrence's end is not its start plus a fixed
/// duration — see `expand_all_day_occurrences`.
pub type OccurrenceSpan = (DateTime<Utc>, DateTime<Utc>);

/// Expands `rrule` (an RFC 5545 RRULE string, without the DTSTART line —
/// that's derived from `starts_at`) into occurrence start times that fall
/// within `[from, to]`. Returns an error only for a malformed or invalid
/// RRULE string; `validate` puts the very same rule set together on write,
/// so anything stored can be expanded and this should not normally fail.
///
/// The rule is unrolled in **Europe/Paris** (#116). A recurring event is a
/// wall-clock promise — « tous les lundis à 9 h » means 9 h on the clock in
/// the hall, on both sides of a change of hour — and that is exactly what
/// RFC 5545 makes a local `DTSTART` mean. Unrolled from `DTSTART:<..>Z` in
/// UTC instead, every occurrence inherited the offset in force the day the
/// series was created: a 09:00 meeting created in September came back at
/// 08:00 in November, measured on a live stack at the verification of #115.
///
/// Europe/Paris rather than a per-family zone because there is no per-family
/// zone in v1 — see `PARIS`.
///
/// « Keeps its wall clock » is not the whole story on the two nights a year
/// Paris changes hour, where a wall clock names no instant or two. What
/// happens there is a resolution, not a consequence of the promise:
///
/// - **The hour Paris skips** (last Sunday of March, no 02:00-02:59). An
///   occurrence whose wall clock falls there is not dropped: it is read with
///   the offset in force before the gap, so it lands an hour later on the
///   clock. A monthly series anchored on 2026-01-29T01:30Z (02:30 in Paris)
///   comes back on 2026-03-29 at 01:30Z — 03:30 in Paris — and at 02:30
///   again in April. Nothing in this file decides that; it is what `rrule`
///   0.14 does. RFC 5545 §3.3.10 says both things: first that an instance
///   with a « nonexistent local time » MUST be ignored and not counted, then
///   that such a time is interpreted as a DATE-TIME (§3.3.5), i.e. with the
///   offset before the gap. `rrule` follows the second reading.
/// - **The hour Paris repeats** (last Sunday of October). RFC 5545 resolves
///   a repeated wall clock to its first pass (§3.3.5, applied to recurrence
///   instances by §3.3.10), and so does the unroll. But a row stores an
///   instant, and it can store the *second* pass, which no
///   `DTSTART;TZID=` line can name. For a series anchored there — and only
///   there — this function moves the occurrences that land on a repeated
///   hour onto the second pass as well. That is a choice made here, not
///   something the RFC or `rrule` prescribes: left on the first pass, the
///   series would not render its own start as the row stores it (#116,
///   round 3). A series anchored anywhere else keeps the first pass.
pub fn expand_occurrences(
    rrule: &str,
    starts_at: DateTime<Utc>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<DateTime<Utc>>, rrule::RRuleError> {
    let set = paris_rule_set(rrule, starts_at)?;
    if !is_second_pass_of_a_repeated_hour(starts_at) {
        return Ok(occurrences_in(set, from, to));
    }
    // The series is anchored on the second pass. Every occurrence that lands
    // on a repeated hour is put on the second pass too — a choice, not a
    // deduction, see the doc above — where the unroll put it on the first,
    // an hour earlier. Widen by that hour before moving them, then filter on
    // the real instants, so `[from, to]` stays inclusive on both ends exactly
    // as this function promises.
    Ok(
        occurrences_in(set, from - Duration::hours(1), to + Duration::hours(1))
            .into_iter()
            .map(second_pass_of_a_repeated_hour)
            .filter(|occurrence| *occurrence >= from && *occurrence <= to)
            .collect(),
    )
}

/// The rule set an hour-bound series unrolls from: `rrule` anchored on
/// `starts_at` read as a Paris instant.
///
/// Built from the typed instant, **not** from a formatted
/// `DTSTART;TZID=Europe/Paris:<wall clock>` line, and that is not a matter
/// of style (#116, round 2). Paris repeats an hour every October: on
/// 2026-10-25 the wall clock reads 02:30 twice — 00:30Z in CEST, then
/// 01:30Z in CET — and `rrule` 0.14 refuses an ambiguous `DTSTART;TZID=`
/// outright rather than picking a side. The web form only reaches the first
/// of the two (`paris_local_to_utc` resolves with `earliest()`). A direct
/// API call reaches either, and so can a calendar re-import: it rewrites
/// `starts_at` from the feed without touching `rrule`
/// (`google_calendar/imports.rs`), so an imported event later given a rule
/// by `PATCH` can end up on the second pass. A formatted wall clock turned a
/// « garde de nuit, 02:30, tous les mois » into a 400 on write and a 500 on
/// read.
///
/// The anchor is the **first** pass of `starts_at`'s wall clock (#116,
/// round 3). That is not the instant the row stores when the row sits on
/// the second pass, and it is deliberate: `rrule` walks wall clocks and
/// resolves each one back with `earliest()`, so anchoring on the second
/// pass made it regenerate the head of the series an hour early, drop it as
/// earlier than `DTSTART`, and — under `COUNT` — spend the freed count a
/// day past the tail. Anchoring on the pass `rrule` will itself produce
/// keeps the head and the count; `expand_occurrences` then moves the
/// occurrences that land on a repeated hour back onto the second pass — a
/// choice of its own, documented there.
fn paris_rule_set(rrule: &str, starts_at: DateTime<Utc>) -> Result<RRuleSet, rrule::RRuleError> {
    rrule
        .parse::<RRule<Unvalidated>>()?
        .build(first_pass_of_a_repeated_hour(starts_at).with_timezone(&PARIS))
}

/// The two instants a Paris wall clock names when the clocks go back, or
/// `None` when it names just one — which is every wall clock but those of
/// the hour repeated once a year.
fn repeated_hour_passes(dt: DateTime<Utc>) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let resolved = PARIS.from_local_datetime(&dt.with_timezone(&PARIS).naive_local());
    match (resolved.earliest(), resolved.latest()) {
        (Some(first), Some(second)) if first != second => {
            Some((first.with_timezone(&Utc), second.with_timezone(&Utc)))
        }
        _ => None,
    }
}

/// Whether `dt` is the **second** of the two instants its Paris wall clock
/// names. False for every instant outside the repeated hour.
fn is_second_pass_of_a_repeated_hour(dt: DateTime<Utc>) -> bool {
    repeated_hour_passes(dt).is_some_and(|(_, second)| dt == second)
}

/// `dt` moved onto the first pass of its Paris wall clock; `dt` unchanged
/// when that wall clock names only one instant.
fn first_pass_of_a_repeated_hour(dt: DateTime<Utc>) -> DateTime<Utc> {
    repeated_hour_passes(dt).map_or(dt, |(first, _)| first)
}

/// `dt` moved onto the second pass of its Paris wall clock; `dt` unchanged
/// when that wall clock names only one instant.
fn second_pass_of_a_repeated_hour(dt: DateTime<Utc>) -> DateTime<Utc> {
    repeated_hour_passes(dt).map_or(dt, |(_, second)| second)
}

/// The `DTSTART` line for an unroll on instants, in UTC. Only the all-day
/// stand-in uses it — see `expand_all_day_occurrences`.
fn utc_dtstart(starts_at: DateTime<Utc>) -> String {
    format!("DTSTART:{}", starts_at.format("%Y%m%dT%H%M%SZ"))
}

/// Unrolls from a `<dtstart>\nRRULE:<rrule>` text. Only the all-day
/// stand-in uses it: its `DTSTART` is a UTC midnight, which no zone can
/// make ambiguous.
fn expand_from_dtstart(
    dtstart: &str,
    rrule: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<DateTime<Utc>>, rrule::RRuleError> {
    let set: RRuleSet = format!("{dtstart}\nRRULE:{rrule}").parse()?;
    Ok(occurrences_in(set, from, to))
}

/// The occurrences of `set` inside `[from, to]`, as UTC instants.
fn occurrences_in(set: RRuleSet, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<DateTime<Utc>> {
    // `RRuleSet::after`/`before` are exclusive of the boundary instant, but
    // callers (list_events) treat `[from, to]` as inclusive on both ends —
    // nudge by a second so an occurrence landing exactly on `from` or `to`
    // isn't silently dropped.
    let set = set
        .after((from - Duration::seconds(1)).with_timezone(&Tz::UTC))
        .before((to + Duration::seconds(1)).with_timezone(&Tz::UTC));

    set.all(MAX_OCCURRENCES)
        .dates
        .into_iter()
        .map(|d| d.with_timezone(&Utc))
        .collect()
}

/// Expands an **all-day** event's recurrence, on civil dates rather than on
/// instants: returns each occurrence as `(starts_at, ends_at)` covering
/// whole Europe/Paris days, the same invariant `normalize_all_day` puts on
/// the stored row.
///
/// Unrolling the stored instants directly cannot work here, and the reason
/// is the invariant itself (#101, round 2). An all-day row is anchored on
/// Paris midnight, which is 22:00Z the previous day in summer and 23:00Z in
/// winter — i.e. exactly on the DST cliff. Written into `DTSTART:<..>Z` and
/// unrolled in UTC, every occurrence inherits the offset in force the month
/// the series was created and slides onto the neighbouring civil day once
/// the clocks change. A monthly reminder set on the 5th of September comes
/// back on 2026-11-04T22:00Z — 23:00 on the *4th* in Paris. The dashboard
/// then drops it on the day it actually falls, and `/agenda` files it under
/// the wrong date: the very symptom #101 is about, re-created for recurring
/// events.
///
/// The same cliff breaks `BYDAY` outright, DST or no DST: Paris midnight on
/// a Saturday is a *Friday* in UTC, so `FREQ=WEEKLY;BYDAY=SA` unrolled in
/// UTC lands on Sundays from its very first occurrence.
///
/// So the rule is unrolled on a UTC-midnight stand-in for each civil date,
/// where no offset can move a date, and each resulting date is then mapped
/// back to the Paris day it names. The window is widened by a day on each
/// side before unrolling (a real instant sits 1-2 h before its stand-in)
/// and the results filtered on the real instants, so `[from, to]` stays
/// inclusive on both ends exactly as `expand_occurrences` promises.
///
/// The stand-in is unrolled through `utc_dtstart`, not through
/// `expand_occurrences`, on purpose: since #116 the latter unrolls in
/// Europe/Paris, and this path's whole construction — and the tests below
/// that pin it — rest on a zone where no offset can move a date. Keeping it
/// on its own `DTSTART` line makes it independent of that choice rather
/// than quietly riding on it. Whether the two unrollings can now be folded
/// into one is a question of structure, not of behaviour; it was left out
/// of #116 deliberately, and no issue carries it yet.
///
/// `ends_at` is read as a **span in civil days**, not as a duration: a
/// three-day break stays three days in a month where one of them is 23 or
/// 25 hours long.
pub fn expand_all_day_occurrences(
    rrule: &str,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<OccurrenceSpan>, rrule::RRuleError> {
    let first_day = paris_date(starts_at);
    let span_days = (paris_date(ends_at) - first_day).num_days().max(1);

    let stand_in_start = first_day.and_time(NaiveTime::MIN).and_utc();
    let dates = expand_from_dtstart(
        &utc_dtstart(stand_in_start),
        rrule,
        from - Duration::days(1),
        to + Duration::days(1),
    )?;

    Ok(dates
        .into_iter()
        .filter_map(|stand_in| {
            let day = stand_in.date_naive();
            let start = paris_start_of_day(day);
            let end = paris_start_of_day(add_days(day, span_days));
            (start >= from && start <= to).then_some((start, end))
        })
        .collect())
}

/// `date + n` days, saturating at chrono's representable range instead of
/// panicking. Only a rule reaching the year 262143 can hit the fallback.
fn add_days(date: NaiveDate, n: i64) -> NaiveDate {
    date.checked_add_signed(Duration::days(n)).unwrap_or(date)
}

/// Validates that `rrule` parses as a well-formed RRULE, used when
/// creating/updating an event so a bad value is rejected at write time
/// (400) instead of surfacing as a silent empty expansion later.
///
/// It runs `paris_rule_set` — the very construction `expand_occurrences`
/// unrolls from — and throws the result away. That identity is the point,
/// and it is structural rather than argued: whatever is accepted here can
/// be expanded later by `expand_occurrences`, because it is literally the
/// same call. An earlier version of this PR argued instead that the two
/// `DTSTART` forms accept the same strings; they did not, and the gap was
/// exactly the ambiguous Paris wall clock that `paris_rule_set` now
/// sidesteps.
///
/// That identity does **not** extend to all-day rows. They go through here
/// on their own `starts_at`, while `expand_all_day_occurrences` unrolls a
/// UTC midnight stand-in, and the two do not accept the same rules. They
/// part at least on an `UNTIL` that falls before the stand-in: an all-day
/// event on 2026-09-05 is stored at 2026-09-04T22:00Z, and
/// `FREQ=DAILY;UNTIL=20260904T235959Z` — what `build_rrule` writes for
/// « Jusqu'au 2026-09-04 » — passes here, then fails there with
/// `UntilBeforeStart`: a 201 on write, a 500 on the whole window on read.
/// Pre-existing (the same on `main`), tracked in #161 and deliberately left
/// out of #116. Nothing here shows that it is the only such gap.
pub fn validate(rrule: &str, starts_at: DateTime<Utc>) -> Result<(), rrule::RRuleError> {
    paris_rule_set(rrule, starts_at).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone, Weekday};

    #[test]
    fn expands_weekly_recurrence_within_window() {
        let start = Utc.with_ymd_and_hms(2026, 1, 5, 9, 0, 0).unwrap(); // Monday
        let from = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 2, 10, 0, 0, 0).unwrap();

        let occurrences = expand_occurrences("FREQ=WEEKLY;COUNT=5", start, from, to).unwrap();
        assert_eq!(occurrences.len(), 5);
        assert_eq!(occurrences[0], start);
    }

    #[test]
    fn rejects_malformed_rrule() {
        let start = Utc.with_ymd_and_hms(2026, 1, 5, 9, 0, 0).unwrap();
        assert!(validate("NOT_A_VALID_RRULE", start).is_err());
    }

    #[test]
    fn includes_occurrence_landing_exactly_on_the_window_bounds() {
        let start = Utc.with_ymd_and_hms(2026, 1, 5, 9, 0, 0).unwrap(); // Monday
        let occurrences = expand_occurrences("FREQ=WEEKLY;COUNT=2", start, start, start).unwrap();
        assert_eq!(occurrences, vec![start]);
    }

    // -- expand_occurrences across a DST change (#116) ------------------------
    //
    // An hour-bound series is a wall-clock promise: « tous les lundis à 9 h »
    // means 9 h on the clock in the hall, on both sides of a change of hour —
    // what RFC 5545 says a `DTSTART;TZID=` rule means. Unrolling in UTC
    // freezes the offset in force the day the series was created, so every
    // occurrence past the next change reads an hour off.

    /// The UTC instant a row stores for a Paris wall-clock time — what the
    /// form produces (`apps/web`'s `paris_local_to_utc`).
    fn paris(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        PARIS
            .with_ymd_and_hms(y, m, d, h, min, 0)
            .single()
            .expect("unambiguous Paris wall clock")
            .with_timezone(&Utc)
    }

    #[test]
    fn a_monthly_hourly_rule_keeps_its_paris_wall_clock_after_the_clocks_go_back() {
        // The reproduction from #116, measured on a live stack: a 09:00
        // meeting created on 2026-09-05 (Paris UTC+2) came back in November
        // (UTC+1) at 2026-11-05T07:00:00Z — 08:00 in Paris, an hour early.
        let occs = expand_occurrences(
            "FREQ=MONTHLY",
            paris(2026, 9, 5, 9, 0),
            paris(2026, 11, 1, 0, 0),
            paris(2026, 11, 30, 0, 0),
        )
        .unwrap();
        assert_eq!(occs.len(), 1);
        assert_eq!(occs[0], paris(2026, 11, 5, 9, 0));
    }

    #[test]
    fn a_monthly_hourly_rule_keeps_its_paris_wall_clock_after_the_clocks_go_forward() {
        // The mirror case, which a fix pinned to one offset would miss: a
        // series created in winter (UTC+1) whose occurrence falls in summer
        // (UTC+2) drifts an hour *late* rather than early.
        let occs = expand_occurrences(
            "FREQ=MONTHLY",
            paris(2026, 1, 5, 9, 0),
            paris(2026, 7, 1, 0, 0),
            paris(2026, 7, 31, 0, 0),
        )
        .unwrap();
        assert_eq!(occs.len(), 1);
        assert_eq!(occs[0], paris(2026, 7, 5, 9, 0));
    }

    #[test]
    fn a_weekly_hourly_rule_keeps_its_paris_wall_clock_and_weekday_across_the_change() {
        // 2026-10-19 is a Monday; Paris goes back to UTC+1 on 2026-10-25.
        // Every Monday of the window has to read 09:00 in Paris, and has to
        // still be a Monday there.
        let occs = expand_occurrences(
            "FREQ=WEEKLY;BYDAY=MO",
            paris(2026, 10, 19, 9, 0),
            paris(2026, 10, 19, 0, 0),
            paris(2026, 11, 9, 23, 59),
        )
        .unwrap();
        assert_eq!(
            occs,
            vec![
                paris(2026, 10, 19, 9, 0),
                paris(2026, 10, 26, 9, 0),
                paris(2026, 11, 2, 9, 0),
                paris(2026, 11, 9, 9, 0),
            ]
        );
        for occ in &occs {
            assert_eq!(paris_date(*occ).weekday(), Weekday::Mon);
        }
    }

    // -- the repeated hour (#116, round 2) -----------------------------------
    //
    // Paris does not just change offset, it *repeats* an hour: on
    // 2026-10-25, 02:30 happens twice — 00:30Z in CEST, then 01:30Z in CET.
    // A wall-clock string naming that hour is ambiguous, and `rrule` 0.14
    // refuses one outright rather than picking a side. The web form only
    // reaches the first of the two — `apps/web`'s `paris_local_to_utc`
    // resolves `02:30` with `earliest()` — while a direct API call reaches
    // either, and so can a calendar re-import on an imported event later
    // given a rule by `PATCH` (the import rewrites `starts_at`, not `rrule`).
    //
    // A rule anchored there has to keep being accepted on write and keep
    // expanding on read. The second matters most: `list_events` turns an
    // expansion error into a 500 for the *whole* window, so one such row
    // would take `/agenda` and the dashboard down with it.
    //
    // Not erroring is not enough, and the two tests below missed that at
    // first because their window opened in November. Unrolling walks the
    // wall clock, and mapping 02:30 back to an instant resolves to the
    // *first* pass — so a series anchored on the second pass regenerated
    // its own start an hour early, `rrule` dropped it as earlier than
    // `DTSTART`, and the series silently lost its first occurrence (and,
    // under `COUNT`, slid a day and spent the count elsewhere). A 200 OK
    // with an occurrence missing, where the round before had a 500. So the
    // window has to contain October, and the assertions below pin the
    // instants, not just the absence of an error.

    /// The two instants Paris wall-clock 02:30 names on 2026-10-25.
    fn first_repeated_0230() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 25, 0, 30, 0).unwrap()
    }
    fn second_repeated_0230() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 25, 1, 30, 0).unwrap()
    }

    #[test]
    fn a_rule_anchored_on_the_repeated_hour_still_expands() {
        for anchor in [first_repeated_0230(), second_repeated_0230()] {
            let occs = expand_occurrences(
                "FREQ=MONTHLY",
                anchor,
                Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2026, 11, 30, 0, 0, 0).unwrap(),
            )
            .unwrap_or_else(|e| panic!("anchor {anchor} failed to expand: {e}"));
            // November has no repeated hour: 02:30 Paris is 01:30Z, whichever
            // of the two October instants the series was anchored on.
            assert_eq!(occs, vec![paris(2026, 11, 25, 2, 30)]);
        }
    }

    #[test]
    fn a_rule_anchored_on_the_repeated_hour_renders_its_own_start() {
        // The window opens in October, on purpose: an occurrence lost at the
        // head of the series is invisible to a November-only window.
        for anchor in [first_repeated_0230(), second_repeated_0230()] {
            let occs = expand_occurrences(
                "FREQ=MONTHLY",
                anchor,
                Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2026, 11, 30, 0, 0, 0).unwrap(),
            )
            .unwrap_or_else(|e| panic!("anchor {anchor} failed to expand: {e}"));
            assert_eq!(
                occs,
                vec![anchor, paris(2026, 11, 25, 2, 30)],
                "the series anchored on {anchor} does not render its own start"
            );
        }
    }

    #[test]
    fn a_daily_rule_on_the_repeated_hour_keeps_its_days_and_its_count() {
        // `COUNT` makes the loss double: the dropped head is not replaced at
        // the head, it is spent at the tail, so the series both starts a day
        // late and ends a day late.
        for anchor in [first_repeated_0230(), second_repeated_0230()] {
            let occs = expand_occurrences(
                "FREQ=DAILY;COUNT=3",
                anchor,
                Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2026, 11, 30, 0, 0, 0).unwrap(),
            )
            .unwrap_or_else(|e| panic!("anchor {anchor} failed to expand: {e}"));
            assert_eq!(
                occs,
                vec![
                    anchor,
                    paris(2026, 10, 26, 2, 30),
                    paris(2026, 10, 27, 2, 30),
                ],
                "the series anchored on {anchor} lost or slid its days"
            );
        }
    }

    #[test]
    fn a_rule_anchored_on_the_repeated_hour_is_accepted_on_write() {
        for anchor in [first_repeated_0230(), second_repeated_0230()] {
            assert!(
                validate("FREQ=MONTHLY", anchor).is_ok(),
                "anchor {anchor} was rejected at write time"
            );
        }
    }

    // -- expand_all_day_occurrences ------------------------------------------
    //
    // #101, round 2. Anchoring an all-day event on Paris midnight puts its
    // stored `starts_at` on the DST cliff: 22:00Z the previous day in
    // summer, 23:00Z in winter. Written into `DTSTART:<..>Z` and unrolled
    // in UTC — which is what this path still does, on a midnight stand-in
    // rather than on the row's own instant — every occurrence would keep
    // the offset of the month the series was created in and slide onto the
    // wrong civil day once the clocks change: the exact symptom #101 is
    // about, re-created for recurring events.

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// Paris midnight opening `y-m-d` — what the row stores for an all-day
    /// event on that date.
    fn midnight(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        paris_start_of_day(day(y, m, d))
    }

    #[test]
    fn a_monthly_all_day_rule_stays_on_its_civil_day_across_the_dst_change() {
        // A rent reminder on the 5th, created in September (Paris UTC+2).
        // November is UTC+1: unrolled in UTC the occurrence sits at
        // 2026-11-04T22:00Z, i.e. 23:00 on the *4th* in Paris.
        let occs = expand_all_day_occurrences(
            "FREQ=MONTHLY",
            midnight(2026, 9, 5),
            midnight(2026, 9, 6),
            midnight(2026, 11, 1),
            midnight(2026, 11, 30),
        )
        .unwrap();
        assert_eq!(occs.len(), 1);
        assert_eq!(paris_date(occs[0].0), day(2026, 11, 5));
        assert_eq!(occs[0].0, midnight(2026, 11, 5));
        assert_eq!(occs[0].1, midnight(2026, 11, 6));
    }

    #[test]
    fn a_weekly_all_day_rule_keeps_its_weekday() {
        // 2026-09-05 is a Saturday. Its Paris midnight is 2026-09-04T22:00Z
        // — a *Friday* in UTC — so a UTC unroll of BYDAY=SA walks off by a
        // day from the very first occurrence.
        let occs = expand_all_day_occurrences(
            "FREQ=WEEKLY;BYDAY=SA",
            midnight(2026, 9, 5),
            midnight(2026, 9, 6),
            midnight(2026, 9, 1),
            midnight(2026, 9, 30),
        )
        .unwrap();
        assert!(!occs.is_empty());
        for (start, _) in &occs {
            assert_eq!(
                paris_date(*start).weekday(),
                Weekday::Sat,
                "occurrence {start} is not a Saturday in Paris"
            );
        }
    }

    #[test]
    fn an_all_day_occurrence_lasts_its_own_civil_day_on_the_long_night() {
        // 2026-10-25 is 25 h long in Paris. A duration carried over from the
        // base occurrence would end that day an hour early.
        let occs = expand_all_day_occurrences(
            "FREQ=DAILY",
            midnight(2026, 10, 20),
            midnight(2026, 10, 21),
            midnight(2026, 10, 25),
            midnight(2026, 10, 25),
        )
        .unwrap();
        assert_eq!(occs.len(), 1);
        assert_eq!(occs[0].0, midnight(2026, 10, 25));
        assert_eq!(occs[0].1, midnight(2026, 10, 26));
        assert_eq!((occs[0].1 - occs[0].0).num_hours(), 25);
    }

    #[test]
    fn an_all_day_occurrence_lasts_its_own_civil_day_on_the_short_night() {
        // 2026-03-29 is 23 h long in Paris.
        let occs = expand_all_day_occurrences(
            "FREQ=DAILY",
            midnight(2026, 3, 25),
            midnight(2026, 3, 26),
            midnight(2026, 3, 29),
            midnight(2026, 3, 29),
        )
        .unwrap();
        assert_eq!(occs.len(), 1);
        assert_eq!(occs[0].0, midnight(2026, 3, 29));
        assert_eq!(occs[0].1, midnight(2026, 3, 30));
        assert_eq!((occs[0].1 - occs[0].0).num_hours(), 23);
    }

    #[test]
    fn a_multi_day_all_day_rule_keeps_its_span_in_civil_days() {
        // A three-day break, repeated monthly: every occurrence covers three
        // whole Paris days, whatever the offset in force that month.
        let occs = expand_all_day_occurrences(
            "FREQ=MONTHLY",
            midnight(2026, 9, 5),
            midnight(2026, 9, 8),
            midnight(2026, 11, 1),
            midnight(2026, 11, 30),
        )
        .unwrap();
        assert_eq!(occs.len(), 1);
        assert_eq!(occs[0].0, midnight(2026, 11, 5));
        assert_eq!(occs[0].1, midnight(2026, 11, 8));
    }

    #[test]
    fn an_all_day_occurrence_landing_exactly_on_a_window_bound_is_kept() {
        let bound = midnight(2026, 11, 5);
        let occs = expand_all_day_occurrences(
            "FREQ=DAILY;COUNT=3",
            midnight(2026, 11, 5),
            midnight(2026, 11, 6),
            bound,
            bound,
        )
        .unwrap();
        assert_eq!(occs.len(), 1);
        assert_eq!(occs[0].0, bound);
    }
}
