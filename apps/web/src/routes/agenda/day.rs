//! `GET /agenda?view=day` (#793): one Paris day, hour by hour.

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use manage_our_home_shared::dto::agenda::OccurrenceResponse;
use manage_our_home_shared::dto::groups::GroupMember;
use manage_our_home_shared::validation::agenda::{normalize_all_day, paris_start_of_day};

use crate::app::html_escape;

use super::calendar::{family_of, initials, member_tint, people, short_weekday, FamilyTag};
use super::{fmt_paris, private_mark, with_family, DISPLAY_TZ};

/// The precision of the line of the current time: a quarter of its hour.
pub(crate) const SLOT_MINUTES: i64 = 15;

/// The minutes a block is tall at least, half an hour: what one line of
/// « 12:15 Pharmacie » needs to be read.
pub(crate) const MIN_MINUTES: usize = 30;

/// The minutes under which a block is one line (`.short`), as the mock-up
/// draws it: under three quarters of an hour, a second line would not fit.
pub(crate) const ONE_LINE_UNDER: usize = 45;

/// The UTC instants opening `day` and the day after it, in Paris: 23, 24
/// or 25 hours apart.
pub(crate) fn day_bounds(day: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    let next = day.succ_opt().unwrap_or(day);
    (paris_start_of_day(day), paris_start_of_day(next))
}

/// What of an occurrence falls on a day, in minutes elapsed since the day
/// opened — not wall-clock minutes, so that a day of 23 or 25 hours keeps
/// one row per quarter of an hour that really passed. `cut_start` and
/// `cut_end` say that the occurrence began the day before, or ends the day
/// after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Clip {
    pub(crate) from: i64,
    pub(crate) to: i64,
    pub(crate) cut_start: bool,
    pub(crate) cut_end: bool,
}

/// The part of `[starts, ends)` that falls on `day`, if any. An instant
/// (`starts == ends`, a task with no length) falls on the day it is in; an
/// occurrence ending at midnight does not reach the day midnight opens.
pub(crate) fn clip(starts: DateTime<Utc>, ends: DateTime<Utc>, day: NaiveDate) -> Option<Clip> {
    let (opens, closes) = day_bounds(day);
    let touches = if ends > starts {
        starts < closes && ends > opens
    } else {
        (opens..closes).contains(&starts)
    };
    if !touches {
        return None;
    }
    let length = (closes - opens).num_minutes();
    let minutes = |at: DateTime<Utc>| (at - opens).num_minutes().clamp(0, length);
    Some(Clip {
        from: minutes(starts),
        to: minutes(ends.max(starts)),
        cut_start: starts < opens,
        cut_end: ends > closes,
    })
}

/// Where an occurrence goes on a day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Place {
    /// The « Journée » banner: an all-day event, or a timed one of a day
    /// or more, as Google puts it there.
    Banner,
    /// A block of the hours, cut at the day's edges.
    Hours(Clip),
}

/// Where an occurrence goes on `day`, `None` when it does not fall on it.
pub(crate) fn place(
    all_day: bool,
    starts: DateTime<Utc>,
    ends: DateTime<Utc>,
    day: NaiveDate,
) -> Option<Place> {
    let clipped = clip(starts, ends, day)?;
    if all_day || ends - starts >= chrono::Duration::hours(24) {
        Some(Place::Banner)
    } else {
        Some(Place::Hours(clipped))
    }
}

/// The minutes a block covers in a day of `length` minutes, `[start,
/// end)`: those of its event, to the minute, at least `MIN_MINUTES`, and
/// never past the day — a block too close to its end to have its minimum
/// moves up.
pub(crate) fn extent(clip: Clip, length: usize) -> (usize, usize) {
    let start = (clip.from.max(0) as usize).min(length);
    let end = (clip.to.max(0) as usize)
        .max(start + MIN_MINUTES)
        .min(length);
    (start.min(end.saturating_sub(MIN_MINUTES)), end)
}

/// `part / whole` in hundredths, to the thousandth, without trailing zeros:
/// « 25 », « 8.333 », « -50 ».
fn hundredths(part: i64, whole: i64) -> String {
    let value = format!("{:.3}", 100.0 * part as f64 / whole as f64);
    let value = value.trim_end_matches('0').trim_end_matches('.');
    if value == "-0" { "0" } else { value }.to_string()
}

/// The `style` of a block in the cell of the hour it starts in: its top and
/// bottom in hundredths of that hour (the bottom below the cell when it
/// lasts past the hour), its left and right in hundredths of the width, for
/// a block starting `minute` minutes into the hour, `minutes` tall, in
/// `lane` of `width` columns.
pub(crate) fn inset(minute: usize, minutes: usize, lane: Lane, width: usize) -> String {
    let (minute, minutes) = (minute as i64, minutes as i64);
    let (col, span, width) = (lane.col as i64, lane.span as i64, width.max(1) as i64);
    format!(
        "top:{}%;bottom:{}%;left:{}%;right:{}%",
        hundredths(minute, 60),
        hundredths(60 - minute - minutes, 60),
        hundredths(col, width),
        hundredths(width - col - span, width),
    )
}

/// A block's column, and how many columns it spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Lane {
    pub(crate) col: usize,
    pub(crate) span: usize,
}

