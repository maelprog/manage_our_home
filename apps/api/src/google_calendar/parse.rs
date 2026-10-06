//! Pure logic: turn a raw ICS feed body into the event shape Agenda
//! (`events` table) understands. No I/O, no DB — kept separate from
//! `imports.rs` so it can be unit-tested directly (TDD, per root
//! CLAUDE.md), the same split `messages::validate_content` and
//! `stocks::can_modify` use.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use icalendar::{Calendar, CalendarComponent, Component, DatePerhapsTime, EventLike};

/// One VEVENT, normalized into the fields Agenda's `events` table stores.
/// Recurring VEVENTs (RRULE) are intentionally *not* expanded here: Google's
/// ICS export already gives each recurrence override its own VEVENT with a
/// RECURRENCE-ID, and expanding the base RRULE ourselves would duplicate
/// Agenda's own `rrule`-based expansion (`src/agenda/recurrence.rs`) for a
/// calendar we don't own writes to — out of scope for a v1 one-way mirror,
/// see docs/v1-scope.md row 9. A plain RRULE VEVENT is imported as a single
/// event using its DTSTART/DTEND (first occurrence only); full recurrence
/// import is a documented v1 limitation, not a bug.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedEvent {
    pub external_uid: String,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub all_day: bool,
    pub external_updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ParseError {
    #[error("invalid_ics")]
    InvalidIcs,
}

fn date_to_utc_midnight(date: NaiveDate) -> DateTime<Utc> {
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("valid midnight"))
}

/// The title an imported event takes when its feed gives none it could show.
const UNTITLED: &str = "(untitled)";

/// The title an imported event is stored under (#372). The import writes
/// `events` directly and never goes through `validate_request`
/// (`agenda/events.rs`), which refuses a blank title since #120: without
/// this, a `SUMMARY:` left empty reached the agenda as an event with no
/// readable name. A feed is not a client that can be told to fix its input,
/// so the event is kept under the same fallback a VEVENT with no `SUMMARY`
/// at all already took — dropping it would lose a real appointment over a
/// missing label. Blank is the API's own test (`str::trim`, U+00A0
/// included); a readable title is stored as the feed wrote it, padding
/// included, as the API stores what it is sent.
fn feed_title(summary: Option<&str>) -> String {
    match summary {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => UNTITLED.to_string(),
    }
}

fn resolve(d: &DatePerhapsTime) -> DateTime<Utc> {
    match d {
        DatePerhapsTime::DateTime(dt) => dt.try_into_utc().unwrap_or_else(Utc::now),
        DatePerhapsTime::Date(date) => date_to_utc_midnight(*date),
    }
}

