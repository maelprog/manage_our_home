//! Pure, dependency-light logic for the Agenda screens (front epic #3,
//! issue #18), shared by `apps/web`'s SSR pages. Written test-first per
//! `.claude/CLAUDE.md`'s TDD process. Four concerns live here so the UI and
//! the fixed backend can never drift.
//!
//! `build_rrule`/`parse_rrule` are the **RRULE v1 subset** builder/parser:
//! the picker exposes only a bounded slice of RFC 5545 (see the epic spec on
//! #18), and these two functions are the single source of truth for the
//! string round-trip.
//!
//! `validate_event_form` mirrors `apps/api`'s create/update guards
//! (non-empty title, `ends_at >= starts_at`) so the form rejects before a
//! round trip.
//!
//! `validate_attachment` is the client-side pre-check (extension allow-list
//! and size cap) `architecture.md`'s Uploads section asks for *before*
//! upload; the backend still sniffs the real MIME type authoritatively.
//!
//! `month_grid`/`week_days` are the civil-date maths behind the hand-rolled
//! calendar (no calendar library, the accepted Leptos trade-off).

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Europe::Paris;

// ---------------------------------------------------------------------------
// RRULE v1 subset
// ---------------------------------------------------------------------------

/// The recurrence frequencies the v1 picker exposes. Sub-daily frequencies
/// (`SECONDLY`/`MINUTELY`/`HOURLY`) are deliberately excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// How a recurrence stops: open-ended, after a fixed number of occurrences
/// (`COUNT`), or on a calendar date (`UNTIL`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecurrenceEnd {
    Never,
    Count(u32),
    Until(NaiveDate),
}

/// The v1 recurrence model. `byday` is only meaningful for `Freq::Weekly`
/// and is kept empty otherwise. Anything the picker can't express (nth
/// weekday of month, `BYSETPOS`, `BYMONTH`, `RDATE`/`EXDATE`, …) has no
/// representation here on purpose — `parse_rrule` returns `None` for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recurrence {
    pub freq: Freq,
    pub interval: u32,
    pub byday: Vec<Weekday>,
    pub end: RecurrenceEnd,
}

/// Canonical Monday→Sunday order, used both to sort `BYDAY` on build and to
/// render the weekday checkboxes.
pub const WEEKDAYS: [Weekday; 7] = [
    Weekday::Mon,
    Weekday::Tue,
    Weekday::Wed,
    Weekday::Thu,
    Weekday::Fri,
    Weekday::Sat,
    Weekday::Sun,
];

fn weekday_token(w: Weekday) -> &'static str {
    match w {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

fn weekday_from_token(tok: &str) -> Option<Weekday> {
    match tok {
        "MO" => Some(Weekday::Mon),
        "TU" => Some(Weekday::Tue),
        "WE" => Some(Weekday::Wed),
        "TH" => Some(Weekday::Thu),
        "FR" => Some(Weekday::Fri),
        "SA" => Some(Weekday::Sat),
        "SU" => Some(Weekday::Sun),
        _ => None,
    }
}

fn freq_token(f: Freq) -> &'static str {
    match f {
        Freq::Daily => "DAILY",
        Freq::Weekly => "WEEKLY",
        Freq::Monthly => "MONTHLY",
        Freq::Yearly => "YEARLY",
    }
}

/// Builds the RRULE string (without the `DTSTART`/`RRULE:` prefix — the
/// backend derives `DTSTART` from `starts_at`) for a v1 `Recurrence`.
/// `INTERVAL=1` is omitted (it's the default); `BYDAY` is emitted only for
/// a weekly rule with at least one day, sorted Monday→Sunday.
pub fn build_rrule(r: &Recurrence) -> String {
    let mut parts = vec![format!("FREQ={}", freq_token(r.freq))];
    if r.interval > 1 {
        parts.push(format!("INTERVAL={}", r.interval));
    }
    if r.freq == Freq::Weekly && !r.byday.is_empty() {
        let mut days: Vec<Weekday> = r.byday.clone();
        days.sort_by_key(|w| WEEKDAYS.iter().position(|x| x == w).unwrap());
        days.dedup();
        let tokens: Vec<&str> = days.iter().map(|w| weekday_token(*w)).collect();
        parts.push(format!("BYDAY={}", tokens.join(",")));
    }
    match &r.end {
        RecurrenceEnd::Never => {}
        RecurrenceEnd::Count(n) => parts.push(format!("COUNT={n}")),
        RecurrenceEnd::Until(d) => {
            parts.push(format!("UNTIL={}T235959Z", d.format("%Y%m%d")));
        }
    }
    parts.join(";")
}