/// The columns of blocks taking `rows` (`[start, end)` each), and how
/// many columns the day needs — one at least. Blocks are placed by start,
/// the longer first among those starting together, each in the first
/// column free when it starts; then each widens to its right over every
/// column that no block overlapping it holds. So a block alone takes the
/// whole width, and blocks that overlap sit side by side, none hiding
/// another. The lanes come back in the order of `rows`.
pub(crate) fn lanes(rows: &[(usize, usize)]) -> (Vec<Lane>, usize) {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&i| (rows[i].0, std::cmp::Reverse(rows[i].1)));
    let mut col = vec![0; rows.len()];
    // The row each column is free again from.
    let mut free_from: Vec<usize> = Vec::new();
    for &i in &order {
        let (start, end) = rows[i];
        let c = match free_from.iter().position(|&f| f <= start) {
            Some(c) => c,
            None => {
                free_from.push(0);
                free_from.len() - 1
            }
        };
        free_from[c] = end;
        col[i] = c;
    }
    let width = free_from.len().max(1);
    let overlap = |a: (usize, usize), b: (usize, usize)| a.0 < b.1 && b.0 < a.1;
    let lanes = (0..rows.len())
        .map(|i| {
            let mut span = 1;
            while col[i] + span < width
                && !(0..rows.len()).any(|j| col[j] == col[i] + span && overlap(rows[i], rows[j]))
            {
                span += 1;
            }
            Lane { col: col[i], span }
        })
        .collect();
    (lanes, width)
}

/// The name of each hour of `day`, by the clock: « 02:00 » twice on the
/// day the clocks go back, not at all on the day they go forward.
pub(crate) fn hour_labels(day: NaiveDate) -> Vec<String> {
    let (opens, closes) = day_bounds(day);
    (0..(closes - opens).num_hours())
        .map(|h| fmt_paris(opens + chrono::Duration::hours(h), "%H:%M"))
        .collect()
}

/// Where `now` is on `day`: its hour, counted from midnight in elapsed
/// hours as the rows are, and its quarter in that hour. `None` on any other
/// day.
pub(crate) fn now_mark(now: DateTime<Utc>, day: NaiveDate) -> Option<(usize, usize)> {
    let (opens, closes) = day_bounds(day);
    if !(opens..closes).contains(&now) {
        return None;
    }
    let minutes = (now - opens).num_minutes() as usize;
    Some((minutes / 60, minutes % 60 / SLOT_MINUTES as usize))
}

/// The hours a block says, its real ones even when it is cut at midnight:
/// « 10:00–11:30 », « 23:00–01:00 », « 12:15 » for an instant.
pub(crate) fn time_label(starts: DateTime<Utc>, ends: DateTime<Utc>) -> String {
    let from = fmt_paris(starts, "%H:%M");
    if ends <= starts {
        return from;
    }
    format!("{from}–{}", fmt_paris(ends, "%H:%M"))
}

/// What a banner entry says under its title, and whether it lasts more
/// than a day — then it wears the ocre of the generated days rather than
/// its member's tint. An all-day event counts its days, « jour 2 sur 3 »,
/// its end being the exclusive Paris midnight of #101; a timed one of a
/// day or more says when it starts and ends.
pub(crate) fn banner_sub(
    all_day: bool,
    starts: DateTime<Utc>,
    ends: DateTime<Utc>,
    day: NaiveDate,
) -> (String, bool) {
    if !all_day {
        let at = |t: DateTime<Utc>| {
            let date = t.with_timezone(&DISPLAY_TZ).date_naive();
            format!(
                "{} {} à {}",
                short_weekday(date),
                date.day(),
                fmt_paris(t, "%H:%M")
            )
        };
        return (format!("{} → {}", at(starts), at(ends)), true);
    }
    let (first, after) = normalize_all_day(starts, ends);
    let first = first.with_timezone(&DISPLAY_TZ).date_naive();
    let after = after.with_timezone(&DISPLAY_TZ).date_naive();
    let days = (after - first).num_days().max(1);
    if days == 1 {
        return ("Journée entière".to_string(), false);
    }
    let nth = (day - first).num_days() + 1;
    (format!("jour {nth} sur {days}"), true)
}

// -- the page's parts ------------------------------------------------------

/// A task's box and whether it is done, as on a chip of the grid.
fn task_marker(occ: &OccurrenceResponse) -> (&'static str, bool) {
    match (occ.event.is_task, occ.event.completed_at.is_some()) {
        (true, true) => ("☑ ", true),
        (true, false) => ("☐ ", false),
        _ => ("", false),
    }
}

/// What an entry of the banner or a block wears of its people and its
/// family: the tint class of its first member (none on a done task, as on
/// a chip), their initials, its link — carrying its family when that is
/// not the active one — and, in a view of several families, its family's
/// swatch and name, written out.
struct Worn {
    tint: String,
    initials: String,
    family: String,
    href: String,
}

fn worn(occ: &OccurrenceResponse, members: &[GroupMember], families: &[FamilyTag]) -> Worn {
    let people = people(occ, members);
    let (_, done) = task_marker(occ);
    let tint = match people.first() {
        Some(first) if !done => format!(" {}", member_tint(first.user_id)),
        _ => String::new(),
    };
    let family = family_of(occ, families);
    let family_html = match family {
        Some(f) if f.worn => format!(
            r#"<span class="fam" data-accent="{}"><span class="swatch"></span>{}</span>"#,
            f.accent.as_str(),
            html_escape(f.name)
        ),
        _ => String::new(),
    };
    let occ_param = occ.occurrence_starts_at.format("%Y-%m-%dT%H:%M:%SZ");
    Worn {
        tint,
        initials: initials(&people),
        family: family_html,
        href: with_family(
            &format!("/agenda/{}?occ={occ_param}", occ.event.id),
            family.and_then(|f| f.carried),
        ),
    }
}