/// Parses an ICS feed body into a list of importable events. VEVENTs
/// missing a UID or DTSTART are silently skipped (malformed/unsupported
/// entries shouldn't fail the whole import); `STATUS:CANCELLED` VEVENTs are
/// skipped too, since a one-way mirror should drop cancellations rather
/// than surface them as live events. If the same UID appears more than
/// once in the feed (Google emits one VEVENT per RECURRENCE-ID override),
/// the last occurrence in document order wins.
pub fn parse_ics(body: &str) -> Result<Vec<ParsedEvent>, ParseError> {
    let calendar: Calendar = body.parse().map_err(|_| ParseError::InvalidIcs)?;

    let mut events: Vec<ParsedEvent> = Vec::new();
    for component in calendar.components.iter() {
        let CalendarComponent::Event(event) = component else {
            continue;
        };

        let Some(uid) = event.get_uid() else {
            continue;
        };
        let Some(start) = event.get_start() else {
            continue;
        };
        if matches!(event.get_status(), Some(icalendar::EventStatus::Cancelled)) {
            continue;
        }

        let all_day = matches!(start, DatePerhapsTime::Date(_));
        let starts_at = resolve(&start);
        let ends_at = event
            .get_end()
            .map(|e| resolve(&e))
            .filter(|e| *e >= starts_at)
            .unwrap_or(starts_at);

        let title = feed_title(event.get_summary());
        let description = event.get_description().map(str::to_string);
        let location = event.get_location().map(str::to_string);
        let external_updated_at = event.get_last_modified().or_else(|| event.get_timestamp());

        if let Some(existing) = events.iter_mut().find(|e| e.external_uid == uid) {
            *existing = ParsedEvent {
                external_uid: uid.to_string(),
                title,
                description,
                location,
                starts_at,
                ends_at,
                all_day,
                external_updated_at,
            };
        } else {
            events.push(ParsedEvent {
                external_uid: uid.to_string(),
                title,
                description,
                location,
                starts_at,
                ends_at,
                all_day,
                external_updated_at,
            });
        }
    }

    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE_EVENT: &str = "\
BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//Google Inc//Google Calendar 70.9054//EN
BEGIN:VEVENT
UID:abc-123@google.com
DTSTAMP:20260101T090000Z
DTSTART:20260115T140000Z
DTEND:20260115T150000Z
SUMMARY:Dentist
DESCRIPTION:Yearly checkup
LOCATION:123 Main St
LAST-MODIFIED:20260102T090000Z
END:VEVENT
END:VCALENDAR
";

    #[test]
    fn parses_a_simple_timed_event() {
        let events = parse_ics(SIMPLE_EVENT).unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.external_uid, "abc-123@google.com");
        assert_eq!(e.title, "Dentist");
        assert_eq!(e.description.as_deref(), Some("Yearly checkup"));
        assert_eq!(e.location.as_deref(), Some("123 Main St"));
        assert!(!e.all_day);
        assert_eq!(
            e.starts_at,
            Utc.with_ymd_and_hms(2026, 1, 15, 14, 0, 0).unwrap()
        );
        assert_eq!(
            e.ends_at,
            Utc.with_ymd_and_hms(2026, 1, 15, 15, 0, 0).unwrap()
        );
        assert_eq!(
            e.external_updated_at,
            Some(Utc.with_ymd_and_hms(2026, 1, 2, 9, 0, 0).unwrap())
        );
    }

    #[test]
    fn parses_an_all_day_event() {
        let ics = "\
BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:allday-1@google.com
DTSTAMP:20260101T090000Z
DTSTART;VALUE=DATE:20260220
DTEND;VALUE=DATE:20260221
SUMMARY:Birthday
END:VEVENT
END:VCALENDAR
";
        let events = parse_ics(ics).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].all_day);
        assert_eq!(
            events[0].starts_at,
            Utc.with_ymd_and_hms(2026, 2, 20, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn skips_events_missing_uid_or_dtstart() {
        let ics = "\
BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
DTSTAMP:20260101T090000Z
DTSTART:20260115T140000Z
SUMMARY:No UID
END:VEVENT
BEGIN:VEVENT
UID:no-start@google.com
DTSTAMP:20260101T090000Z
SUMMARY:No start
END:VEVENT
END:VCALENDAR
";
        let events = parse_ics(ics).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn skips_cancelled_events() {
        let ics = "\
BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:cancelled-1@google.com
DTSTAMP:20260101T090000Z
DTSTART:20260115T140000Z
SUMMARY:Cancelled meeting
STATUS:CANCELLED
END:VEVENT
END:VCALENDAR
";
        let events = parse_ics(ics).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn later_vevent_with_same_uid_overrides_earlier_one() {
        let ics = "\
BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:recurring-1@google.com
DTSTAMP:20260101T090000Z
DTSTART:20260115T140000Z
DTEND:20260115T150000Z
SUMMARY:Weekly sync
END:VEVENT
BEGIN:VEVENT
UID:recurring-1@google.com
DTSTAMP:20260101T090000Z
DTSTART:20260122T140000Z
DTEND:20260122T150000Z
SUMMARY:Weekly sync (moved)
END:VEVENT
END:VCALENDAR
";
        let events = parse_ics(ics).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "Weekly sync (moved)");
        assert_eq!(
            events[0].starts_at,
            Utc.with_ymd_and_hms(2026, 1, 22, 14, 0, 0).unwrap()
        );
    }

    #[test]
    fn defaults_title_when_summary_missing() {
        let ics = "\
BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:no-summary@google.com
DTSTAMP:20260101T090000Z
DTSTART:20260115T140000Z
END:VEVENT
END:VCALENDAR
";
        let events = parse_ics(ics).unwrap();
        assert_eq!(events[0].title, UNTITLED);
        // No DTEND: ends_at falls back to starts_at rather than erroring.
        assert_eq!(events[0].ends_at, events[0].starts_at);
    }

    // -- feed_title (#372) --------------------------------------------------

    #[test]
    fn a_missing_summary_takes_the_fallback_title() {
        assert_eq!(feed_title(None), UNTITLED);
    }

    #[test]
    fn an_empty_summary_takes_the_fallback_title() {
        assert_eq!(feed_title(Some("")), UNTITLED);
    }

    #[test]
    fn a_summary_of_spaces_takes_the_fallback_title() {
        assert_eq!(feed_title(Some("  \t ")), UNTITLED);
    }

    /// Same `str::trim` as `validate_request` (`agenda/events.rs`), which
    /// drops U+00A0 too: the import lets through exactly the titles the API
    /// takes.
    #[test]
    fn a_summary_of_non_breaking_spaces_takes_the_fallback_title() {
        assert_eq!(feed_title(Some("\u{00a0}\u{00a0}")), UNTITLED);
    }

    /// The API stores a padded title as sent; so does the import.
    #[test]
    fn a_readable_summary_is_kept_as_the_feed_wrote_it() {
        assert_eq!(feed_title(Some("  Anniversaire  ")), "  Anniversaire  ");
    }

    /// The blanks are escaped (`\x20`, `\u{00a0}`): written out, trailing
    /// whitespace in this literal is the first thing an editor strips.
    #[test]
    fn a_blank_summary_in_a_feed_is_imported_under_the_fallback_title() {
        let ics = "\
BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:spaces-summary@google.com
DTSTAMP:20260101T090000Z
DTSTART:20260115T140000Z
SUMMARY:\x20\x20\x20
END:VEVENT
BEGIN:VEVENT
UID:nbsp-summary@google.com
DTSTAMP:20260101T090000Z
DTSTART:20260116T140000Z
SUMMARY:\u{00a0}\u{00a0}
END:VEVENT
BEGIN:VEVENT
UID:empty-summary@google.com
DTSTAMP:20260101T090000Z
DTSTART:20260117T140000Z
SUMMARY:
END:VEVENT
END:VCALENDAR
";
        let events = parse_ics(ics).unwrap();
        assert_eq!(events.len(), 3);
        assert!(events.iter().all(|e| e.title == UNTITLED), "{events:?}");
    }

    #[test]
    fn rejects_garbage_input() {
        assert_eq!(parse_ics("not an ics file"), Err(ParseError::InvalidIcs));
    }
}