/// Parses an RRULE string back into the v1 model, or `None` if it uses any
/// feature outside the v1 subset (so the caller can fall back to read-only
/// display without corrupting the stored rule). Unknown keys, sub-daily
/// frequencies, `BYDAY` on a non-weekly rule, nth-weekday tokens (`3TU`),
/// and a rule carrying both `COUNT` and `UNTIL` are all rejected.
pub fn parse_rrule(rrule: &str) -> Option<Recurrence> {
    let mut freq: Option<Freq> = None;
    let mut interval: u32 = 1;
    let mut byday: Vec<Weekday> = Vec::new();
    let mut count: Option<u32> = None;
    let mut until: Option<NaiveDate> = None;

    for pair in rrule.split(';').filter(|s| !s.is_empty()) {
        let (key, value) = pair.split_once('=')?;
        match key.to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = Some(match value.to_ascii_uppercase().as_str() {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    _ => return None, // sub-daily / unknown → outside v1
                });
            }
            "INTERVAL" => {
                let n: u32 = value.parse().ok()?;
                if n < 1 {
                    return None;
                }
                interval = n;
            }
            "BYDAY" => {
                for tok in value.split(',') {
                    // Reject nth-weekday tokens like `3TU`/`-1FR`: v1 only
                    // supports bare weekday codes.
                    byday.push(weekday_from_token(&tok.to_ascii_uppercase())?);
                }
            }
            "COUNT" => count = Some(value.parse().ok()?),
            "UNTIL" => {
                let date_part = value.get(0..8)?;
                until = Some(NaiveDate::parse_from_str(date_part, "%Y%m%d").ok()?);
            }
            _ => return None, // BYMONTH/BYMONTHDAY/BYSETPOS/WKST/... → outside v1
        }
    }

    let freq = freq?;
    if !byday.is_empty() && freq != Freq::Weekly {
        return None;
    }
    let end = match (count, until) {
        (Some(_), Some(_)) => return None,
        (Some(n), None) => RecurrenceEnd::Count(n),
        (None, Some(d)) => RecurrenceEnd::Until(d),
        (None, None) => RecurrenceEnd::Never,
    };
    Some(Recurrence {
        freq,
        interval,
        byday,
        end,
    })
}

// ---------------------------------------------------------------------------
// Event form validation (mirror of apps/api's create/update guards)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventFormError {
    TitleRequired,
    EndsBeforeStarts,
}

