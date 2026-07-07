use chrono::{DateTime, Duration, Utc};
use rrule::{RRuleSet, Tz};

/// Cap on occurrences expanded per request — a window query is always
/// date-bounded, but an unbounded RRULE (no COUNT/UNTIL) combined with a
/// huge `[from, to]` window could otherwise generate an unreasonable
/// number of rows. 1000 comfortably covers "daily for 2+ years".
const MAX_OCCURRENCES: u16 = 1000;

/// Expands `rrule` (an RFC 5545 RRULE string, without the DTSTART line —
/// that's derived from `starts_at`) into occurrence start times that fall
/// within `[from, to]`. Returns an error only for a malformed RRULE string;
/// callers validate on write (`validate`) so this should not normally fail.
pub fn expand_occurrences(
    rrule: &str,
    starts_at: DateTime<Utc>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<DateTime<Utc>>, rrule::RRuleError> {
    let ical = format!("DTSTART:{}\nRRULE:{}", starts_at.format("%Y%m%dT%H%M%SZ"), rrule);
    let set: RRuleSet = ical.parse()?;
    // `RRuleSet::after`/`before` are exclusive of the boundary instant, but
    // callers (list_events) treat `[from, to]` as inclusive on both ends —
    // nudge by a second so an occurrence landing exactly on `from` or `to`
    // isn't silently dropped.
    let set = set
        .after((from - Duration::seconds(1)).with_timezone(&Tz::UTC))
        .before((to + Duration::seconds(1)).with_timezone(&Tz::UTC));

    let result = set.all(MAX_OCCURRENCES);
    Ok(result.dates.into_iter().map(|d| d.with_timezone(&Utc)).collect())
}

/// Validates that `rrule` parses as a well-formed RRULE, used when
/// creating/updating an event so a bad value is rejected at write time
/// (400) instead of surfacing as a silent empty expansion later.
pub fn validate(rrule: &str, starts_at: DateTime<Utc>) -> Result<(), rrule::RRuleError> {
    let ical = format!("DTSTART:{}\nRRULE:{}", starts_at.format("%Y%m%dT%H%M%SZ"), rrule);
    ical.parse::<RRuleSet>().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

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
}