/// The « Journée » banner at the head of the day (#793): the national days
/// and school holidays of the day, then its all-day events and those of a
/// day or more, each a link to its page. Empty when there is none.
pub(crate) fn banner_html(
    day: NaiveDate,
    occs: &[&OccurrenceResponse],
    national: &[&str],
    members: &[GroupMember],
    families: &[FamilyTag],
) -> String {
    if occs.is_empty() && national.is_empty() {
        return String::new();
    }
    let national: String = national
        .iter()
        .map(|label| format!(r#"<p class="ad gen">{}</p>"#, html_escape(label)))
        .collect();
    let events: String = occs
        .iter()
        .map(|occ| {
            let e = &occ.event;
            let (sub, multi) = banner_sub(
                e.all_day,
                occ.occurrence_starts_at,
                occ.occurrence_ends_at,
                day,
            );
            let (marker, done) = task_marker(occ);
            let w = worn(occ, members, families);
            let class = match (multi, done) {
                (true, _) => " gen",
                (false, true) => " done",
                (false, false) => w.tint.as_str(),
            };
            format!(
                r#"<a class="ad{class}" href="{href}">{initials}<strong>{private}{marker}{title}</strong> <span>{sub}</span>{family}</a>"#,
                href = w.href,
                initials = w.initials,
                private = private_mark(e.is_private),
                title = html_escape(&e.title),
                sub = html_escape(&sub),
                family = w.family,
            )
        })
        .collect();
    format!(r#"<div class="allday"><h2 class="eyebrow">Journée</h2>{national}{events}</div>"#)
}

/// A block of the hours: its title, its real hours, its people's initials,
/// its family in a view of several. `minutes` is how tall it is: under
/// `ONE_LINE_UNDER` it is one line, « 12:15 Pharmacie » (`.short`); under
/// an hour (`.mid`), two lines, its family beside its hours; from an hour,
/// its family on a third. Cut at an edge of the day, that edge is flat and
/// dashed. `inset` places it (`inset`).
fn block_html(
    occ: &OccurrenceResponse,
    clip: Clip,
    minutes: usize,
    inset: &str,
    members: &[GroupMember],
    families: &[FamilyTag],
) -> String {
    let e = &occ.event;
    let (marker, done) = task_marker(occ);
    let w = worn(occ, members, families);
    let short = match minutes {
        m if m < ONE_LINE_UNDER => " short",
        m if m < 60 => " mid",
        _ => "",
    };
    let cut_start = if clip.cut_start { " cut-s" } else { "" };
    let cut_end = if clip.cut_end { " cut-e" } else { "" };
    let done = if done { " done" } else { "" };
    let time = format!(
        r#"<span class="t">{}</span>{}"#,
        html_escape(&time_label(
            occ.occurrence_starts_at,
            occ.occurrence_ends_at
        )),
        w.initials
    );
    // Its lines: one at half an hour; the hours, initials and family on
    // the second under an hour; the family on a third from an hour.
    let lines = match short {
        " short" => format!("{time}{}", w.family),
        " mid" => format!(r#"<span class="sub">{time}{}</span>"#, w.family),
        _ => format!(r#"<span class="sub">{time}</span>{}"#, w.family),
    };
    format!(
        r#"<a class="blk{tint}{short}{cut_start}{cut_end}{done}" style="{inset}" href="{href}"><strong>{private}{marker}{title}</strong> {lines}</a>"#,
        tint = w.tint,
        href = w.href,
        private = private_mark(e.is_private),
        title = html_escape(&e.title),
    )
}

/// The line of the current time, in its hour's cell: a quarter's
/// precision, set when the page is drawn — no script moves it.
fn now_line(quarter: usize, now: DateTime<Utc>) -> String {
    let at = match quarter {
        1 => " q1",
        2 => " q2",
        3 => " q3",
        _ => "",
    };
    format!(
        r#"<span class="now{at}"><span class="sr-only">Il est {}</span></span>"#,
        fmt_paris(now, "%H:%M")
    )
}

/// The hours of the day (#793): a table of one row per hour that elapses —
/// 23, 24 or 25 — each named in its first cell, each block in the second
/// cell of the hour it starts in, placed to the minute by its `style`
/// (`inset`): down from the top of that hour, as tall as it lasts, in its
/// column of the day (`lanes`). That `style` is the one per block that
/// DESIGN.md's residue of inline styles admits (arbitrage of 2026-11-13).
/// The table is `role="presentation"`: its links read in time order, and a
/// screen reader is not walked through two dozen empty cells.
///
/// The row of 08:00 carries `id="h8"`, which the sheet makes the scroller's
/// initial target, so the day opens on the morning without a script; the
/// rows before it are a scroll away. `now`, on the day it falls on, is
/// drawn across its hour. `empty_note` says « Rien de prévu à l'heure » at
/// 08:00 when no block is drawn — the caller leaves it out when the events
/// could not be read.
pub(crate) fn hours_html(
    day: NaiveDate,
    blocks: &[(&OccurrenceResponse, Clip)],
    members: &[GroupMember],
    families: &[FamilyTag],
    now: Option<DateTime<Utc>>,
    empty_note: bool,
) -> String {
    let labels = hour_labels(day);
    let length = labels.len() * 60;
    let spans: Vec<(usize, usize)> = blocks.iter().map(|(_, c)| extent(*c, length)).collect();
    let (lanes, width) = lanes(&spans);
    let mut order: Vec<usize> = (0..blocks.len()).collect();
    order.sort_by_key(|&i| (spans[i].0, lanes[i].col));
    let now_at = now.and_then(|n| now_mark(n, day).map(|m| (m, n)));
    let mut out = String::new();
    for (hour, label) in labels.iter().enumerate() {
        out.push_str(if label == "08:00" {
            r#"<tr id="h8">"#
        } else {
            "<tr>"
        });
        let mark = match now_at {
            Some(((h, quarter), n)) if h == hour => now_line(quarter, n),
            _ => String::new(),
        };
        out.push_str(&format!(r#"<th scope="row">{label}{mark}</th><td>"#));
        if empty_note && blocks.is_empty() && label == "08:00" {
            out.push_str(r#"<p class="muted">Rien de prévu à l'heure ce jour-là.</p>"#);
        }
        for &i in order.iter().filter(|&&i| spans[i].0 / 60 == hour) {
            let (start, end) = spans[i];
            let (occ, clip) = blocks[i];
            let place = inset(start % 60, end - start, lanes[i], width);
            out.push_str(&block_html(
                occ,
                clip,
                end - start,
                &place,
                members,
                families,
            ));
        }
        out.push_str("</td></tr>");
    }
    format!(
        r#"<div class="hours-scroll" tabindex="0" role="region" aria-label="Heures"><table class="hours" role="presentation"><tbody>{out}</tbody></table></div>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// A Paris wall-clock instant.
    fn paris(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        chrono_tz::Europe::Paris
            .with_ymd_and_hms(y, m, d, h, min, 0)
            .earliest()
            .unwrap()
            .with_timezone(&Utc)
    }

    const NOV_12: (i32, u32, u32) = (2026, 11, 12);

    fn nov(d: u32) -> NaiveDate {
        date(NOV_12.0, NOV_12.1, d)
    }

    fn clip_of(from: i64, to: i64, cut_start: bool, cut_end: bool) -> Clip {
        Clip {
            from,
            to,
            cut_start,
            cut_end,
        }
    }

    // -- day_bounds ------------------------------------------------------

    #[test]
    fn a_paris_day_lasts_23_24_or_25_hours() {
        let hours = |d: NaiveDate| {
            let (a, b) = day_bounds(d);
            (b - a).num_hours()
        };
        assert_eq!(hours(nov(12)), 24);
        // The clocks go forward on 2026-03-29 and back on 2026-10-25.
        assert_eq!(hours(date(2026, 3, 29)), 23);
        assert_eq!(hours(date(2026, 10, 25)), 25);
    }

    #[test]
    fn a_day_opens_at_paris_midnight() {
        let (opens, closes) = day_bounds(nov(12));
        assert_eq!(opens, Utc.with_ymd_and_hms(2026, 11, 11, 23, 0, 0).unwrap());
        assert_eq!(
            closes,
            Utc.with_ymd_and_hms(2026, 11, 12, 23, 0, 0).unwrap()
        );
    }

    // -- clip -----------------------------------------------------------

    #[test]
    fn an_event_inside_the_day_keeps_its_minutes() {
        let c = clip(
            paris(2026, 11, 12, 10, 0),
            paris(2026, 11, 12, 11, 30),
            nov(12),
        );
        assert_eq!(c, Some(clip_of(600, 690, false, false)));
    }

    #[test]
    fn an_event_across_midnight_is_cut_on_both_days() {
        let (s, e) = (paris(2026, 11, 12, 23, 0), paris(2026, 11, 13, 1, 0));
        assert_eq!(clip(s, e, nov(12)), Some(clip_of(1380, 1440, false, true)));
        assert_eq!(clip(s, e, nov(13)), Some(clip_of(0, 60, true, false)));
        assert_eq!(clip(s, e, nov(11)), None);
        assert_eq!(clip(s, e, nov(14)), None);
    }

    #[test]
    fn an_event_ending_at_midnight_does_not_reach_the_next_day() {
        let (s, e) = (paris(2026, 11, 11, 23, 0), paris(2026, 11, 12, 0, 0));
        assert_eq!(clip(s, e, nov(11)), Some(clip_of(1380, 1440, false, false)));
        assert_eq!(clip(s, e, nov(12)), None);
    }

    #[test]
    fn an_instant_is_kept_on_its_day() {
        let at = paris(2026, 11, 12, 12, 15);
        assert_eq!(clip(at, at, nov(12)), Some(clip_of(735, 735, false, false)));
        // Midnight opens a day, it does not close the one before.
        let midnight = paris(2026, 11, 12, 0, 0);
        assert_eq!(
            clip(midnight, midnight, nov(12)),
            Some(clip_of(0, 0, false, false))
        );
        assert_eq!(clip(midnight, midnight, nov(11)), None);
    }

    #[test]
    fn minutes_are_elapsed_time_on_a_day_of_23_hours() {
        // 2026-03-29: 01:30 is 90 minutes in; 03:30, an hour after 02:30
        // that never was, is 150.
        let c = clip(
            paris(2026, 3, 29, 1, 30),
            paris(2026, 3, 29, 3, 30),
            date(2026, 3, 29),
        );
        assert_eq!(c, Some(clip_of(90, 150, false, false)));
        // The day ends 1380 minutes in.
        let late = clip(
            paris(2026, 3, 29, 23, 0),
            paris(2026, 3, 30, 1, 0),
            date(2026, 3, 29),
        );
        assert_eq!(late, Some(clip_of(1320, 1380, false, true)));
    }

    #[test]
    fn minutes_are_elapsed_time_on_a_day_of_25_hours() {
        // 2026-10-25: 04:00 comes five hours after midnight.
        let c = clip(
            paris(2026, 10, 25, 1, 0),
            paris(2026, 10, 25, 4, 0),
            date(2026, 10, 25),
        );
        assert_eq!(c, Some(clip_of(60, 300, false, false)));
        let late = clip(
            paris(2026, 10, 25, 23, 0),
            paris(2026, 10, 26, 1, 0),
            date(2026, 10, 25),
        );
        assert_eq!(late, Some(clip_of(1440, 1500, false, true)));
    }

    // -- place ----------------------------------------------------------

    #[test]
    fn an_all_day_event_goes_to_the_banner_of_each_day_it_covers() {
        // Three days, stored with the exclusive Paris midnight of #101.
        let (s, e) = (paris(2026, 11, 11, 0, 0), paris(2026, 11, 14, 0, 0));
        for d in [11, 12, 13] {
            assert_eq!(place(true, s, e, nov(d)), Some(Place::Banner), "{d}");
        }
        assert_eq!(place(true, s, e, nov(10)), None);
        assert_eq!(place(true, s, e, nov(14)), None);
    }

    #[test]
    fn a_timed_event_of_a_day_or_more_goes_to_the_banner_like_google() {
        let (s, e) = (paris(2026, 11, 11, 18, 0), paris(2026, 11, 13, 10, 0));
        for d in [11, 12, 13] {
            assert_eq!(place(false, s, e, nov(d)), Some(Place::Banner), "{d}");
        }
        assert_eq!(place(false, s, e, nov(14)), None);
        // Exactly 24 hours is a day.
        let (s, e) = (paris(2026, 11, 12, 9, 0), paris(2026, 11, 13, 9, 0));
        assert_eq!(place(false, s, e, nov(12)), Some(Place::Banner));
    }

    #[test]
    fn a_timed_event_under_a_day_goes_to_the_hours_cut_at_midnight() {
        let (s, e) = (paris(2026, 11, 12, 23, 0), paris(2026, 11, 13, 1, 0));
        assert_eq!(
            place(false, s, e, nov(12)),
            Some(Place::Hours(clip_of(1380, 1440, false, true)))
        );
        assert_eq!(
            place(false, s, e, nov(13)),
            Some(Place::Hours(clip_of(0, 60, true, false)))
        );
        assert_eq!(place(false, s, e, nov(14)), None);
    }

    // -- extent: a block's position and height ----------------------------

    #[test]
    fn a_block_starts_at_its_minute_and_is_as_tall_as_its_length() {
        assert_eq!(extent(clip_of(600, 690, false, false), 1440), (600, 690));
        // 10:05–11:20, to the minute.
        assert_eq!(extent(clip_of(605, 680, false, false), 1440), (605, 680));
    }

    #[test]
    fn a_short_block_is_half_an_hour_tall() {
        assert_eq!(extent(clip_of(735, 735, false, false), 1440), (735, 765));
        assert_eq!(extent(clip_of(735, 750, false, false), 1440), (735, 765));
        assert_eq!(extent(clip_of(605, 610, false, false), 1440), (605, 635));
        assert_eq!(extent(clip_of(600, 630, false, false), 1440), (600, 630));
    }

    #[test]
    fn a_block_stays_inside_the_day() {
        assert_eq!(extent(clip_of(1380, 1440, false, true), 1440), (1380, 1440));
        // 23:50: half an hour, the last of the day.
        assert_eq!(
            extent(clip_of(1430, 1440, false, false), 1440),
            (1410, 1440)
        );
        // A day of 23 hours lasts 1380 minutes.
        assert_eq!(extent(clip_of(1320, 1380, false, true), 1380), (1320, 1380));
        assert_eq!(
            extent(clip_of(1380, 1380, false, false), 1380),
            (1350, 1380)
        );
    }

    // -- inset: a block's place in the cell of its hour --------------------

    #[test]
    fn a_block_is_placed_in_hundredths_of_its_hour_and_of_the_width() {
        // 10:00–11:30, alone: from the top of its hour to half the next.
        assert_eq!(
            inset(0, 90, lane(0, 1), 1),
            "top:0%;bottom:-50%;left:0%;right:0%"
        );
        // 10:15–10:45, the second of two columns.
        assert_eq!(
            inset(15, 30, lane(1, 1), 2),
            "top:25%;bottom:25%;left:50%;right:0%"
        );
        // 10:05–10:50, the first of three columns, spanning two.
        assert_eq!(
            inset(5, 45, lane(0, 2), 3),
            "top:8.333%;bottom:16.667%;left:0%;right:33.333%"
        );
    }

    // -- lanes: overlapping blocks side by side ---------------------------

    fn lane(col: usize, span: usize) -> Lane {
        Lane { col, span }
    }

    #[test]
    fn a_block_alone_takes_the_whole_width() {
        assert_eq!(lanes(&[(40, 46)]), (vec![lane(0, 1)], 1));
        assert_eq!(lanes(&[]), (vec![], 1));
    }

    #[test]
    fn blocks_that_follow_each_other_share_a_column() {
        assert_eq!(
            lanes(&[(40, 44), (44, 48)]),
            (vec![lane(0, 1), lane(0, 1)], 1)
        );
    }

    #[test]
    fn overlapping_blocks_sit_side_by_side() {
        assert_eq!(
            lanes(&[(40, 46), (42, 44)]),
            (vec![lane(0, 1), lane(1, 1)], 2)
        );
    }

    #[test]
    fn a_block_later_in_the_day_takes_back_the_whole_width() {
        assert_eq!(
            lanes(&[(40, 46), (42, 44), (60, 64)]),
            (vec![lane(0, 1), lane(1, 1), lane(0, 2)], 2)
        );
    }

    #[test]
    fn a_freed_column_is_taken_again() {
        // The third starts when the second ends: the second's column.
        assert_eq!(
            lanes(&[(40, 48), (42, 44), (44, 46)]),
            (vec![lane(0, 1), lane(1, 1), lane(1, 1)], 2)
        );
    }

    #[test]
    fn a_block_widens_over_the_columns_free_all_along_it() {
        // A, B, C start together; D starts when C ends and takes its
        // column; E starts when B ends: B's column, D still beside it.
        // F, after them all, spans the three.
        let (l, k) = lanes(&[(40, 48), (40, 44), (40, 42), (42, 48), (44, 46), (50, 52)]);
        assert_eq!(k, 3);
        assert_eq!(
            l,
            vec![
                lane(0, 1),
                lane(1, 1),
                lane(2, 1),
                lane(2, 1),
                lane(1, 1),
                lane(0, 3)
            ]
        );
        // G, beside A only, widens over the two columns no one else holds.
        let (l, k) = lanes(&[(40, 48), (40, 42), (40, 42), (44, 46)]);
        assert_eq!(k, 3);
        assert_eq!(l[3], lane(1, 2));
    }

    #[test]
    fn the_lanes_do_not_depend_on_the_order_given() {
        // The longer of two blocks starting together goes first.
        assert_eq!(
            lanes(&[(42, 44), (40, 46)]),
            (vec![lane(1, 1), lane(0, 1)], 2)
        );
        assert_eq!(
            lanes(&[(40, 42), (40, 46)]),
            (vec![lane(1, 1), lane(0, 1)], 2)
        );
    }

    // -- hour_labels -------------------------------------------------------

    #[test]
    fn each_hour_is_named_by_the_clock() {
        let labels = hour_labels(nov(12));
        assert_eq!(labels.len(), 24);
        assert_eq!(labels[0], "00:00");
        assert_eq!(labels[8], "08:00");
        assert_eq!(labels[23], "23:00");
    }

    #[test]
    fn a_day_of_23_hours_skips_two_oclock_and_one_of_25_repeats_it() {
        let spring = hour_labels(date(2026, 3, 29));
        assert_eq!(spring.len(), 23);
        assert_eq!(&spring[..4], ["00:00", "01:00", "03:00", "04:00"]);
        let autumn = hour_labels(date(2026, 10, 25));
        assert_eq!(autumn.len(), 25);
        assert_eq!(&autumn[..5], ["00:00", "01:00", "02:00", "02:00", "03:00"]);
        assert_eq!(autumn[24], "23:00");
    }

    // -- now_mark ----------------------------------------------------------

    #[test]
    fn now_is_marked_in_its_hour_and_quarter_on_its_day_only() {
        let now = paris(2026, 11, 12, 14, 20);
        assert_eq!(now_mark(now, nov(12)), Some((14, 1)));
        assert_eq!(now_mark(now, nov(11)), None);
        assert_eq!(now_mark(now, nov(13)), None);
        assert_eq!(now_mark(paris(2026, 11, 12, 0, 0), nov(12)), Some((0, 0)));
        assert_eq!(
            now_mark(paris(2026, 11, 12, 23, 59), nov(12)),
            Some((23, 3))
        );
    }

    #[test]
    fn now_counts_elapsed_hours_on_a_day_of_25() {
        // 10:00 on 2026-10-25 is eleven hours after midnight.
        assert_eq!(
            now_mark(paris(2026, 10, 25, 10, 0), date(2026, 10, 25)),
            Some((11, 0))
        );
    }

    // -- time_label --------------------------------------------------------

    #[test]
    fn a_block_says_its_real_hours() {
        assert_eq!(
            time_label(paris(2026, 11, 12, 10, 0), paris(2026, 11, 12, 11, 30)),
            "10:00–11:30"
        );
        // Cut at midnight, it still says when it ends.
        assert_eq!(
            time_label(paris(2026, 11, 12, 23, 0), paris(2026, 11, 13, 1, 0)),
            "23:00–01:00"
        );
        let at = paris(2026, 11, 12, 12, 15);
        assert_eq!(time_label(at, at), "12:15");
    }

    // -- banner_sub --------------------------------------------------------

    #[test]
    fn an_all_day_event_of_one_day_says_so() {
        let (s, e) = (paris(2026, 11, 12, 0, 0), paris(2026, 11, 13, 0, 0));
        assert_eq!(
            banner_sub(true, s, e, nov(12)),
            ("Journée entière".to_string(), false)
        );
    }

    #[test]
    fn an_all_day_event_of_several_days_counts_them() {
        let (s, e) = (paris(2026, 11, 11, 0, 0), paris(2026, 11, 14, 0, 0));
        assert_eq!(
            banner_sub(true, s, e, nov(11)),
            ("jour 1 sur 3".to_string(), true)
        );
        assert_eq!(
            banner_sub(true, s, e, nov(12)),
            ("jour 2 sur 3".to_string(), true)
        );
        assert_eq!(
            banner_sub(true, s, e, nov(13)),
            ("jour 3 sur 3".to_string(), true)
        );
    }

    #[test]
    fn a_timed_event_of_a_day_or_more_says_when_it_starts_and_ends() {
        let (s, e) = (paris(2026, 11, 11, 18, 0), paris(2026, 11, 13, 10, 0));
        assert_eq!(
            banner_sub(false, s, e, nov(12)),
            ("mer. 11 à 18:00 → ven. 13 à 10:00".to_string(), true)
        );
    }

    // -- the page's parts ----------------------------------------------------

    use manage_our_home_shared::dto::agenda::EventResponse;
    use uuid::Uuid;

    fn occ(title: &str, all_day: bool, s: DateTime<Utc>, e: DateTime<Utc>) -> OccurrenceResponse {
        OccurrenceResponse {
            event: EventResponse {
                id: Uuid::from_u128(1),
                group_id: Uuid::nil(),
                created_by: Uuid::from_u128(7),
                title: title.to_string(),
                description: None,
                location: None,
                starts_at: s,
                ends_at: e,
                all_day,
                is_task: false,
                completed_at: None,
                rrule: None,
                assignee_ids: vec![Uuid::from_u128(7)],
                is_private: false,
            },
            occurrence_starts_at: s,
            occurrence_ends_at: e,
        }
    }

    fn member() -> GroupMember {
        GroupMember {
            user_id: Uuid::from_u128(7),
            display_name: "Chloé".to_string(),
            email: "c@example.test".to_string(),
            has_avatar: false,
            role: "member".to_string(),
        }
    }

    /// The `style` attribute holding `value`, assembled: the count of inline
    /// styles (`app.rs`) reads this file, and an expectation is not one.
    fn styled(value: &str) -> String {
        format!("style{}\"{value}\"", '=')
    }

    /// The hours of 12 November drawn with `occs`, each clipped to the day.
    fn hours(occs: &[OccurrenceResponse], now: Option<DateTime<Utc>>) -> String {
        let blocks: Vec<(&OccurrenceResponse, Clip)> = occs
            .iter()
            .map(|o| {
                let c = clip(o.occurrence_starts_at, o.occurrence_ends_at, nov(12)).unwrap();
                (o, c)
            })
            .collect();
        hours_html(nov(12), &blocks, &[member()], &[], now, true)
    }

    #[test]
    fn the_hours_are_a_row_per_hour_each_named() {
        let html = hours(&[], None);
        assert_eq!(html.matches("<tr").count(), 24, "{html}");
        assert_eq!(html.matches(r#"<th scope="row">"#).count(), 24, "{html}");
        assert!(
            html.contains(r#"<tr><th scope="row">00:00</th><td></td></tr>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<tr id="h8"><th scope="row">08:00</th>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<div class="hours-scroll" tabindex="0" role="region" aria-label="Heures"><table class="hours" role="presentation">"#),
            "{html}"
        );
    }

    #[test]
    fn a_day_with_nothing_at_an_hour_says_so_at_eight() {
        let html = hours(&[], None);
        assert!(
            html.contains(r#"<tr id="h8"><th scope="row">08:00</th><td><p class="muted">Rien de prévu à l'heure ce jour-là.</p></td></tr>"#),
            "{html}"
        );
        // Not when the events could not be read.
        let quiet = hours_html(nov(12), &[], &[], &[], None, false);
        assert!(!quiet.contains("Rien de prévu"), "{quiet}");
    }

    #[test]
    fn a_block_is_placed_in_its_hour_as_tall_as_it_lasts_in_its_members_tint() {
        let o = occ(
            "Réunion école",
            false,
            paris(2026, 11, 12, 10, 0),
            paris(2026, 11, 12, 11, 30),
        );
        let html = hours(&[o], None);
        let tint = member_tint(Uuid::from_u128(7));
        assert!(
            html.contains(&format!(
                r#"<tr><th scope="row">10:00</th><td><a class="blk {tint}" {} href="/agenda/{}?occ=2026-11-12T09:00:00Z"><strong>Réunion école</strong> <span class="sub"><span class="t">10:00–11:30</span><span class="avatar""#,
                styled("top:0%;bottom:-50%;left:0%;right:0%"),
                Uuid::from_u128(1)
            )),
            "{html}"
        );
        assert!(!html.contains("Rien de prévu"), "{html}");
    }

    /// A block of `minutes` from 10:00 in a view of several families, its
    /// family worn.
    fn worn_block(minutes: i64) -> String {
        let o = occ(
            "Piano",
            false,
            paris(2026, 11, 12, 10, 0),
            paris(2026, 11, 12, 10, 0) + chrono::Duration::minutes(minutes),
        );
        let c = clip(o.occurrence_starts_at, o.occurrence_ends_at, nov(12)).unwrap();
        let fam = FamilyTag {
            gid: Uuid::nil(),
            name: "Foyer",
            accent: manage_our_home_shared::validation::groups::Accent::default(),
            carried: None,
            worn: true,
        };
        let html = hours_html(nov(12), &[(&o, c)], &[member()], &[fam], None, true);
        let start = html.find(r#"<a class="blk"#).expect("a block");
        html[start..start + html[start..].find("</a>").unwrap()].to_string()
    }

    #[test]
    fn a_blocks_lines_are_its_title_its_hours_and_its_family() {
        let fam =
            r#"<span class="fam" data-accent="sarcelle"><span class="swatch"></span>Foyer</span>"#;
        // Half an hour: one line, as it was.
        let short = worn_block(30);
        assert!(
            short.contains(
                r#"<strong>Piano</strong> <span class="t">10:00–10:30</span><span class="avatar"#
            ),
            "{short}"
        );
        assert!(short.ends_with(&format!("</span>{fam}")), "{short}");
        assert!(!short.contains(r#"class="sub""#), "{short}");
        // Under an hour: the hours, the initials and the family on one line.
        let mid = worn_block(45);
        assert!(
            mid.contains(r#"<strong>Piano</strong> <span class="sub"><span class="t">10:00–10:45</span><span class="avatar"#),
            "{mid}"
        );
        assert!(mid.ends_with(&format!("</span>{fam}</span>")), "{mid}");
        // An hour or more: the family on a line of its own.
        let long = worn_block(90);
        assert!(
            long.contains(r#"<strong>Piano</strong> <span class="sub"><span class="t">10:00–11:30</span><span class="avatar"#),
            "{long}"
        );
        assert!(long.ends_with(&format!("</span></span>{fam}")), "{long}");
    }

    #[test]
    fn a_block_under_three_quarters_is_short_and_under_an_hour_mid() {
        let at = |title: &str, minutes: u32| {
            let html = hours(
                &[occ(
                    title,
                    false,
                    paris(2026, 11, 12, 10, 0),
                    paris(2026, 11, 12, 10, 0) + chrono::Duration::minutes(minutes.into()),
                )],
                None,
            );
            let start = html.find(r#"<a class="blk"#).expect("a block");
            html[start..start + html[start..].find('>').unwrap()].to_string()
        };
        assert!(!at("Pharmacie", 30).contains(" mid"));
        assert!(at("Pharmacie", 30).contains(" short"));
        // Under three quarters of an hour, one line, as the mock-up has it.
        assert!(at("Piano", 31).contains(" short"));
        assert!(at("Piano", 44).contains(" short"));
        assert!(!at("Piano", 44).contains(" mid"));
        assert!(at("Piano", 45).contains(" mid"));
        assert!(!at("Piano", 45).contains(" short"));
        assert!(at("Piano", 59).contains(" mid"));
        assert!(!at("Réunion", 60).contains(" mid"));
        assert!(!at("Réunion", 90).contains(" mid"));
    }

    #[test]
    fn overlapping_blocks_are_side_by_side_and_a_short_one_is_one_line() {
        let a = occ(
            "Réunion",
            false,
            paris(2026, 11, 12, 10, 0),
            paris(2026, 11, 12, 11, 30),
        );
        let b = occ(
            "Plombier",
            false,
            paris(2026, 11, 12, 10, 30),
            paris(2026, 11, 12, 11, 0),
        );
        let html = hours(&[a, b], None);
        // Two columns in the cell of 10:00: the long block on the left,
        // the short one beside it from half past, in time order.
        let long = html
            .find(&styled("top:0%;bottom:-50%;left:0%;right:50%"))
            .expect(&html);
        let short = html
            .find(&format!(
                r#" short" {}"#,
                styled("top:50%;bottom:0%;left:50%;right:0%")
            ))
            .expect(&html);
        assert!(long < short, "{html}");
        assert!(
            html.contains(r#"<tr><th scope="row">10:00</th><td><a class="blk"#),
            "{html}"
        );
        // A row away from them has an empty cell.
        assert!(
            html.contains(r#"<tr><th scope="row">00:00</th><td></td></tr>"#),
            "{html}"
        );
    }

    #[test]
    fn a_block_cut_at_midnight_is_flat_there_and_says_its_real_hours() {
        let o = occ(
            "Soirée",
            false,
            paris(2026, 11, 12, 23, 0),
            paris(2026, 11, 13, 1, 0),
        );
        let html = hours(&[o], None);
        assert!(
            html.contains(r#"<tr><th scope="row">23:00</th><td><a class="blk"#),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#" cut-e" {}"#,
                styled("top:0%;bottom:0%;left:0%;right:0%")
            )),
            "{html}"
        );
        assert!(html.contains("23:00–01:00"), "{html}");
    }

    #[test]
    fn now_is_a_line_in_its_hour_on_today_only() {
        let html = hours(&[], Some(paris(2026, 11, 12, 14, 20)));
        assert!(
            html.contains(r#"<th scope="row">14:00<span class="now q1"><span class="sr-only">Il est 14:20</span></span></th>"#),
            "{html}"
        );
        assert_eq!(html.matches("now").count(), 1, "{html}");
        let other = hours(&[], Some(paris(2026, 11, 13, 14, 20)));
        assert!(!other.contains("now"), "{other}");
    }

    #[test]
    fn the_morning_is_found_by_the_clock_on_a_day_of_25_hours() {
        let html = hours_html(date(2026, 10, 25), &[], &[], &[], None, true);
        assert_eq!(html.matches("<tr").count(), 25, "{html}");
        assert!(
            html.contains(r#"<tr id="h8"><th scope="row">08:00</th>"#),
            "{html}"
        );
    }

    #[test]
    fn the_banner_lists_national_days_then_events() {
        let one = occ(
            "Anniversaire",
            true,
            paris(2026, 11, 12, 0, 0),
            paris(2026, 11, 13, 0, 0),
        );
        let three = occ(
            "Voyage",
            true,
            paris(2026, 11, 11, 0, 0),
            paris(2026, 11, 14, 0, 0),
        );
        let html = banner_html(nov(12), &[&one, &three], &["Armistice"], &[member()], &[]);
        assert!(
            html.starts_with(r#"<div class="allday"><h2 class="eyebrow">Journée</h2><p class="ad gen">Armistice</p>"#),
            "{html}"
        );
        let tint = member_tint(Uuid::from_u128(7));
        // One day: its member's tint. Three: the ocre of generated days,
        // the initials kept.
        assert!(
            html.contains(&format!(r#"<a class="ad {tint}" href="#)),
            "{html}"
        );
        assert!(
            html.contains("<strong>Anniversaire</strong> <span>Journée entière</span>"),
            "{html}"
        );
        assert!(html.contains(r#"<a class="ad gen" href="#), "{html}");
        assert!(
            html.contains("<strong>Voyage</strong> <span>jour 2 sur 3</span>"),
            "{html}"
        );
        assert_eq!(html.matches(r#"class="avatar""#).count(), 2, "{html}");
    }

    #[test]
    fn no_banner_on_a_day_without_one() {
        assert_eq!(banner_html(nov(12), &[], &[], &[], &[]), "");
    }
}