/// Mirrors `apps/api/src/agenda/events.rs`: a non-empty title and
/// `ends_at >= starts_at`. `starts_at`/`ends_at` are already resolved to
/// UTC instants by the caller (naive-local → Europe/Paris → UTC happens in
/// `apps/web`); this stays timezone-agnostic on purpose.
pub fn validate_event_form(
    title: &str,
    starts_at: chrono::DateTime<chrono::Utc>,
    ends_at: chrono::DateTime<chrono::Utc>,
) -> Result<(), EventFormError> {
    if title.trim().is_empty() {
        return Err(EventFormError::TitleRequired);
    }
    if ends_at < starts_at {
        return Err(EventFormError::EndsBeforeStarts);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// All-day normalization
// ---------------------------------------------------------------------------

/// Europe/Paris wall-clock midnight opening `date`, as a UTC instant.
///
/// Paris shifts its clocks at 02:00/03:00 local, so midnight is never
/// skipped nor repeated and `earliest()` always resolves. The remaining
/// arms keep the function total rather than panicking on a hypothetical
/// the tz database would have to invent.
fn paris_start_of_day(date: NaiveDate) -> DateTime<Utc> {
    let naive = date.and_time(NaiveTime::MIN);
    let local = Paris.from_local_datetime(&naive);
    local
        .earliest()
        .or_else(|| local.latest())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|| naive.and_utc())
}

/// The timestamps an `all_day` event must be stored with: Paris midnight
/// opening its first day, to Paris midnight opening the day *after* its
/// last one.
///
/// Nothing enforced this before (#101). `all_day` was a display flag and
/// nothing else, so an event ticked "journée entière" kept whatever the two
/// `datetime-local` fields held — and the fields' own defaults are "now" and
/// "now + 1 h". The dashboard keeps occurrences whose `occurrence_ends_at`
/// is still ahead (`apps/web/src/routes/home.rs`, #73), so a birthday
/// created at 08:00 → 09:00 vanished from it at 09:01 while still being,
/// to the reader, an event happening today.
///
/// **The end is exclusive**, i.e. the midnight that opens the next day, not
/// 23:59. Three reasons: it is RFC 5545's own convention for a DATE-valued
/// `DTEND` (so an ICS feed carrying one already agrees with us); it makes
/// the event's duration exactly the civil day, DST included — 23 h on the
/// spring-forward day, 25 h on the fall-back one, which a fixed `+24 h`
/// would get wrong twice a year; and it leaves no dead minute between
/// 23:59 and midnight during which a still-current event reads as finished.
///
/// The same convention makes the function **idempotent**: an end already
/// sitting on a Paris midnight is read as the exclusive end it is, rather
/// than being pushed one more day out. That matters because `update_event`
/// re-normalizes on *every* PATCH — including ones that touch neither
/// timestamp — so a non-idempotent version would grow an event by a day
/// each time somebody edited its title.
///
/// Europe/Paris, not the caller's zone: it is the fixed v1 display timezone
/// (F3's `DISPLAY_TZ`), the one the forms parse into and every page renders
/// back from. There is no per-family timezone in v1.
pub fn normalize_all_day(
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let start_day = starts_at.with_timezone(&Paris).date_naive();
    let end_paris = ends_at.with_timezone(&Paris);
    let end_day = end_paris.date_naive();

    // An end already on Paris midnight names the first day *past* the
    // event; any other time of day is inside the last day it covers, which
    // therefore has to be included.
    let mut exclusive_end_day = if end_paris.time() == NaiveTime::MIN && end_day > start_day {
        end_day
    } else {
        end_day.succ_opt().unwrap_or(end_day)
    };
    // A backwards or same-instant pair still yields one whole day. The API
    // rejects `ends_at < starts_at` before ever calling this, but the
    // invariant "an all-day event lasts at least a day" belongs here, not
    // in the caller.
    let first_day_after_start = start_day.succ_opt().unwrap_or(start_day);
    if exclusive_end_day < first_day_after_start {
        exclusive_end_day = first_day_after_start;
    }

    (
        paris_start_of_day(start_day),
        paris_start_of_day(exclusive_end_day),
    )
}

// ---------------------------------------------------------------------------
// Attachment pre-validation (extension allow-list + size cap)
// ---------------------------------------------------------------------------

/// Mirror of `apps/api/src/storage.rs::MAX_ATTACHMENT_SIZE_BYTES` (20 MiB).
pub const MAX_ATTACHMENT_SIZE_BYTES: u64 = 20 * 1024 * 1024;

/// Extensions matching the backend's sniffed MIME allow-list
/// (`image/png`, `image/jpeg`, `image/webp`, `application/pdf`). This is a
/// cheap pre-check only — the backend still sniffs the real bytes and is
/// the authority; a `.png`-renamed executable is caught there, not here.
pub const ALLOWED_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "pdf"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentError {
    UnsupportedType,
    TooLarge,
}

/// Client-side pre-upload check: the filename's extension must be in the
/// allow-list and the size must not exceed the cap. Extension match is
/// case-insensitive.
pub fn validate_attachment(filename: &str, size_bytes: u64) -> Result<(), AttachmentError> {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    if !ALLOWED_EXTENSIONS.contains(&ext.as_str()) {
        return Err(AttachmentError::UnsupportedType);
    }
    if size_bytes > MAX_ATTACHMENT_SIZE_BYTES {
        return Err(AttachmentError::TooLarge);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Calendar grid maths (hand-rolled month/week views)
// ---------------------------------------------------------------------------

/// The 42 civil dates (6 weeks × 7 days, Monday-first) of the month grid
/// containing `(year, month)`: the month's days plus the leading days of the
/// previous month and trailing days of the next month needed to fill whole
/// weeks. Returns a flat `Vec` the caller chunks into rows of 7. `month` is
/// 1–12; an out-of-range month yields an empty grid.
pub fn month_grid(year: i32, month: u32) -> Vec<NaiveDate> {
    let Some(first) = NaiveDate::from_ymd_opt(year, month, 1) else {
        return Vec::new();
    };
    // Back up to the Monday on or before the 1st.
    let offset = first.weekday().num_days_from_monday();
    let start = first - chrono::Duration::days(offset as i64);
    (0..42).map(|i| start + chrono::Duration::days(i)).collect()
}

/// The 7 civil dates (Monday→Sunday) of the week containing `date`.
pub fn week_days(date: NaiveDate) -> Vec<NaiveDate> {
    let offset = date.weekday().num_days_from_monday();
    let monday = date - chrono::Duration::days(offset as i64);
    (0..7).map(|i| monday + chrono::Duration::days(i)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- build_rrule / parse_rrule round-trip --------------------------------

    fn round_trip(r: &Recurrence) {
        let s = build_rrule(r);
        assert_eq!(
            parse_rrule(&s).as_ref(),
            Some(r),
            "round trip failed for {s}"
        );
    }

    #[test]
    fn daily_never() {
        let r = Recurrence {
            freq: Freq::Daily,
            interval: 1,
            byday: vec![],
            end: RecurrenceEnd::Never,
        };
        assert_eq!(build_rrule(&r), "FREQ=DAILY");
        round_trip(&r);
    }

    #[test]
    fn interval_greater_than_one_is_emitted_and_one_is_omitted() {
        let every_three = Recurrence {
            freq: Freq::Daily,
            interval: 3,
            byday: vec![],
            end: RecurrenceEnd::Never,
        };
        assert_eq!(build_rrule(&every_three), "FREQ=DAILY;INTERVAL=3");
        round_trip(&every_three);

        let every_one = Recurrence {
            freq: Freq::Weekly,
            interval: 1,
            byday: vec![Weekday::Mon],
            end: RecurrenceEnd::Never,
        };
        assert!(!build_rrule(&every_one).contains("INTERVAL"));
    }

    #[test]
    fn weekly_byday_sorted_monday_first_and_deduped() {
        let r = Recurrence {
            freq: Freq::Weekly,
            interval: 1,
            byday: vec![Weekday::Fri, Weekday::Mon, Weekday::Wed, Weekday::Mon],
            end: RecurrenceEnd::Never,
        };
        assert_eq!(build_rrule(&r), "FREQ=WEEKLY;BYDAY=MO,WE,FR");
        // Parses back deduped/sorted, so compare against the normalized form.
        let normalized = Recurrence {
            byday: vec![Weekday::Mon, Weekday::Wed, Weekday::Fri],
            ..r
        };
        round_trip(&normalized);
    }

    #[test]
    fn weekly_with_count() {
        let r = Recurrence {
            freq: Freq::Weekly,
            interval: 2,
            byday: vec![Weekday::Tue],
            end: RecurrenceEnd::Count(10),
        };
        assert_eq!(build_rrule(&r), "FREQ=WEEKLY;INTERVAL=2;BYDAY=TU;COUNT=10");
        round_trip(&r);
    }

    #[test]
    fn monthly_with_until() {
        let r = Recurrence {
            freq: Freq::Monthly,
            interval: 1,
            byday: vec![],
            end: RecurrenceEnd::Until(NaiveDate::from_ymd_opt(2026, 12, 31).unwrap()),
        };
        assert_eq!(build_rrule(&r), "FREQ=MONTHLY;UNTIL=20261231T235959Z");
        round_trip(&r);
    }

    #[test]
    fn yearly_round_trips() {
        round_trip(&Recurrence {
            freq: Freq::Yearly,
            interval: 1,
            byday: vec![],
            end: RecurrenceEnd::Never,
        });
    }

    #[test]
    fn parse_rejects_features_outside_v1() {
        // Sub-daily frequency.
        assert_eq!(parse_rrule("FREQ=HOURLY"), None);
        // nth-weekday token.
        assert_eq!(parse_rrule("FREQ=MONTHLY;BYDAY=3TU"), None);
        // BYDAY on a non-weekly rule.
        assert_eq!(parse_rrule("FREQ=MONTHLY;BYDAY=MO"), None);
        // Unknown key.
        assert_eq!(parse_rrule("FREQ=MONTHLY;BYMONTHDAY=15"), None);
        assert_eq!(parse_rrule("FREQ=WEEKLY;BYSETPOS=1"), None);
        // Both COUNT and UNTIL.
        assert_eq!(
            parse_rrule("FREQ=DAILY;COUNT=3;UNTIL=20261231T235959Z"),
            None
        );
        // Malformed.
        assert_eq!(parse_rrule("garbage"), None);
        assert_eq!(parse_rrule("FREQ="), None);
        assert_eq!(parse_rrule(""), None);
    }

    #[test]
    fn parse_is_case_insensitive_on_keys_and_values() {
        assert_eq!(
            parse_rrule("freq=weekly;byday=mo,we"),
            Some(Recurrence {
                freq: Freq::Weekly,
                interval: 1,
                byday: vec![Weekday::Mon, Weekday::Wed],
                end: RecurrenceEnd::Never,
            })
        );
    }

    // -- validate_event_form -------------------------------------------------

    fn dt(y: i32, mo: u32, d: u32, h: u32) -> chrono::DateTime<chrono::Utc> {
        use chrono::TimeZone;
        chrono::Utc.with_ymd_and_hms(y, mo, d, h, 0, 0).unwrap()
    }

    #[test]
    fn empty_title_is_rejected() {
        assert_eq!(
            validate_event_form("   ", dt(2026, 1, 1, 9), dt(2026, 1, 1, 10)),
            Err(EventFormError::TitleRequired)
        );
    }

    #[test]
    fn ends_before_starts_is_rejected() {
        assert_eq!(
            validate_event_form("Rendez-vous", dt(2026, 1, 1, 10), dt(2026, 1, 1, 9)),
            Err(EventFormError::EndsBeforeStarts)
        );
    }

    #[test]
    fn valid_event_form_is_accepted_including_equal_bounds() {
        assert!(validate_event_form("Rendez-vous", dt(2026, 1, 1, 9), dt(2026, 1, 1, 10)).is_ok());
        assert!(validate_event_form("Ponctuel", dt(2026, 1, 1, 9), dt(2026, 1, 1, 9)).is_ok());
    }

    // -- validate_attachment -------------------------------------------------

    #[test]
    fn allowed_extensions_pass_case_insensitively() {
        for name in ["photo.png", "scan.PDF", "img.JPeG", "a.webp", "b.jpg"] {
            assert!(
                validate_attachment(name, 1024).is_ok(),
                "{name} should pass"
            );
        }
    }

    #[test]
    fn disallowed_extension_is_rejected() {
        assert_eq!(
            validate_attachment("notes.txt", 10),
            Err(AttachmentError::UnsupportedType)
        );
        assert_eq!(
            validate_attachment("noextension", 10),
            Err(AttachmentError::UnsupportedType)
        );
    }

    #[test]
    fn oversize_file_is_rejected_at_the_boundary() {
        assert!(validate_attachment("x.png", MAX_ATTACHMENT_SIZE_BYTES).is_ok());
        assert_eq!(
            validate_attachment("x.png", MAX_ATTACHMENT_SIZE_BYTES + 1),
            Err(AttachmentError::TooLarge)
        );
    }

    // -- month_grid / week_days ----------------------------------------------

    #[test]
    fn month_grid_is_six_weeks_starting_on_a_monday() {
        let grid = month_grid(2026, 7); // July 2026, 1st is a Wednesday
        assert_eq!(grid.len(), 42);
        assert_eq!(grid[0].weekday(), Weekday::Mon);
        assert_eq!(grid[41].weekday(), Weekday::Sun);
        // The grid contains July 1st and it's preceded by June days.
        assert!(grid.contains(&NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()));
        assert_eq!(grid[0], NaiveDate::from_ymd_opt(2026, 6, 29).unwrap());
    }

    #[test]
    fn month_grid_handles_leap_february() {
        let grid = month_grid(2024, 2); // 2024 is a leap year
        assert!(grid.contains(&NaiveDate::from_ymd_opt(2024, 2, 29).unwrap()));
    }

    #[test]
    fn month_grid_rejects_out_of_range_month() {
        assert!(month_grid(2026, 13).is_empty());
    }

    #[test]
    fn week_days_runs_monday_to_sunday() {
        // 2026-07-22 is a Wednesday.
        let days = week_days(NaiveDate::from_ymd_opt(2026, 7, 22).unwrap());
        assert_eq!(days.len(), 7);
        assert_eq!(days[0], NaiveDate::from_ymd_opt(2026, 7, 20).unwrap()); // Mon
        assert_eq!(days[6], NaiveDate::from_ymd_opt(2026, 7, 26).unwrap()); // Sun
    }

    // -- normalize_all_day ---------------------------------------------------

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    /// Paris is UTC+2 in September, so the civil day D runs from
    /// `D-1T22:00Z` to `DT22:00Z`.
    fn sept(d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        utc(2026, 9, d, h, mi)
    }

    #[test]
    fn a_morning_slot_becomes_the_whole_paris_day() {
        // The reproduction from #101: 08:00 -> 09:00 Paris on 2026-09-03,
        // i.e. 06:00Z -> 07:00Z, with "journée entière" ticked.
        let (s, e) = normalize_all_day(sept(3, 6, 0), sept(3, 7, 0));
        assert_eq!(s, sept(2, 22, 0));
        assert_eq!(e, sept(3, 22, 0));
    }

    #[test]
    fn normalization_is_idempotent() {
        // `update_event` re-normalizes on every PATCH, including ones that
        // touch neither timestamp: a second pass must not push the event out
        // by a day.
        let (s1, e1) = normalize_all_day(sept(3, 6, 0), sept(3, 7, 0));
        let (s2, e2) = normalize_all_day(s1, e1);
        assert_eq!((s1, e1), (s2, e2));
    }

    #[test]
    fn a_multi_day_span_keeps_every_day_it_covered() {
        // 2026-09-03 08:00 -> 2026-09-05 09:00 Paris covers three civil
        // days; the normalized end is the midnight *after* the last of them.
        let (s, e) = normalize_all_day(sept(3, 6, 0), sept(5, 7, 0));
        assert_eq!(s, sept(2, 22, 0));
        assert_eq!(e, sept(5, 22, 0));
    }

    #[test]
    fn a_zero_length_instant_becomes_a_full_day() {
        // What an ICS feed without DTEND produces (`google_calendar::parse`
        // falls back to `ends_at = starts_at`), and what the form allows too.
        let (s, e) = normalize_all_day(sept(3, 6, 0), sept(3, 6, 0));
        assert_eq!(s, sept(2, 22, 0));
        assert_eq!(e, sept(3, 22, 0));
    }

    #[test]
    fn an_end_before_the_start_still_yields_one_whole_day() {
        // The API rejects this pair before normalizing, but the function is
        // total: it never returns an end at or before its start.
        let (s, e) = normalize_all_day(sept(3, 6, 0), sept(1, 6, 0));
        assert_eq!(s, sept(2, 22, 0));
        assert_eq!(e, sept(3, 22, 0));
        assert!(e > s);
    }

    #[test]
    fn the_spring_forward_day_is_twenty_three_hours_long() {
        // 2026-03-29: Paris jumps 02:00 -> 03:00, so the civil day is 23 h.
        // A fixed `+ 24 h` end would overshoot into the next day here.
        let start = utc(2026, 3, 29, 10, 0);
        let (s, e) = normalize_all_day(start, start);
        assert_eq!(s, utc(2026, 3, 28, 23, 0));
        assert_eq!(e, utc(2026, 3, 29, 22, 0));
        assert_eq!((e - s).num_hours(), 23);
    }

    #[test]
    fn the_fall_back_day_is_twenty_five_hours_long() {
        // 2026-10-25: Paris repeats 02:00 -> 03:00, so the civil day is
        // 25 h — and a fixed `+ 24 h` end would fall an hour short.
        let start = utc(2026, 10, 25, 10, 0);
        let (s, e) = normalize_all_day(start, start);
        assert_eq!(s, utc(2026, 10, 24, 22, 0));
        assert_eq!(e, utc(2026, 10, 25, 23, 0));
        assert_eq!((e - s).num_hours(), 25);
    }

    #[test]
    fn the_paris_day_is_not_the_utc_day() {
        // 23:30Z on 2026-09-03 is already 01:30 on the 4th in Paris, so the
        // day to normalize onto is the 4th. Anchoring on UTC midnight (what
        // `google_calendar::parse` does for an ICS DATE) names the 3rd here.
        let start = utc(2026, 9, 3, 23, 30);
        let (s, e) = normalize_all_day(start, start);
        assert_eq!(s, sept(3, 22, 0));
        assert_eq!(e, sept(4, 22, 0));
    }

    #[test]
    fn a_winter_day_is_anchored_on_cet_midnight() {
        // January: Paris is UTC+1, so the civil day opens at 23:00Z the day
        // before. The fixed display timezone carries its DST with it.
        let start = utc(2026, 1, 5, 9, 0);
        let (s, e) = normalize_all_day(start, start);
        assert_eq!(s, utc(2026, 1, 4, 23, 0));
        assert_eq!(e, utc(2026, 1, 5, 23, 0));
    }
}
