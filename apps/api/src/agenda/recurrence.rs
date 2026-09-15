use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use manage_our_home_shared::validation::agenda::{paris_date, paris_start_of_day};
use rrule::{Frequency, RRule, RRuleSet, Tz, Unvalidated};

/// Cap on occurrences expanded per request — a window query is always
/// date-bounded, but an unbounded RRULE (no COUNT/UNTIL) combined with a
/// huge `[from, to]` window could otherwise generate an unreasonable
/// number of rows. 1000 comfortably covers "daily for 2+ years".
///
/// It counts the occurrences a call **returns**, on both paths: an
/// occurrence outside `[from, to]` that the unroll walks past costs nothing
/// (#119).
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
/// within `[from, to]`. Returns an error when the rule cannot be unrolled
/// from that anchor: malformed, or invalid against it (an `UNTIL` earlier
/// than `starts_at`, say).
///
/// `validate` runs the same construction on write, so a rule unrolls from
/// the `starts_at` it was accepted with. That is **not** a promise that
/// every stored series unrolls: what is stored is not always what was
/// validated. A calendar re-import rewrites `starts_at` from the feed
/// without going back through `validate` (`google_calendar/imports.rs`),
/// and can move it past the rule's `UNTIL`, or a row can predate a check.
/// A stored series can therefore fail here; `list_events` then renders that
/// row on its own and logs why, instead of failing the whole window (#161).
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
///   again in April. That much is what `rrule` 0.14 does. RFC 5545 §3.3.10
///   says both things: first that an instance with a « nonexistent local
///   time » MUST be ignored and not counted, then that such a time is
///   interpreted as a DATE-TIME (§3.3.5), i.e. with the offset before the
///   gap. `rrule` follows the second reading.
///
///   Read that way, a rule that steps by the hour or less — or that lists
///   both 02:xx and 03:xx — produces on that day an instant it also
///   produces from 03:xx: 02:00 is read as 01:00Z, which is what 03:00
///   names. `rrule` returns both, and under `COUNT` spends a unit on each
///   (#116, round 4). **This function keeps one**: an instant the series
///   has already produced is not produced again, and is not counted, so a
///   `COUNT=n` series has n distinct instants. That is a choice made here.
///   The RFC does not say how its two sentences combine; it does say a
///   recurrence set holds an instant once (« Duplicate instances are
///   ignored », §3.8.5.3, written for RRULE against RDATE). And it is what
///   the first sentence gives wherever the two readings collide — the gap's
///   copy is ignored and not counted — while the second keeps governing
///   everywhere they do not: daily, weekly, or every two hours from
///   midnight, the gap's occurrence lands on no other one and stays, an
///   hour later on the clock. Only instants in the hour right after a gap
///   are compared; nothing else is touched.
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
///
///   The first pass is the *only* pass such a series gets: a rule that steps
///   by the hour or less, walking wall clocks, reaches 02:00 once and goes
///   on to 03:00. `FREQ=HOURLY` from 2026-10-25T00:00Z (02:00 CEST) gives
///   00:00Z, then 02:00Z (03:00 CET) — never 01:00Z, the second 02:00.
///   Unrolled in UTC, before #116, it did give 01:00Z. That follows from
///   the first-pass reading above, and is left as is.
pub fn expand_occurrences(
    rrule: &str,
    starts_at: DateTime<Utc>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<DateTime<Utc>>, rrule::RRuleError> {
    if !is_second_pass_of_a_repeated_hour(starts_at) {
        return paris_occurrences_in(rrule, starts_at, from, to);
    }
    // The series is anchored on the second pass. Every occurrence that lands
    // on a repeated hour is put on the second pass too — a choice, not a
    // deduction, see the doc above — where the unroll put it on the first,
    // an hour earlier. Widen by that hour before moving them, then filter on
    // the real instants, so `[from, to]` stays inclusive on both ends exactly
    // as this function promises.
    Ok(paris_occurrences_in(
        rrule,
        starts_at,
        from - Duration::hours(1),
        to + Duration::hours(1),
    )?
    .into_iter()
    .map(second_pass_of_a_repeated_hour)
    .filter(|occurrence| *occurrence >= from && *occurrence <= to)
    .collect())
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
    build_paris_rule_set(rrule.parse()?, starts_at)
}

fn build_paris_rule_set(
    rule: RRule<Unvalidated>,
    starts_at: DateTime<Utc>,
) -> Result<RRuleSet, rrule::RRuleError> {
    rule.build(first_pass_of_a_repeated_hour(starts_at).with_timezone(&PARIS))
}

/// The occurrences of an hour-bound series inside `[from, to]`, each instant
/// once — see « the hour Paris skips » on `expand_occurrences`.
///
/// `rrule` counts `COUNT` itself, copies included, so the rule is unrolled
/// with its count lifted and the count is kept here, on distinct instants.
/// `validate` builds the same rule with its own `COUNT`; nothing `rrule`
/// validates depends on the count's value, so both accept and refuse the
/// same rules. Everything else follows what `RRuleSet::all` does on the
/// window `occurrences_in` gives it — same bounds, same stop past `to`,
/// same `MAX_OCCURRENCES`, same iteration limits — so a series that
/// produces no copy unrolls exactly as it did through `occurrences_in`.
fn paris_occurrences_in(
    rrule: &str,
    starts_at: DateTime<Utc>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<DateTime<Utc>>, rrule::RRuleError> {
    let rule: RRule<Unvalidated> = rrule.parse()?;
    let count = rule.get_count();
    let rule = match count {
        Some(_) => rule.count(u32::MAX),
        None => rule,
    };
    let set = build_paris_rule_set(rule, starts_at)?.limit();

    // The same one-second nudge as `occurrences_in`, for the same reason.
    let (after, before) = (from - Duration::seconds(1), to + Duration::seconds(1));
    let mut produced_after_a_gap = ProducedAfterAGap::default();
    let mut distinct: u32 = 0;
    let mut dates = Vec::new();
    let mut iter = set.into_iter();
    while dates.len() < usize::from(MAX_OCCURRENCES) && count.is_none_or(|n| distinct < n) {
        let Some(occurrence) = iter.next() else { break };
        let occurrence = occurrence.with_timezone(&Utc);
        if !produced_after_a_gap.first_time(occurrence) {
            continue;
        }
        distinct += 1;
        if occurrence >= after && occurrence <= before {
            dates.push(occurrence);
        }
        if occurrence > before {
            break;
        }
    }
    Ok(dates)
}

/// The instants a series has produced in the hour right after a skipped
/// hour — the only hour where `rrule` can produce one twice (02:00 read as
/// 03:00). Forgotten once the series is well past that hour, so a series
/// unrolled over years holds at most one such hour at a time.
#[derive(Default)]
struct ProducedAfterAGap {
    seen: std::collections::HashSet<DateTime<Utc>>,
    latest: Option<DateTime<Utc>>,
}

impl ProducedAfterAGap {
    /// Whether `occurrence` is produced for the first time. Always true
    /// outside the hour after a gap.
    fn first_time(&mut self, occurrence: DateTime<Utc>) -> bool {
        if !follows_a_skipped_hour(occurrence) {
            // A copy is at most an hour behind the instant produced before
            // it; two hours past the last one seen, none can come back.
            if self
                .latest
                .is_some_and(|latest| occurrence > latest + Duration::hours(2))
            {
                self.seen.clear();
                self.latest = None;
            }
            return true;
        }
        self.latest = self.latest.max(Some(occurrence));
        self.seen.insert(occurrence)
    }
}

/// Whether `dt` falls in the hour right after one Paris skips: its wall
/// clock an hour earlier does not exist (2026-03-29T01:00Z reads 03:00, and
/// there was no 02:00). False everywhere else.
fn follows_a_skipped_hour(dt: DateTime<Utc>) -> bool {
    let an_hour_earlier = dt.with_timezone(&PARIS).naive_local() - Duration::hours(1);
    PARIS
        .from_local_datetime(&an_hour_earlier)
        .earliest()
        .is_none()
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

/// The rule set an all-day series unrolls from: `rrule` anchored on the
/// UTC-midnight stand-in for the Paris date `starts_at` opens — see
/// `expand_all_day_occurrences`. Its `DTSTART` is a UTC midnight, which no
/// zone can make ambiguous.
///
/// `expand_all_day_occurrences` unrolls this set and `validate` builds it
/// for an all-day row, so the two accept and refuse the same rules (#161).
fn all_day_rule_set(rrule: &str, starts_at: DateTime<Utc>) -> Result<RRuleSet, rrule::RRuleError> {
    let stand_in_start = paris_date(starts_at).and_time(NaiveTime::MIN).and_utc();
    format!("{}\nRRULE:{rrule}", utc_dtstart(stand_in_start)).parse()
}

/// The occurrences of `set` from the start of the series, as UTC instants:
/// lazily, with no window and no cap. The caller stops the walk.
///
/// Neither bound belongs here (#119). An all-day occurrence is a civil date,
/// and the instant returned for it sits an hour or two *before* the
/// UTC-midnight stand-in the unroll produces, so only the caller, once it
/// has mapped a stand-in back to its Paris day, can tell whether the
/// occurrence falls in the window — and only there can the cap count
/// occurrences the caller actually returns. Handing `RRuleSet` the widened
/// window and letting `all` cap the widened result spent part of
/// `MAX_OCCURRENCES` outside the window asked for: a series with an
/// occurrence on the extra day before `from` — any series that started
/// before the window — came back with 999 occurrences instead of 1000, the
/// last one dropped in silence at the far end.
///
/// `after`/`before` would not have carried over anyway: `rrule` 0.14 honours
/// them in `RRuleSet::all` and ignores them in the iterator API. `limit`
/// keeps the iteration guards `all` enables, the ones that stop a rule which
/// walks without producing.
fn occurrences_of(set: RRuleSet) -> impl Iterator<Item = DateTime<Utc>> {
    set.limit().into_iter().map(|d| d.with_timezone(&Utc))
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
/// back to the Paris day it names. `[from, to]` is compared against those
/// Paris instants and not against the stand-ins, which sit an hour or two
/// later, so it stays inclusive on both ends exactly as `expand_occurrences`
/// promises. `MAX_OCCURRENCES` is spent on the occurrences this function
/// returns, and on nothing else: the unroll is walked lazily and stopped
/// here rather than capped on a widened window (#119, see `occurrences_of`).
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

    let mut spans = Vec::new();
    for stand_in in occurrences_of(all_day_rule_set(rrule, starts_at)?) {
        let day = stand_in.date_naive();
        let start = paris_start_of_day(day);
        if start > to {
            break;
        }
        if start >= from {
            spans.push((start, paris_start_of_day(add_days(day, span_days))));
            if spans.len() == usize::from(MAX_OCCURRENCES) {
                break;
            }
        }
    }
    Ok(spans)
}

/// The occurrences of a stored series inside `[from, to]` (inclusive), as
/// `(starts_at, ends_at)` spans — the one unroll **every reader** of a
/// series goes through: `list_events` for the agenda, `refill_notifications`
/// for the reminders.
///
/// An all-day series is unrolled on civil dates, not on instants. Its stored
/// start sits on Paris midnight — 22:00Z in summer, 23:00Z in winter — so
/// unrolling it in UTC carries every later occurrence onto the neighbouring
/// day as soon as the clocks change, which is #101's own symptom re-created
/// one level up: see `expand_all_day_occurrences`. An hour-bound series
/// keeps its duration in real time: see `expand_occurrences`.
///
/// One function rather than two call sites each choosing an unroll (#169).
/// The reminders used to call `expand_occurrences` whatever the row, i.e.
/// unroll an all-day series on instants from the Paris midnight it stores,
/// and the two readers parted on an `UNTIL` between that midnight and the
/// end of its UTC day. `FREQ=DAILY;UNTIL=20261010T235959Z` — what
/// `build_rrule` writes for « Jusqu'au 10/10 » — listed ten days from
/// 1 October and reminded eleven: Paris midnight on the 11th is
/// 2026-10-10T22:00Z, before the `UNTIL`. On civil dates the 11th is past it.
pub fn expand_series(
    rrule: &str,
    all_day: bool,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<OccurrenceSpan>, rrule::RRuleError> {
    if all_day {
        expand_all_day_occurrences(rrule, starts_at, ends_at, from, to)
    } else {
        let duration = ends_at - starts_at;
        expand_occurrences(rrule, starts_at, from, to)
            .map(|starts| starts.into_iter().map(|s| (s, s + duration)).collect())
    }
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
/// It runs `paris_rule_set` — the construction `expand_occurrences` unrolls
/// from, down to the anchor; the unroll only lifts `COUNT` to keep it
/// itself, and `rrule` validates nothing on the count's value — and throws
/// the result away. That identity is structural rather than argued: a rule
/// accepted here with a given `starts_at` can be expanded later by
/// `expand_occurrences` **from that same `starts_at`**. An earlier version
/// of this PR argued instead that the two `DTSTART` forms accept the same
/// strings; they did not, and the gap was exactly the ambiguous Paris wall
/// clock that `paris_rule_set` now sidesteps.
///
/// An **all-day** row (`all_day`) is checked on its own construction as
/// well, `all_day_rule_set`, because that is what
/// `expand_all_day_occurrences` unrolls: a UTC-midnight stand-in for its
/// Paris date, not the Paris midnight the row stores. Checked on the stored
/// instant, the two parted on an `UNTIL` between them (#161): an all-day
/// event on 2026-09-05 is stored at 2026-09-04T22:00Z, and
/// `FREQ=DAILY;UNTIL=20260904T235959Z` — what `build_rrule` writes for
/// « Jusqu'au 2026-09-04 » — was a 201 on write, then `UntilBeforeStart`
/// and a 500 on the whole window on read. The same identity holds: a rule
/// accepted here unrolls from the same row.
///
/// Neither identity says anything about a `starts_at` that changes without
/// coming back here. A calendar re-import does exactly that: it rewrites
/// `starts_at` from the feed and leaves `rrule` alone
/// (`google_calendar/imports.rs`). An imported event on 2026-10-01 given
/// `FREQ=DAILY;UNTIL=20261010T235959Z` by `PATCH` passes here; moved by
/// the feed to 2026-10-25 and re-imported, it no longer unrolls. That is
/// not closed here but on read: `list_events` renders such a row on its
/// own and logs why, rather than failing the window (#161).
///
/// An all-day series has a **second reader**, and until #169 it did not
/// unroll the same construction: `refill_notifications` went through
/// `expand_occurrences`, i.e. `paris_rule_set`, from the Paris midnight the
/// row stores. The two parse the rule differently — `all_day_rule_set`
/// formats it into a text of several lines, `paris_rule_set` parses it as
/// one — and they part on rules that either one alone would take. So an
/// all-day rule is checked on both, the second from the instant
/// `normalize_all_day` will store rather than the one the client sent
/// (#165). Checked on `all_day_rule_set` alone, `FREQ=WEEKLY;BYDAY=X:MO`
/// was a 201, then a 500 on `POST /reminders`.
///
/// Both readers now unroll through `expand_series` (#169), so the reminders
/// read an all-day row on `all_day_rule_set` too, and no reader builds
/// `paris_rule_set` for an all-day row any more. The check on it is still
/// here, and as far as can be seen it is **redundant** for such a row, with
/// `rrule` 0.14: once `:` and line breaks are refused, both constructions
/// parse the same value the same way, and the only checks that depend on
/// the anchor are the zone of `UNTIL` (UTC, on both) and `UNTIL` not before
/// it. The Paris midnight the row stores always precedes the UTC-midnight
/// stand-in by an hour or two, so an `UNTIL` the stand-in takes is taken by
/// the stored instant too. Measured, not proven: a fuzz at the verification
/// of #172 found 0 of 240 000 (rule, anchor) pairs accepted by
/// `all_day_rule_set` and refused by `paris_rule_set`, and a narrower one
/// run while fixing it 0 of 183 960 (every Paris day of 2026, 7 frequencies,
/// 6 extra parts, 12 `COUNT`/`UNTIL` forms around both anchors). Kept rather
/// than removed because removing it was not #169's to decide — #162 is where
/// the two constructions are weighed — and because the redundancy rests on
/// `rrule`'s current checks, which an upgrade could change.
///
/// And a rule is **one value**, whichever the path: no line break, no `:`.
/// A rule carrying a line break injected its own `EXDATE:`, `RDATE:` or
/// `DTSTART:` lines into `all_day_rule_set`'s text; on the hour-bound path,
/// the parser picks a property name from any line, so
/// `FREQ=WEEKLY\nRRULE:FREQ=DAILY` was stored as written and unrolled as
/// `FREQ=DAILY`. A `:` does the same on a single line, because the two
/// readers do not cut the rule at the same place: `all_day_rule_set` keeps
/// it whole after its own `RRULE:`, `paris_rule_set` keeps only what follows
/// its first `:`. An all-day `BYDAY=1:WKST=MO;FREQ=WEEKLY` on a Saturday was
/// accepted by both, then listed on Mondays and reminded on Saturdays; the
/// hour-bound `FREQ=WEEKLY;X:FREQ=DAILY` was stored as written and unrolled
/// every day. None of these is an RRULE value — RFC 5545 §3.3.10 gives a
/// value no `:` and no line — and none of our clients writes one
/// (`build_rrule`); they are refused before any parser sees them rather than
/// left to where each parser happens to cut (#165). That refuses
/// `RRULE:FREQ=DAILY` too, which the hour-bound path used to take as
/// `FREQ=DAILY`: the property name is not part of the value, and the API
/// derives that line itself.
///
/// And a rule is **ASCII** (#170). `rrule` 0.14 cuts a `BYDAY` entry two
/// bytes before its end (`NWeekday::from_str`) and panics when that cut
/// lands inside a multibyte character: `FREQ=WEEKLY;BYDAY=éA` took the
/// handler down instead of answering. RFC 5545 §3.3.10 writes a whole
/// RRULE value in ASCII, so the rule is refused on its first non-ASCII
/// byte, wherever it sits, before any parser sees it.
///
/// And an all-day rule **steps by days** (#171): no `FREQ=HOURLY`,
/// `MINUTELY` or `SECONDLY`, no `BYHOUR`, `BYMINUTE` or `BYSECOND`.
/// `expand_all_day_occurrences` keeps the date of each occurrence and
/// nothing of its time, so such a rule unrolls into copies of the same day:
/// `FREQ=HOURLY;COUNT=5` was listed five times on its one day and reminded
/// once, the reminders keeping one row per instant, and an unbounded
/// `FREQ=MINUTELY` spent `MAX_OCCURRENCES` on its first day, the rest of the
/// window empty on both readers. Refusing them on write rather than
/// collapsing the copies on read is the maintainer's call (2026-09-15, not
/// the RFC's). RFC 5545 §3.3.10 only speaks to the `BY*` parts: it says
/// `BYSECOND`, `BYMINUTE` and `BYHOUR` MUST NOT be used when `DTSTART` is a
/// DATE, and that such values MUST be ignored — it prescribes ignoring
/// them, not refusing them; the refusal is ours too. It says nothing
/// against a sub-daily `FREQ` on a DATE.
///
/// Read on the parsed rule, so a part the parser fills is caught whatever
/// its case and wherever it sits. A part with an empty value
/// (`FREQ=DAILY;BYHOUR=`) parses to an empty list, has no effect on either
/// reader, and is accepted. An hour-bound row keeps all of these.
///
/// This closes `POST` and `PATCH`, not every way a row gets there:
///
/// - A row **stored before this check** still unrolls as it did; nothing
///   refuses it on read, and no migration rewrites it. It does get a 400 on
///   any `PATCH` that does not replace its rule — a title, `completed` —
///   because `update_event` validates the merged rule.
/// - A **calendar re-import** rewrites `all_day` from the feed without
///   coming back here (`google_calendar/imports.rs`). An hour-bound imported
///   event given `FREQ=HOURLY;COUNT=5` by `PATCH`, then turned into a
///   `VALUE=DATE` event by the feed, became an all-day row holding that
///   rule, and listed five entries for one reminder again. Not closed here
///   but in the re-import, which drops such a rule from the row it turns
///   all-day (#175, `steps_by_less_than_a_day`).
pub fn validate(
    rrule: &str,
    starts_at: DateTime<Utc>,
    all_day: bool,
) -> Result<(), rrule::RRuleError> {
    if !rrule.is_ascii() || rrule.contains(['\r', '\n', ':']) {
        return Err(rrule::ParseError::InvalidParameterFormat(rrule.into()).into());
    }
    if all_day {
        all_day_rule_set(rrule, starts_at)?;
        let stored = paris_start_of_day(paris_date(starts_at));
        paris_rule_set(rrule, stored)?;
        if steps_by_days(&rrule.parse()?) {
            Ok(())
        } else {
            Err(rrule::ParseError::InvalidParameterFormat(rrule.into()).into())
        }
    } else {
        paris_rule_set(rrule, starts_at).map(|_| ())
    }
}

/// Whether `rule` steps by whole days and names no time of day — what an
/// all-day series has to do to unroll one occurrence per day: see `validate`.
/// Read on the rule as parsed, before `rrule` fills a `BYHOUR` in from the
/// anchor.
fn steps_by_days(rule: &RRule<Unvalidated>) -> bool {
    !matches!(
        rule.get_freq(),
        Frequency::Hourly | Frequency::Minutely | Frequency::Secondly
    ) && rule.get_by_hour().is_empty()
        && rule.get_by_minute().is_empty()
        && rule.get_by_second().is_empty()
}

/// Whether `rrule` parses and does **not** step by days — the rules
/// `validate` refuses on an all-day row for that reason alone (#171). A rule
/// that does not parse is not one of them: it is refused for that.
///
/// For the calendar re-import (#175), which turns rows all-day without
/// coming back through `validate` and drops such a rule rather than store it.
pub fn steps_by_less_than_a_day(rrule: &str) -> bool {
    rrule
        .parse::<RRule<Unvalidated>>()
        .is_ok_and(|rule| !steps_by_days(&rule))
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
        assert!(validate("NOT_A_VALID_RRULE", start, false).is_err());
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
    // expanding on read. The second matters most: `list_events` used to turn
    // an expansion error into a 500 for the *whole* window, so one such row
    // took `/agenda` and the dashboard down with it. Since #161 it renders
    // the row on its own instead — which still loses the series.
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
                validate("FREQ=MONTHLY", anchor, false).is_ok(),
                "anchor {anchor} was rejected at write time"
            );
        }
    }

    // -- the skipped hour, under an hour-bound frequency (#116, round 4) ------
    //
    // Paris skips 02:00-02:59 on 2026-03-29: 01:59 CET (00:59Z) is followed
    // by 03:00 CEST (01:00Z). `rrule` reads a wall clock in the gap with the
    // offset before it, so 02:00 comes back as 01:00Z — which is also what
    // 03:00 names. A daily rule never produces both, but a rule that steps
    // by the hour or less, or lists both hours, produces 02:00 *and* 03:00
    // on that day: the same instant twice, and under `COUNT` a unit of the
    // count spent on the copy. Unrolled in UTC, before #116, no rule could
    // produce a duplicate at all.

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn an_hourly_rule_across_the_skipped_hour_has_no_duplicate_and_keeps_its_count() {
        // 00:00 Paris on 2026-03-29. Four hours on the clock: 00:00, 01:00,
        // (02:00 does not exist), 03:00, 04:00.
        let occs = expand_occurrences(
            "FREQ=HOURLY;COUNT=4",
            utc(2026, 3, 28, 23, 0),
            utc(2026, 3, 28, 0, 0),
            utc(2026, 3, 30, 0, 0),
        )
        .unwrap();
        assert_eq!(
            occs,
            vec![
                utc(2026, 3, 28, 23, 0),
                utc(2026, 3, 29, 0, 0),
                utc(2026, 3, 29, 1, 0),
                utc(2026, 3, 29, 2, 0),
            ]
        );
    }

    #[test]
    fn the_count_spent_on_the_skipped_hour_is_given_back_past_a_window_that_opens_later() {
        // Same series, `COUNT=5`, seen through a window that opens after the
        // gap: the count is spent from `DTSTART`, not from `from`, so the
        // copy made in the gap still has to be given back here.
        let occs = expand_occurrences(
            "FREQ=HOURLY;COUNT=5",
            utc(2026, 3, 28, 23, 0),
            utc(2026, 3, 29, 2, 0),
            utc(2026, 3, 30, 0, 0),
        )
        .unwrap();
        assert_eq!(occs, vec![utc(2026, 3, 29, 2, 0), utc(2026, 3, 29, 3, 0)]);
    }

    #[test]
    fn a_minutely_rule_across_the_skipped_hour_has_no_duplicate_and_keeps_its_count() {
        // 01:00 Paris, every 30 minutes, six times: 01:00, 01:30, then the
        // gap's 02:00 and 02:30 (read as 03:00 and 03:30 CEST), whose copies
        // at 03:00 and 03:30 are the same instants, then 04:00 and 04:30.
        let occs = expand_occurrences(
            "FREQ=MINUTELY;INTERVAL=30;COUNT=6",
            utc(2026, 3, 29, 0, 0),
            utc(2026, 3, 28, 0, 0),
            utc(2026, 3, 30, 0, 0),
        )
        .unwrap();
        assert_eq!(
            occs,
            vec![
                utc(2026, 3, 29, 0, 0),
                utc(2026, 3, 29, 0, 30),
                utc(2026, 3, 29, 1, 0),
                utc(2026, 3, 29, 1, 30),
                utc(2026, 3, 29, 2, 0),
                utc(2026, 3, 29, 2, 30),
            ]
        );
    }

    #[test]
    fn a_daily_rule_listing_both_hours_of_the_gap_has_no_duplicate_and_keeps_its_count() {
        // 02:30 and 03:30 Paris every day, from 2026-03-28. On the 29th the
        // two name the same instant (01:30Z); the fourth distinct occurrence
        // is the 30th's 02:30 (00:30Z, CEST).
        let occs = expand_occurrences(
            "FREQ=DAILY;BYHOUR=2,3;BYMINUTE=30;COUNT=4",
            paris(2026, 3, 28, 2, 30),
            utc(2026, 3, 27, 0, 0),
            utc(2026, 4, 1, 0, 0),
        )
        .unwrap();
        assert_eq!(
            occs,
            vec![
                utc(2026, 3, 28, 1, 30),
                utc(2026, 3, 28, 2, 30),
                utc(2026, 3, 29, 1, 30),
                utc(2026, 3, 30, 0, 30),
            ]
        );
    }

    #[test]
    fn a_rule_whose_skipped_hour_lands_on_no_other_occurrence_keeps_it_an_hour_later() {
        // The witness: where the gap's occurrence does not coincide with
        // another one, it is kept, an hour later on the clock — not dropped.
        // Daily and weekly at 02:30 Paris:
        for (rule, anchor) in [
            ("FREQ=DAILY;COUNT=3", paris(2026, 3, 28, 2, 30)),
            ("FREQ=WEEKLY;COUNT=3", paris(2026, 3, 22, 2, 30)),
        ] {
            let occs = expand_occurrences(rule, anchor, anchor, utc(2026, 4, 30, 0, 0)).unwrap();
            assert_eq!(occs.len(), 3, "{rule}: {occs:?}");
            assert_eq!(
                occs[1],
                utc(2026, 3, 29, 1, 30),
                "{rule}: the 29th's 02:30 is not kept as 03:30 CEST: {occs:?}"
            );
        }
        // …and every two hours from 00:00 Paris: 02:00 lands on 03:00 CEST,
        // which the rule does not otherwise produce.
        let occs = expand_occurrences(
            "FREQ=HOURLY;INTERVAL=2;COUNT=3",
            utc(2026, 3, 28, 23, 0),
            utc(2026, 3, 28, 0, 0),
            utc(2026, 3, 30, 0, 0),
        )
        .unwrap();
        assert_eq!(
            occs,
            vec![
                utc(2026, 3, 28, 23, 0),
                utc(2026, 3, 29, 1, 0),
                utc(2026, 3, 29, 2, 0),
            ]
        );
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

    // -- validate on the all-day anchor (#161) -------------------------------
    //
    // An all-day row is unrolled on a UTC-midnight stand-in for its Paris
    // date, not on the Paris midnight it stores (22:00Z or 23:00Z the day
    // before). A rule validated on the stored instant but unrolled on the
    // stand-in was accepted on write and failed on read — a 201, then a 500
    // on the whole window. Write has to refuse what the unroll refuses.

    #[test]
    fn an_all_day_rule_until_the_day_before_is_refused_on_write() {
        // The reproduction from #161: an all-day event on 2026-09-05, stored
        // at 2026-09-04T22:00Z, with what `build_rrule` writes for
        // « Jusqu'au 2026-09-04 ».
        assert!(validate(
            "FREQ=DAILY;UNTIL=20260904T235959Z",
            midnight(2026, 9, 5),
            true
        )
        .is_err());
    }

    #[test]
    fn an_all_day_rule_until_its_own_day_is_accepted_on_write() {
        assert!(validate(
            "FREQ=DAILY;UNTIL=20260905T235959Z",
            midnight(2026, 9, 5),
            true
        )
        .is_ok());
    }

    #[test]
    fn an_hour_bound_rule_is_still_validated_on_its_own_instant() {
        // Same instant, same rule, not all-day: it unrolls from
        // 2026-09-04T22:00Z itself, which the UNTIL does not precede.
        assert!(validate(
            "FREQ=DAILY;UNTIL=20260904T235959Z",
            midnight(2026, 9, 5),
            false
        )
        .is_ok());
    }

    #[test]
    fn validate_accepts_exactly_the_all_day_rules_the_unroll_accepts() {
        // Anchors on both offsets, and UNTILs on each side of both the stored
        // instant and the stand-in — the two anchors part between them.
        let anchors = [midnight(2026, 9, 5), midnight(2026, 12, 5)];
        let rules = [
            "FREQ=DAILY",
            "FREQ=WEEKLY;BYDAY=SA;COUNT=3",
            "FREQ=DAILY;UNTIL=20260904T215959Z",
            "FREQ=DAILY;UNTIL=20260904T220000Z",
            "FREQ=DAILY;UNTIL=20260904T230000Z",
            "FREQ=DAILY;UNTIL=20260904T235959Z",
            "FREQ=DAILY;UNTIL=20260905T000000Z",
            "FREQ=DAILY;UNTIL=20260905T235959Z",
            "FREQ=MONTHLY;UNTIL=20261204T225959Z",
            "FREQ=MONTHLY;UNTIL=20261204T230000Z",
            "FREQ=MONTHLY;UNTIL=20261204T235959Z",
            "FREQ=MONTHLY;UNTIL=20261205T000000Z",
            "NOT_A_VALID_RRULE",
        ];
        for anchor in anchors {
            for rule in rules {
                let written = validate(rule, anchor, true).is_ok();
                let read = expand_all_day_occurrences(
                    rule,
                    anchor,
                    anchor + Duration::days(1),
                    midnight(2026, 1, 1),
                    midnight(2027, 12, 31),
                )
                .is_ok();
                assert_eq!(
                    written, read,
                    "{rule} from {anchor}: accepted on write = {written}, unrolls on read = {read}"
                );
            }
        }
    }

    // -- a rule is one line, and every reader unrolls it (#165) --------------
    //
    // At #165 an all-day series had two readers building its rule set two
    // ways: `list_events` wrapped the rule in `all_day_rule_set`'s text,
    // `refill_notifications` parsed the rule alone through
    // `expand_occurrences`, from the instant the row stores. A rule carrying
    // a line break injected its own `EXDATE:`/`RDATE:`/`DTSTART:` lines into
    // the first and was refused by the second: a 201 on write, then a 500 on
    // `POST /reminders` and an error logged by the reminders job at every
    // pass. Since #169 both readers go through `expand_series`, i.e.
    // `all_day_rule_set` for an all-day row; `validate` still checks both
    // constructions (see its doc), and these tests pin that.

    /// Rules that bring lines of their own — through `\n`, `\r\n`, or a bare
    /// `\r`, which `str::lines` strips on one reader and not on the other.
    const MULTI_LINE_RULES: [&str; 8] = [
        "FREQ=WEEKLY\nEXDATE:20260927T000000Z",
        "FREQ=WEEKLY\nRDATE:20260927T000000Z",
        "FREQ=WEEKLY\nDTSTART:20260101T000000Z",
        "FREQ=WEEKLY\nRRULE:FREQ=DAILY",
        "FREQ=WEEKLY\r\nEXDATE:20260927T000000Z",
        "FREQ=WEEKLY\rEXDATE:20260927T000000Z",
        "FREQ=WEEKLY\r",
        "FREQ=WEEKLY\n",
    ];

    #[test]
    fn a_rule_with_a_line_break_is_refused_on_write_all_day() {
        for rule in MULTI_LINE_RULES {
            assert!(
                validate(rule, midnight(2026, 9, 5), true).is_err(),
                "{rule:?} accepted on an all-day row"
            );
        }
    }

    #[test]
    fn a_rule_with_a_line_break_is_refused_on_write_hour_bound() {
        // `FREQ=WEEKLY\nRRULE:FREQ=DAILY` parses on this path too — as
        // `FREQ=DAILY`, the line after the break — so the check cannot be
        // left to the parser.
        for rule in MULTI_LINE_RULES {
            assert!(
                validate(rule, utc(2026, 9, 5, 9, 0), false).is_err(),
                "{rule:?} accepted on an hour-bound row"
            );
        }
    }

    #[test]
    fn an_all_day_rule_the_hour_bound_construction_cannot_unroll_is_refused_on_write() {
        // One line, no break: `all_day_rule_set` reads everything after
        // `RRULE:` and takes `X:MO` for a Monday; `expand_occurrences` finds
        // a `:` in a line with no property name and parses `MO` alone.
        // `expand_occurrences` was the reminders' reader of an all-day row
        // until #169; it is still the construction `validate` checks.
        let rule = "FREQ=WEEKLY;BYDAY=X:MO";
        let stored = midnight(2026, 9, 5);
        assert!(
            expand_occurrences(rule, stored, stored, stored + Duration::days(30)).is_err(),
            "the premise: the hour-bound construction refuses {rule}"
        );
        assert!(validate(rule, stored, true).is_err());
    }

    #[test]
    fn validate_accepts_exactly_the_all_day_rules_both_constructions_accept() {
        // What this compares: acceptance on write against acceptance by the
        // all-day unroll (`listed`, what both readers use since #169) **and**
        // by the hour-bound construction from the stored instant
        // (`hour_bound`, the reminders' reader of an all-day row until #169,
        // still checked by `validate`). It does not compare two live readers
        // any more.
        //
        // Checked from the instant the client sends as well as from the one
        // the row stores: `validate` runs before `normalize_all_day`.
        let rules = [
            "FREQ=DAILY",
            "FREQ=WEEKLY;BYDAY=SA;COUNT=3",
            "FREQ=DAILY;UNTIL=20260904T215959Z",
            "FREQ=DAILY;UNTIL=20260904T235959Z",
            "FREQ=DAILY;UNTIL=20260905T235959Z",
            "FREQ=MONTHLY;UNTIL=20261204T235959Z",
            "FREQ=DAILY;UNTIL=20261010",
            ";FREQ=DAILY",
            "NOT_A_VALID_RRULE",
        ];
        let window = (midnight(2026, 1, 1), midnight(2027, 12, 31));
        for rule in rules
            .iter()
            .chain(MULTI_LINE_RULES.iter())
            .chain(COLON_RULES.iter())
        {
            for stored in [midnight(2026, 9, 5), midnight(2026, 12, 5)] {
                let listed = expand_all_day_occurrences(
                    rule,
                    stored,
                    stored + Duration::days(1),
                    window.0,
                    window.1,
                )
                .is_ok();
                let hour_bound = expand_occurrences(rule, stored, window.0, window.1).is_ok();
                let one_value = !rule.contains(['\r', '\n', ':']);
                for sent in [stored, stored + Duration::hours(12)] {
                    let written = validate(rule, sent, true).is_ok();
                    assert_eq!(
                        written,
                        listed && hour_bound && one_value,
                        "{rule:?} sent at {sent}: accepted on write = {written}, \
                         listed = {listed}, hour_bound = {hour_bound}"
                    );
                }
            }
        }
    }

    #[test]
    fn validate_accepts_exactly_the_hour_bound_rules_the_unroll_accepts() {
        let rules = [
            "FREQ=DAILY",
            "FREQ=DAILY;UNTIL=20260905T085959Z",
            "FREQ=DAILY;UNTIL=20260905T090000Z",
            "NOT_A_VALID_RRULE",
        ];
        let start = utc(2026, 9, 5, 9, 0);
        for rule in rules
            .iter()
            .chain(MULTI_LINE_RULES.iter())
            .chain(COLON_RULES.iter())
        {
            let written = validate(rule, start, false).is_ok();
            let read = expand_occurrences(rule, start, start, start + Duration::days(30)).is_ok();
            let one_value = !rule.contains(['\r', '\n', ':']);
            assert_eq!(
                written,
                read && one_value,
                "{rule:?}: accepted on write = {written}, unrolls = {read}"
            );
        }
    }

    // -- a rule holds no `:` (#165) -------------------------------------------
    //
    // A `:` is how a content line separates a property name from its value;
    // an RRULE value (RFC 5545 §3.3.10) never holds one. The two
    // constructions do not agree on what to do with it: `all_day_rule_set`
    // keeps the rule whole after its own `RRULE:`, `expand_occurrences` keeps
    // only what follows the first `:`. At #165 they were the agenda's and the
    // reminders' readers of an all-day row (both use the first since #169):
    // on one line, with no break, a rule could still be read two ways, or
    // stored as written and unrolled as something else.

    /// One-line rules carrying a `:`. Some are refused by one construction, some
    /// accepted by both and read differently, some accepted as written and
    /// unrolled as their tail.
    const COLON_RULES: [&str; 6] = [
        "BYDAY=1:WKST=MO;FREQ=WEEKLY",
        "FREQ=WEEKLY;X:FREQ=DAILY",
        "RRULE:FREQ=DAILY",
        "EXRULE:FREQ=DAILY",
        "FREQ=WEEKLY;BYDAY=X:MO",
        "FREQ=WEEKLY;BYDAY=MO:",
    ];

    #[test]
    fn the_two_constructions_part_on_a_rule_holding_a_colon() {
        // The premise of the refusal, measured on both examples from the
        // verification of #166. `hour_bound` is what the reminders read for
        // an all-day row until #169; since then they read `listed`.
        let saturday = midnight(2026, 9, 5);
        assert_eq!(paris_date(saturday).weekday(), Weekday::Sat);
        let rule = "BYDAY=1:WKST=MO;FREQ=WEEKLY";
        let window = (saturday, saturday + Duration::days(21));
        let listed: Vec<Weekday> = expand_all_day_occurrences(
            rule,
            saturday,
            saturday + Duration::days(1),
            window.0,
            window.1,
        )
        .unwrap()
        .into_iter()
        .map(|(start, _)| paris_date(start).weekday())
        .collect();
        let hour_bound: Vec<Weekday> = expand_occurrences(rule, saturday, window.0, window.1)
            .unwrap()
            .into_iter()
            .map(|start| paris_date(start).weekday())
            .collect();
        assert!(
            !listed.is_empty() && listed.iter().all(|d| *d == Weekday::Mon),
            "{listed:?}"
        );
        assert!(
            !hour_bound.is_empty() && hour_bound.iter().all(|d| *d == Weekday::Sat),
            "{hour_bound:?}"
        );

        // Hour-bound: stored as written, unrolled as its tail, every day.
        let start = utc(2026, 9, 5, 9, 0);
        let daily = expand_occurrences(
            "FREQ=WEEKLY;X:FREQ=DAILY",
            start,
            start,
            start + Duration::days(6),
        )
        .unwrap();
        assert_eq!(daily.len(), 7, "{daily:?}");
    }

    #[test]
    fn a_rule_with_a_colon_is_refused_on_write_all_day() {
        for rule in COLON_RULES {
            for sent in [midnight(2026, 9, 5), midnight(2026, 12, 5)] {
                assert!(
                    validate(rule, sent, true).is_err(),
                    "{rule:?} accepted on an all-day row"
                );
            }
        }
    }

    #[test]
    fn a_rule_with_a_colon_is_refused_on_write_hour_bound() {
        // `RRULE:FREQ=DAILY` was accepted here before, and unrolled as
        // `FREQ=DAILY`; it is refused now, like every other `:`.
        for rule in COLON_RULES {
            assert!(
                validate(rule, utc(2026, 9, 5, 9, 0), false).is_err(),
                "{rule:?} accepted on an hour-bound row"
            );
        }
    }

    // -- a rule is ASCII (#170) -----------------------------------------------
    //
    // `rrule` 0.14 cuts a `BYDAY` entry two bytes before its end
    // (`NWeekday::from_str`): past a multibyte character that cut lands
    // inside it, and the parser panics instead of returning an error. The
    // panic went up through the handler, and the client got no response.

    /// Non-ASCII rules. The first three made `rrule` panic at #170; the
    /// others carry a non-ASCII character the parser refused without one.
    const NON_ASCII_RULES: [&str; 6] = [
        "FREQ=WEEKLY;BYDAY=éA",
        "FREQ=WEEKLY;BYDAY=MO,éA",
        "FREQ=MONTHLY;BYDAY=1éA",
        "FREQ=WEEKLY;BYDAY=MOé",
        "FREQ=WEEKLÝ",
        "FREQ=WEEKLY;COUNT=５",
    ];

    /// Whether `validate` refuses `rule`, a panic being reported as one
    /// rather than as a failed assertion.
    fn refused_without_panic(rule: &str, starts_at: DateTime<Utc>, all_day: bool) -> bool {
        std::panic::catch_unwind(|| validate(rule, starts_at, all_day))
            .unwrap_or_else(|_| panic!("{rule:?} panicked, all_day = {all_day}"))
            .is_err()
    }

    #[test]
    fn a_non_ascii_rule_is_refused_on_write_all_day() {
        for rule in NON_ASCII_RULES {
            assert!(
                refused_without_panic(rule, midnight(2026, 9, 5), true),
                "{rule:?} accepted on an all-day row"
            );
        }
    }

    #[test]
    fn a_non_ascii_rule_is_refused_on_write_hour_bound() {
        for rule in NON_ASCII_RULES {
            assert!(
                refused_without_panic(rule, utc(2026, 9, 5, 9, 0), false),
                "{rule:?} accepted on an hour-bound row"
            );
        }
    }

    #[test]
    fn an_ascii_byday_is_still_accepted_on_write() {
        let anchors = [(midnight(2026, 9, 5), true), (utc(2026, 9, 5, 9, 0), false)];
        for (starts_at, all_day) in anchors {
            for rule in ["FREQ=WEEKLY;BYDAY=MO,SA", "FREQ=MONTHLY;BYDAY=-1FR"] {
                assert!(
                    validate(rule, starts_at, all_day).is_ok(),
                    "{rule:?} refused, all_day = {all_day}"
                );
            }
        }
    }

    #[test]
    fn validate_checks_the_hour_bound_construction_from_the_instant_normalize_all_day_stores() {
        // `validate` runs before `normalize_all_day` and rebuilds the stored
        // anchor itself. Pinned on the edges of a Paris day and across both
        // changes of hour, where an offset slip would move it by a day.
        use manage_our_home_shared::validation::agenda::normalize_all_day;
        let instants = [
            utc(2026, 1, 1, 0, 0),
            utc(2026, 3, 28, 22, 59) + Duration::seconds(59),
            utc(2026, 3, 28, 23, 0),
            utc(2026, 3, 29, 0, 59) + Duration::seconds(59),
            utc(2026, 3, 29, 1, 0),
            utc(2026, 3, 29, 21, 59) + Duration::seconds(59),
            utc(2026, 3, 29, 22, 0),
            utc(2026, 9, 4, 21, 59) + Duration::seconds(59),
            utc(2026, 9, 4, 22, 0),
            utc(2026, 10, 24, 21, 59) + Duration::seconds(59),
            utc(2026, 10, 24, 22, 0),
            utc(2026, 10, 25, 0, 30),
            utc(2026, 10, 25, 1, 30),
            utc(2026, 10, 25, 22, 59) + Duration::seconds(59),
            utc(2026, 10, 25, 23, 0),
            utc(2026, 12, 31, 22, 59) + Duration::seconds(59),
            utc(2026, 12, 31, 23, 0),
        ];
        for sent in instants {
            assert_eq!(
                paris_start_of_day(paris_date(sent)),
                normalize_all_day(sent, sent + Duration::hours(1)).0,
                "{sent}"
            );
        }
    }

    // -- an all-day rule steps by days (#171) ---------------------------------
    //
    // An all-day series is unrolled on civil dates: `expand_all_day_occurrences`
    // keeps the date of each occurrence and drops its time. A rule that steps
    // by the hour or less, or names a time of day, has nothing left once the
    // time is dropped but copies of the same day: `FREQ=HOURLY;COUNT=5` was
    // listed five times on its one day, and reminded once, the reminder rows
    // being keyed by instant. Unbounded, `FREQ=MINUTELY` spent the whole
    // `MAX_OCCURRENCES` on its first day and the window saw no other.

    /// All-day rules that step by less than a day, or name a time of day —
    /// in either case, and wherever the part sits.
    const TIME_OF_DAY_RULES: [&str; 12] = [
        "FREQ=HOURLY",
        "FREQ=HOURLY;COUNT=5",
        "FREQ=MINUTELY",
        "FREQ=SECONDLY;COUNT=3",
        "COUNT=5;FREQ=HOURLY;INTERVAL=24",
        "freq=hourly",
        "FREQ=DAILY;BYHOUR=12;COUNT=3",
        "FREQ=DAILY;BYHOUR=0",
        "FREQ=WEEKLY;BYDAY=SA;BYMINUTE=30",
        "FREQ=MONTHLY;BYSECOND=0",
        "BYMINUTE=0;FREQ=YEARLY",
        "FREQ=DAILY;byhour=12",
    ];

    #[test]
    fn an_all_day_rule_stepping_by_less_than_a_day_or_naming_a_time_is_refused_on_write() {
        for rule in TIME_OF_DAY_RULES {
            for stored in [midnight(2026, 9, 5), midnight(2026, 12, 5)] {
                for sent in [stored, stored + Duration::hours(12)] {
                    assert!(
                        validate(rule, sent, true).is_err(),
                        "{rule:?} sent at {sent} accepted on an all-day row"
                    );
                }
            }
        }
    }

    #[test]
    fn an_hour_bound_rule_may_still_step_by_the_hour_or_name_a_time() {
        for rule in TIME_OF_DAY_RULES {
            assert!(
                validate(rule, utc(2026, 9, 5, 9, 0), false).is_ok(),
                "{rule:?} refused on an hour-bound row"
            );
        }
    }

    #[test]
    fn an_all_day_rule_stepping_by_days_is_still_accepted_on_write() {
        let rules = [
            "FREQ=DAILY",
            "FREQ=DAILY;INTERVAL=2;COUNT=4",
            "FREQ=WEEKLY;BYDAY=MO,SA",
            "FREQ=MONTHLY;BYMONTHDAY=5",
            "FREQ=MONTHLY;BYDAY=-1FR",
            "FREQ=YEARLY;BYMONTH=9;BYMONTHDAY=5",
            "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1",
            "FREQ=DAILY;UNTIL=20261010T235959Z",
        ];
        for rule in rules {
            assert!(
                validate(rule, midnight(2026, 9, 5), true).is_ok(),
                "{rule:?} refused on an all-day row"
            );
        }
    }

    // -- the cap counts what the window keeps (#119) --------------------------
    //
    // Both readers cap an unroll at `MAX_OCCURRENCES`, so a window far wider
    // than the cap comes back truncated rather than unbounded. The all-day
    // path used to spend part of that budget outside the window it was asked
    // for: it unrolled over a window widened by a day on each side and let
    // `RRuleSet::all` cap the widened result, so a series with an occurrence
    // on the extra day before `from` — any series started before the window —
    // came back one occurrence short, 999 instead of 1000, and the missing
    // one was the *last*, dropped in silence at the far end.

    /// A daily all-day series whose first day is `lead_days` before the
    /// window, listed over a window of `window_days` civil days.
    fn all_day_days_in_window(lead_days: i64, window_days: i64) -> Vec<NaiveDate> {
        let first_day = day(2025, 1, 1);
        let from_day = add_days(first_day, lead_days);
        expand_all_day_occurrences(
            "FREQ=DAILY",
            paris_start_of_day(first_day),
            paris_start_of_day(add_days(first_day, 1)),
            paris_start_of_day(from_day),
            paris_start_of_day(add_days(from_day, window_days - 1)),
        )
        .unwrap()
        .into_iter()
        .map(|(start, _)| paris_date(start))
        .collect()
    }

    #[test]
    fn an_all_day_window_is_filled_up_to_the_cap_whatever_precedes_it() {
        let cap = i64::from(MAX_OCCURRENCES);
        for lead_days in [0, 1, 400] {
            let from_day = add_days(day(2025, 1, 1), lead_days);
            // A window exactly the size of the cap is filled, to its last day.
            let days = all_day_days_in_window(lead_days, cap);
            assert_eq!(days.len(), usize::from(MAX_OCCURRENCES), "lead {lead_days}");
            assert_eq!(days[0], from_day, "lead {lead_days}");
            assert_eq!(
                days[days.len() - 1],
                add_days(from_day, cap - 1),
                "lead {lead_days}"
            );
            // A wider one is truncated at the cap, from the start of the window.
            let days = all_day_days_in_window(lead_days, cap + 500);
            assert_eq!(days.len(), usize::from(MAX_OCCURRENCES), "lead {lead_days}");
            assert_eq!(days[0], from_day, "lead {lead_days}");
            assert_eq!(
                days[days.len() - 1],
                add_days(from_day, cap - 1),
                "lead {lead_days}"
            );
        }
    }

    #[test]
    fn the_all_day_path_caps_where_the_hour_bound_one_does() {
        let cap = i64::from(MAX_OCCURRENCES);
        let first_day = day(2025, 1, 1);
        let from_day = add_days(first_day, 400);
        for window_days in [cap - 1, cap, cap + 1, cap + 500] {
            let hour_bound = expand_occurrences(
                "FREQ=DAILY",
                paris_start_of_day(first_day) + Duration::hours(9),
                paris_start_of_day(from_day),
                paris_start_of_day(add_days(from_day, window_days - 1)) + Duration::hours(23),
            )
            .unwrap();
            assert_eq!(
                all_day_days_in_window(400, window_days).len(),
                hour_bound.len(),
                "window of {window_days} days"
            );
        }
    }
}
