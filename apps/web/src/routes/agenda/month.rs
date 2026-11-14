//! The month view of `GET /agenda` (#836): six weeks of seven days, each
//! day's events as bars of their titles, on a phone as on a PC.
//!
//! # One markup, read at two widths
//!
//! The grid is drawn once. A week is a CSS grid of seven columns: a row for
//! the days' numbers, then a row per bar. Each day is a link to its day view
//! (#793) as tall as its week, drawn under its bars; each bar is placed by
//! three classes — its first column (`.c1`…`.c7`), how many columns it
//! spans (`.s2`…`.s7`) and its row (`.l0`…`.l4`) — so that an event of
//! several days is one bar across its columns, cut at the week's edges and
//! continued on the next week.
//!
//! A phone shows three bars a day, a PC four (`PHONE_BARS`, `PC_BARS`):
//! the fourth row's bars are `.pc`, and each day counts what it does not
//! show into a « +N » that leads to its day view — `.phone` for the count
//! past three, `.pc` for the count past four.
//!
//! It replaces, for the month, the grid of #71 and its re-reading as a list
//! under 861px, and the mini grid of #675 above that list. The week view
//! keeps them (`calendar::render_grid`, `calendar::render_mini`).
//!
//! # What a bar says
//!
//! Its title, cut by an ellipsis — the day view reads it whole —, after
//! « ↻ » for a recurring event and « ☐ » / « ☑ » for a task. An event of a
//! day wears its first member's tint and edge: filled when it concerns the
//! visitor (`concerns`), outlined when it concerns other members — the
//! shape carries the difference, not the hue alone (WCAG 1.4.1). An event
//! of several days, a national day and a school holiday wear the ocre of
//! what the app generates (`.gen`, #565): a national day and a school
//! holiday are text, not links, and name no member.

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use manage_our_home_shared::dto::agenda::{EventResponse, NationalDayResponse, OccurrenceResponse};
use manage_our_home_shared::dto::groups::GroupMember;
use manage_our_home_shared::validation::agenda::normalize_all_day;
use uuid::Uuid;

use crate::app::html_escape;

use super::calendar::{
    day_href, family_of, member_tint, people, FamilyTag, FR_WEEKDAYS, SCHOOL_HOLIDAY,
};
use super::day::day_name;
use super::{private_mark, with_family, DISPLAY_TZ};

/// The bars a day shows on a phone, before its « +N ».
pub(crate) const PHONE_BARS: usize = 3;
/// The bars a day shows on a PC, before its « +N ».
pub(crate) const PC_BARS: usize = 4;

/// The classes of a bar's first column, written out because the
/// stylesheet's guards look for a class in the sources by its name.
const COLS: [&str; 7] = ["c1", "c2", "c3", "c4", "c5", "c6", "c7"];
/// The classes of how many columns a bar spans, one column needing none.
const SPANS: [&str; 7] = ["", "s2", "s3", "s4", "s5", "s6", "s7"];
/// The classes of a bar's row: its lane, under the days' numbers. The last
/// one holds a PC's « +N ».
const LANES: [&str; 5] = ["l0", "l1", "l2", "l3", "l4"];

/// What a bar stands for.
#[derive(Debug, Clone, Copy)]
pub(crate) enum What<'a> {
    /// A school holiday of the family's zone (#565), over every day it covers.
    Holiday(&'a str),
    /// A one-day national day (#565): public holiday, clock change, fête.
    National(&'a str),
    /// One of the family's occurrences.
    Event(&'a OccurrenceResponse),
}

/// A bar of the month, with the first and last day it covers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Item<'a> {
    pub(crate) what: What<'a>,
    pub(crate) first: NaiveDate,
    pub(crate) last: NaiveDate,
}

impl Item<'_> {
    fn covers(&self, day: NaiveDate) -> bool {
        (self.first..=self.last).contains(&day)
    }
}

/// The days an occurrence's bar covers, first and last. An event on the
/// whole day, or of 24 hours or more, covers every day it reaches — the
/// day view's banner rule (`day::place`), and Google's; a timed event under
/// a day is a bar on the day it starts, even past midnight.
pub(crate) fn span(
    all_day: bool,
    starts: DateTime<Utc>,
    ends: DateTime<Utc>,
) -> (NaiveDate, NaiveDate) {
    let date = |t: DateTime<Utc>| t.with_timezone(&DISPLAY_TZ).date_naive();
    let (starts, ends) = if all_day {
        normalize_all_day(starts, ends)
    } else if ends - starts >= chrono::Duration::hours(24) {
        (starts, ends)
    } else {
        return (date(starts), date(starts));
    };
    // The end is exclusive: an event ending at midnight does not reach
    // the day midnight opens.
    let last = date(ends - chrono::Duration::seconds(1));
    (date(starts), last.max(date(starts)))
}

/// Where a bar sits in a week: its first column (0 for Monday), how many
/// columns it spans, and its lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Slot {
    pub(crate) col: usize,
    pub(crate) width: usize,
    pub(crate) lane: usize,
}

/// The slots of the bars of the week opening on `monday`, in the order the
/// bars come: each bar, cut at the week's edges, takes the lowest lane free
/// on every column it covers. `None` for a bar that does not reach the week.
pub(crate) fn pack(monday: NaiveDate, spans: &[(NaiveDate, NaiveDate)]) -> Vec<Option<Slot>> {
    let sunday = monday + chrono::Duration::days(6);
    // The lanes taken, per column.
    let mut taken: [Vec<bool>; 7] = Default::default();
    spans
        .iter()
        .map(|&(first, last)| {
            if last < monday || first > sunday {
                return None;
            }
            let col = (first.max(monday) - monday).num_days() as usize;
            let end = (last.min(sunday) - monday).num_days() as usize;
            let lane = (0..)
                .find(|&lane| (col..=end).all(|c| !taken[c].get(lane).copied().unwrap_or(false)))
                .unwrap_or(0);
            for column in &mut taken[col..=end] {
                if column.len() <= lane {
                    column.resize(lane + 1, false);
                }
                column[lane] = true;
            }
            Some(Slot {
                col,
                width: end - col + 1,
                lane,
            })
        })
        .collect()
}

/// How many bars covering column `col` sit at lane `cap` or below it: what
/// a day showing `cap` bars counts into its « +N ».
pub(crate) fn hidden(slots: &[Option<Slot>], col: usize, cap: usize) -> usize {
    slots
        .iter()
        .flatten()
        .filter(|s| s.lane >= cap && (s.col..s.col + s.width).contains(&col))
        .count()
}

/// Whether an event concerns the visitor: they are among its assignees, or
/// it has none and they created it — the people `calendar::people` names,
/// read off the ids rather than the roster, which may not be readable.
pub(crate) fn concerns(event: &EventResponse, me: Uuid) -> bool {
    if event.assignee_ids.is_empty() {
        event.created_by == me
    } else {
        event.assignee_ids.contains(&me)
    }
}

/// A day's accessible name: « jeudi 12 novembre, 7 événements, 3 qui te
/// concernent », « …, rien de prévu ».
pub(crate) fn day_label(day: NaiveDate, events: usize, mine: usize) -> String {
    let name = day_name(day);
    let count = match events {
        0 => return format!("{name}, rien de prévu"),
        1 => "1 événement".to_string(),
        n => format!("{n} événements"),
    };
    let concern = match mine {
        0 => "aucun ne te concerne".to_string(),
        1 => "1 qui te concerne".to_string(),
        n => format!("{n} qui te concernent"),
    };
    format!("{name}, {count}, {concern}")
}

/// The bars of the month, in the order they take their lanes: the bars of
/// several days first — earliest, then longest, a school holiday before an
/// event — then a day's own: its national days, its events on the whole
/// day, its timed events in time order (`occs` comes in time order).
pub(crate) fn items<'a>(
    occs: &'a [OccurrenceResponse],
    national: &'a [NationalDayResponse],
) -> Vec<Item<'a>> {
    let generated = national.iter().map(|n| {
        if n.kind == SCHOOL_HOLIDAY {
            Item {
                what: What::Holiday(&n.label),
                first: n.date,
                last: n.end.unwrap_or(n.date).max(n.date),
            }
        } else {
            Item {
                what: What::National(&n.label),
                first: n.date,
                last: n.date,
            }
        }
    });
    let events = occs.iter().map(|o| {
        let (first, last) = span(
            o.event.all_day,
            o.occurrence_starts_at,
            o.occurrence_ends_at,
        );
        Item {
            what: What::Event(o),
            first,
            last,
        }
    });
    let mut out: Vec<Item> = generated.chain(events).collect();
    // A stable sort: a day's timed events keep the order they came in.
    out.sort_by_key(|i| {
        let rank = match i.what {
            What::Holiday(_) | What::National(_) => 0,
            What::Event(o) if o.event.all_day => 1,
            What::Event(_) => 2,
        };
        (i.last == i.first, i.first, std::cmp::Reverse(i.last), rank)
    });
    out
}

/// What `month_html` needs to know of the visitor and the families shown.
pub(crate) struct Viewer<'a> {
    /// The visitor: a bar concerns them or not.
    pub(crate) me: Uuid,
    /// The rosters of the families shown, for the bars' tints.
    pub(crate) members: &'a [GroupMember],
    /// The families shown (#622, `calendar::family_tags`).
    pub(crate) families: &'a [FamilyTag<'a>],
}

/// The month grid: `days`, whole weeks from a Monday, the days outside
/// `month` muted; `items` its bars (`items`); `title` its name, « novembre
/// 2026 ».
pub(crate) fn month_html(
    days: &[NaiveDate],
    items: &[Item],
    month: u32,
    today: NaiveDate,
    title: &str,
    viewer: &Viewer,
) -> String {
    // The weekdays: a capital initial on a phone, « lun. » on a PC — the
    // stylesheet capitalises the phone's. Each day's link names its
    // weekday in full, so the heads are not read.
    let heads: String = FR_WEEKDAYS
        .iter()
        .map(|d| {
            let d = d.to_lowercase();
            let (initial, rest) = d.split_at(1);
            format!(r#"<span>{initial}<span class="pc">{rest}.</span></span>"#)
        })
        .collect();
    let mut out = format!(
        r#"<div class="month" role="group" aria-label="{}"><div class="dh" aria-hidden="true">{heads}</div>"#,
        html_escape(title)
    );
    let spans: Vec<(NaiveDate, NaiveDate)> = items.iter().map(|i| (i.first, i.last)).collect();
    for week in days.chunks(7) {
        let slots = pack(week[0], &spans);
        out.push_str(r#"<div class="wk">"#);
        for (col, day) in week.iter().enumerate() {
            out.push_str(&day_link(*day, col, items, month, today, viewer.me));
            if col == 0 {
                out.push_str(&format!(
                    r#"<span class="wn pc c1" aria-hidden="true">S{}</span>"#,
                    day.iso_week().week()
                ));
            }
            // The bars that start on this column, in their lanes' order.
            for (item, slot) in items.iter().zip(&slots) {
                if let Some(slot) = slot.filter(|s| s.col == col && s.lane < PC_BARS) {
                    out.push_str(&bar_html(item, slot, viewer));
                }
            }
            // What the day does not show, on a phone and on a PC.
            for (cap, width) in [(PHONE_BARS, "phone"), (PC_BARS, "pc")] {
                let n = hidden(&slots, col, cap);
                if n > 0 {
                    out.push_str(&format!(
                        r#"<a class="plus {width} {lane} {col}" href="{href}">+{n}<span class="sr-only"> {autres}</span></a>"#,
                        lane = LANES[cap],
                        col = COLS[col],
                        href = day_href(*day),
                        autres = if n == 1 { "autre" } else { "autres" },
                    ));
                }
            }
        }
        out.push_str("</div>");
    }
    out.push_str("</div>");
    out
}

/// A day of the grid: a link to its day view as tall as its week, its
/// number at its head, named by what it carries (`day_label`). Today is
/// `aria-current="date"`; a day of another month, `.out`.
fn day_link(
    day: NaiveDate,
    col: usize,
    items: &[Item],
    month: u32,
    today: NaiveDate,
    me: Uuid,
) -> String {
    let (mut events, mut mine) = (0, 0);
    for item in items.iter().filter(|i| i.covers(day)) {
        if let What::Event(occ) = item.what {
            events += 1;
            mine += usize::from(concerns(&occ.event, me));
        }
    }
    format!(
        r#"<a class="day {col}{out}" href="{href}" aria-label="{label}"{current}><span>{n}</span></a>"#,
        col = COLS[col],
        out = if day.month() == month { "" } else { " out" },
        href = day_href(day),
        label = day_label(day, events, mine),
        current = if day == today {
            r#" aria-current="date""#
        } else {
            ""
        },
        n = day.day(),
    )
}

/// A bar in its slot: an event a link to its page, a national day or a
/// school holiday a `<span>`. The fourth lane is a PC's only (`.pc`).
fn bar_html(item: &Item, slot: Slot, viewer: &Viewer) -> String {
    let place = format!(
        "{} {}{}{}",
        LANES[slot.lane],
        COLS[slot.col],
        if slot.width > 1 { " " } else { "" },
        SPANS[slot.width.clamp(1, 7) - 1],
    ) + if slot.lane == PHONE_BARS { " pc" } else { "" };
    let occ = match item.what {
        What::Holiday(label) | What::National(label) => {
            return format!(
                r#"<span class="bar gen {place}">{}</span>"#,
                html_escape(label)
            );
        }
        What::Event(occ) => occ,
    };
    let e = &occ.event;
    let done = e.is_task && e.completed_at.is_some();
    let marker = match (e.is_task, done) {
        (true, true) => "☑ ",
        (true, false) => "☐ ",
        _ if e.rrule.is_some() => "↻ ",
        _ => "",
    };
    // Several days: the ocre. A day: the first member's tint, none on a
    // done task (`calendar::render_chips`), outlined when the event is
    // other members'.
    let look = if item.last > item.first {
        " gen".to_string()
    } else {
        let tint = match people(occ, viewer.members).first() {
            _ if done => " done".to_string(),
            Some(first) => format!(" {}", member_tint(first.user_id)),
            None => String::new(),
        };
        tint + if concerns(e, viewer.me) { "" } else { " oth" }
    };
    let family = family_of(occ, viewer.families);
    let (accent, family_name) = match family {
        Some(f) if f.worn => (
            format!(r#" data-accent="{}""#, f.accent.as_str()),
            format!(r#"<span class="sr-only"> ({})</span>"#, html_escape(f.name)),
        ),
        _ => (String::new(), String::new()),
    };
    let occ_param = occ.occurrence_starts_at.format("%Y-%m-%dT%H:%M:%SZ");
    format!(
        r#"<a class="bar{look} {place}" href="{href}"{accent}>{private}{marker}{title}{family_name}</a>"#,
        href = with_family(
            &format!("/agenda/{}?occ={occ_param}", e.id),
            family.and_then(|f| f.carried)
        ),
        private = private_mark(e.is_private),
        title = html_escape(&e.title),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use manage_our_home_shared::validation::agenda::month_grid;

    fn ymd(m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, d).unwrap()
    }

    /// An instant of November 2026 in Paris (UTC+1): `d` at `h`:`min`.
    fn paris(d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, d, h, min, 0).unwrap() - chrono::Duration::hours(1)
    }

    fn event(
        id: u128,
        title: &str,
        (starts, ends): (DateTime<Utc>, DateTime<Utc>),
        all_day: bool,
        assignees: &[u128],
    ) -> OccurrenceResponse {
        OccurrenceResponse {
            event: EventResponse {
                id: Uuid::from_u128(id),
                group_id: Uuid::nil(),
                created_by: Uuid::from_u128(99),
                title: title.to_string(),
                description: None,
                location: None,
                starts_at: starts,
                ends_at: ends,
                all_day,
                is_task: false,
                completed_at: None,
                rrule: None,
                assignee_ids: assignees.iter().map(|n| Uuid::from_u128(*n)).collect(),
                is_private: false,
            },
            occurrence_starts_at: starts,
            occurrence_ends_at: ends,
        }
    }

    /// A timed event of an hour on November `d` at `h`:00, for member 1.
    fn timed(id: u128, title: &str, d: u32, h: u32) -> OccurrenceResponse {
        event(id, title, (paris(d, h, 0), paris(d, h + 1, 0)), false, &[1])
    }

    /// An event on the whole days `from` to `to` of November, both included
    /// — stored from Paris midnight to the exclusive Paris midnight after
    /// its last day (#101).
    fn whole(id: u128, title: &str, from: u32, to: u32, assignees: &[u128]) -> OccurrenceResponse {
        event(
            id,
            title,
            (paris(from, 0, 0), paris(to + 1, 0, 0)),
            true,
            assignees,
        )
    }

    fn national(
        date: NaiveDate,
        kind: &str,
        label: &str,
        end: Option<NaiveDate>,
    ) -> NationalDayResponse {
        NationalDayResponse {
            date,
            end,
            kind: kind.to_string(),
            label: label.to_string(),
        }
    }

    // -- span ---------------------------------------------------------------

    #[test]
    fn a_timed_event_is_a_bar_on_the_day_it_starts() {
        assert_eq!(
            span(false, paris(12, 10, 0), paris(12, 11, 0)),
            (ymd(11, 12), ymd(11, 12))
        );
        // Past midnight, under a day: still its first day only.
        assert_eq!(
            span(false, paris(12, 23, 0), paris(13, 1, 0)),
            (ymd(11, 12), ymd(11, 12))
        );
        // An instant (a task with no length).
        assert_eq!(
            span(false, paris(12, 9, 0), paris(12, 9, 0)),
            (ymd(11, 12), ymd(11, 12))
        );
    }

    #[test]
    fn a_whole_day_event_covers_its_days_its_end_excluded() {
        assert_eq!(
            span(true, paris(11, 0, 0), paris(12, 0, 0)),
            (ymd(11, 11), ymd(11, 11))
        );
        assert_eq!(
            span(true, paris(11, 0, 0), paris(14, 0, 0)),
            (ymd(11, 11), ymd(11, 13))
        );
    }

    #[test]
    fn a_timed_event_of_a_day_or_more_covers_every_day_it_reaches() {
        // Wednesday 18:00 to Friday 10:00.
        assert_eq!(
            span(false, paris(11, 18, 0), paris(13, 10, 0)),
            (ymd(11, 11), ymd(11, 13))
        );
        // 24 hours to the minute, ending at midnight: not the day after.
        assert_eq!(
            span(false, paris(11, 0, 0), paris(12, 0, 0)),
            (ymd(11, 11), ymd(11, 11))
        );
        // 24 hours to the minute, from evening to evening: both days.
        assert_eq!(
            span(false, paris(11, 18, 0), paris(12, 18, 0)),
            (ymd(11, 11), ymd(11, 12))
        );
    }

    // -- pack ---------------------------------------------------------------

    /// Monday 9 November 2026.
    fn monday() -> NaiveDate {
        ymd(11, 9)
    }

    fn slot(col: usize, width: usize, lane: usize) -> Option<Slot> {
        Some(Slot { col, width, lane })
    }

    #[test]
    fn bars_of_one_day_stack_in_their_column() {
        let wed = ymd(11, 11);
        let slots = pack(
            monday(),
            &[(wed, wed), (wed, wed), (ymd(11, 12), ymd(11, 12))],
        );
        assert_eq!(slots, vec![slot(2, 1, 0), slot(2, 1, 1), slot(3, 1, 0)]);
    }

    #[test]
    fn a_bar_of_several_days_takes_one_lane_across_its_columns() {
        // Wednesday to Friday first, then a day under it, and a Monday
        // beside it that it does not cover, in the lane it leaves free.
        let slots = pack(
            monday(),
            &[
                (ymd(11, 11), ymd(11, 13)),
                (ymd(11, 12), ymd(11, 12)),
                (ymd(11, 9), ymd(11, 9)),
            ],
        );
        assert_eq!(slots, vec![slot(2, 3, 0), slot(3, 1, 1), slot(0, 1, 0)]);
    }

    #[test]
    fn a_bar_takes_the_lowest_lane_free_on_all_its_columns() {
        // Tuesday is taken on lane 0, so Monday–Wednesday goes to lane 1
        // although Monday and Wednesday are free on lane 0.
        let slots = pack(
            monday(),
            &[(ymd(11, 10), ymd(11, 10)), (ymd(11, 9), ymd(11, 11))],
        );
        assert_eq!(slots, vec![slot(1, 1, 0), slot(0, 3, 1)]);
    }

    #[test]
    fn a_bar_is_cut_at_the_edges_of_the_week_and_continues_on_the_next() {
        // Saturday 7 to Wednesday 18 November.
        let long = (ymd(11, 7), ymd(11, 18));
        assert_eq!(pack(ymd(11, 2), &[long]), vec![slot(5, 2, 0)]);
        assert_eq!(pack(monday(), &[long]), vec![slot(0, 7, 0)]);
        assert_eq!(pack(ymd(11, 16), &[long]), vec![slot(0, 3, 0)]);
    }

    #[test]
    fn a_bar_outside_the_week_has_no_slot_and_takes_no_lane() {
        let slots = pack(
            monday(),
            &[
                (ymd(11, 2), ymd(11, 8)),
                (ymd(11, 16), ymd(11, 16)),
                (ymd(11, 9), ymd(11, 9)),
            ],
        );
        assert_eq!(slots, vec![None, None, slot(0, 1, 0)]);
    }

    // -- hidden -------------------------------------------------------------

    #[test]
    fn a_day_counts_the_bars_from_its_cap_down() {
        let wed = ymd(11, 11);
        let slots = pack(monday(), &[(wed, wed); 6]);
        assert_eq!(hidden(&slots, 2, PHONE_BARS), 3);
        assert_eq!(hidden(&slots, 2, PC_BARS), 2);
        assert_eq!(hidden(&slots, 3, PHONE_BARS), 0);
    }

    #[test]
    fn a_bar_of_several_days_is_counted_on_every_day_it_covers() {
        let wed = ymd(11, 11);
        let thu = ymd(11, 12);
        // Three bars on Wednesday, then Wednesday–Thursday on lane 3.
        let slots = pack(monday(), &[(wed, wed), (wed, wed), (wed, wed), (wed, thu)]);
        assert_eq!(slots[3], slot(2, 2, 3));
        assert_eq!(hidden(&slots, 2, PHONE_BARS), 1);
        assert_eq!(hidden(&slots, 3, PHONE_BARS), 1);
        assert_eq!(hidden(&slots, 3, PC_BARS), 0);
    }

    // -- concerns -----------------------------------------------------------

    #[test]
    fn an_event_concerns_its_assignees() {
        let e = timed(1, "Dentiste", 12, 10);
        assert!(concerns(&e.event, Uuid::from_u128(1)));
        assert!(!concerns(&e.event, Uuid::from_u128(2)));
        let shared = event(
            2,
            "Réunion",
            (paris(12, 10, 0), paris(12, 11, 0)),
            false,
            &[2, 3],
        );
        assert!(concerns(&shared.event, Uuid::from_u128(3)));
        // Its creator, when it names assignees, is not one of them.
        assert!(!concerns(&shared.event, Uuid::from_u128(99)));
    }

    #[test]
    fn an_event_without_assignees_concerns_its_creator() {
        let e = event(
            1,
            "Courses",
            (paris(12, 10, 0), paris(12, 11, 0)),
            false,
            &[],
        );
        assert!(concerns(&e.event, Uuid::from_u128(99)));
        assert!(!concerns(&e.event, Uuid::from_u128(1)));
    }

    // -- day_label ----------------------------------------------------------

    #[test]
    fn a_day_names_its_events_and_those_that_concern_the_visitor() {
        let thu = ymd(11, 12);
        assert_eq!(
            day_label(thu, 7, 3),
            "jeudi 12 novembre, 7 événements, 3 qui te concernent"
        );
        assert_eq!(
            day_label(thu, 1, 1),
            "jeudi 12 novembre, 1 événement, 1 qui te concerne"
        );
        assert_eq!(
            day_label(thu, 2, 0),
            "jeudi 12 novembre, 2 événements, aucun ne te concerne"
        );
        assert_eq!(day_label(thu, 0, 0), "jeudi 12 novembre, rien de prévu");
    }

    // -- items --------------------------------------------------------------

    fn names<'a>(items: &[Item<'a>]) -> Vec<&'a str> {
        items
            .iter()
            .map(|i| match i.what {
                What::Holiday(l) | What::National(l) => l,
                What::Event(o) => o.event.title.as_str(),
            })
            .collect()
    }

    #[test]
    fn bars_of_several_days_come_first_then_a_days_own() {
        let occs = vec![
            timed(1, "Matin", 11, 8),
            whole(2, "Journée", 11, 11, &[1]),
            whole(3, "Séjour", 11, 13, &[1]),
            whole(4, "Long séjour", 10, 14, &[1]),
            timed(5, "Soir", 11, 20),
        ];
        let nat = vec![
            national(ymd(11, 11), "public_holiday", "Armistice", None),
            national(ymd(11, 12), SCHOOL_HOLIDAY, "Vacances", Some(ymd(11, 20))),
        ];
        let items = items(&occs, &nat);
        assert_eq!(
            names(&items),
            [
                "Long séjour",
                "Séjour",
                "Vacances",
                "Armistice",
                "Journée",
                "Matin",
                "Soir"
            ]
        );
        let holiday = items
            .iter()
            .find(|i| matches!(i.what, What::Holiday(_)))
            .unwrap();
        assert_eq!((holiday.first, holiday.last), (ymd(11, 12), ymd(11, 20)));
    }

    #[test]
    fn of_bars_of_several_days_starting_together_the_longest_comes_first() {
        let occs = vec![
            whole(1, "Court", 11, 12, &[1]),
            whole(2, "Long", 11, 14, &[1]),
        ];
        assert_eq!(names(&items(&occs, &[])), ["Long", "Court"]);
    }

    // -- month_html ---------------------------------------------------------

    fn november() -> Vec<NaiveDate> {
        month_grid(2026, 11)
    }

    fn render(occs: &[OccurrenceResponse], nat: &[NationalDayResponse], me: u128) -> String {
        let items = items(occs, nat);
        month_html(
            &november(),
            &items,
            11,
            ymd(11, 12),
            "novembre 2026",
            &Viewer {
                me: Uuid::from_u128(me),
                members: &[],
                families: &[],
            },
        )
    }

    /// The opening tag of the first element whose text is `text`.
    fn tag_of(html: &str, text: &str) -> String {
        let end = html
            .find(&format!(">{text}<"))
            .unwrap_or_else(|| panic!("no {text} in {html}"));
        let start = html[..end].rfind('<').unwrap();
        html[start..=end].to_string()
    }

    #[test]
    fn the_grid_is_named_by_its_month_and_heads_its_columns() {
        let html = render(&[], &[], 1);
        assert!(
            html.starts_with(r#"<div class="month" role="group" aria-label="novembre 2026">"#),
            "{html}"
        );
        assert_eq!(html.matches(r#"<div class="wk">"#).count(), 6, "{html}");
        assert!(
            html.contains(r#"<span>l<span class="pc">un.</span></span>"#),
            "{html}"
        );
    }

    #[test]
    fn every_day_is_a_link_to_its_day_view_named_by_what_it_carries() {
        let occs = vec![timed(1, "Dentiste", 12, 10), timed(2, "Foot", 12, 17)];
        let html = render(&occs, &[], 1);
        assert_eq!(html.matches(r#"<a class="day "#).count(), 42, "{html}");
        let thu = tag_of(&html, "<span>12</span>");
        assert!(
            thu.contains(r#"href="/agenda?view=day&date=2026-11-12""#),
            "{thu}"
        );
        assert!(
            thu.contains(r#"aria-label="jeudi 12 novembre, 2 événements, 2 qui te concernent""#),
            "{thu}"
        );
        // Today, by class for the eye and by attribute for the ear.
        assert!(thu.contains(r#"aria-current="date""#), "{thu}");
        assert!(thu.contains(" c4"), "{thu}");
        // A day of October, muted; the 13th, not today.
        assert!(tag_of(&html, "<span>26</span>").contains(" out"), "{html}");
        assert!(
            !tag_of(&html, "<span>13</span>").contains("aria-current"),
            "{html}"
        );
    }

    #[test]
    fn a_pc_reads_each_weeks_number() {
        let html = render(&[], &[], 1);
        assert!(
            html.contains(r#"<span class="wn pc c1" aria-hidden="true">S44</span>"#),
            "{html}"
        );
        assert!(html.contains(">S49<"), "{html}");
    }

    #[test]
    fn an_event_of_the_visitor_is_filled_another_members_outlined() {
        let occs = vec![timed(1, "Dentiste", 12, 10), timed(2, "Foot", 13, 17)];
        // Member 1 is the visitor: both events are member 1's.
        let html = render(&occs, &[], 1);
        let mine = tag_of(&html, "Dentiste");
        assert!(mine.starts_with(r#"<a class="bar "#), "{mine}");
        assert!(!mine.contains(" oth"), "{mine}");
        assert!(mine.contains(" l0 c4"), "{mine}");
        // Member 2 visits: neither is theirs.
        let html = render(&occs, &[], 2);
        assert!(tag_of(&html, "Dentiste").contains(" oth"), "{html}");
        assert!(tag_of(&html, "Foot").contains(" c5"), "{html}");
    }

    #[test]
    fn an_event_of_several_days_is_one_ocre_bar_across_its_columns_cut_at_the_week() {
        // Saturday 14 to Tuesday 17 November.
        let occs = vec![whole(1, "Séjour chez Papi", 14, 17, &[1])];
        let html = render(&occs, &[], 1);
        let bars: Vec<_> = html.match_indices(">Séjour chez Papi<").collect();
        assert_eq!(bars.len(), 2, "{html}");
        let first = tag_of(&html, "Séjour chez Papi");
        assert!(first.contains(" gen"), "{first}");
        assert!(first.contains(" l0 c6 s2"), "{first}");
        let rest = &html[bars[0].0 + 1..];
        let second = tag_of(rest, "Séjour chez Papi");
        assert!(second.contains(" l0 c1 s2"), "{second}");
        // Counted on every day it covers.
        assert!(
            html.contains(r#"aria-label="dimanche 15 novembre, 1 événement, 1 qui te concerne""#),
            "{html}"
        );
    }

    #[test]
    fn a_recurring_event_and_a_task_say_so_before_their_title() {
        let mut series = timed(1, "Yoga", 12, 10);
        series.event.rrule = Some("FREQ=WEEKLY".to_string());
        let mut task = timed(2, "Ménage", 13, 10);
        task.event.is_task = true;
        let mut done = timed(3, "Linge", 14, 10);
        done.event.is_task = true;
        done.event.completed_at = Some(paris(14, 12, 0));
        let html = render(&[series, task, done], &[], 1);
        assert!(html.contains(">↻ Yoga<"), "{html}");
        assert!(html.contains(">☐ Ménage<"), "{html}");
        let linge = tag_of(&html, "☑ Linge");
        assert!(linge.contains(" done"), "{linge}");
        assert!(!linge.contains(" m"), "a done task loses its tint: {linge}");
    }

    #[test]
    fn a_national_day_and_a_school_holiday_are_ocre_text_not_links() {
        let nat = vec![
            national(ymd(11, 11), "public_holiday", "Armistice", None),
            national(ymd(11, 19), SCHOOL_HOLIDAY, "Vacances", Some(ymd(11, 24))),
        ];
        let html = render(&[], &nat, 1);
        let armistice = tag_of(&html, "Armistice");
        assert!(
            armistice.starts_with(r#"<span class="bar gen l0 c3""#),
            "{armistice}"
        );
        assert!(
            tag_of(&html, "Vacances").starts_with(r#"<span class="bar gen l0 c4 s4""#),
            "{html}"
        );
        assert_eq!(html.matches(">Vacances<").count(), 2, "{html}");
        // Not an event of the family.
        assert!(
            html.contains(r#"aria-label="mercredi 11 novembre, rien de prévu""#),
            "{html}"
        );
    }

    #[test]
    fn a_crowded_day_shows_three_bars_on_a_phone_four_on_a_pc_and_counts_the_rest() {
        let occs: Vec<_> = (0..7)
            .map(|n| timed(n, &format!("E{n}"), 12, 8 + n as u32))
            .collect();
        let html = render(&occs, &[], 1);
        assert!(!tag_of(&html, "E2").contains(" pc"), "{html}");
        assert!(tag_of(&html, "E3").contains(" l3 c4 pc"), "{html}");
        assert!(!html.contains(">E4<"), "{html}");
        let href = r#"href="/agenda?view=day&date=2026-11-12""#;
        assert!(
            html.contains(&format!(
                r#"<a class="plus phone l3 c4" {href}>+4<span class="sr-only"> autres</span></a>"#
            )),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"<a class="plus pc l4 c4" {href}>+3<span class="sr-only"> autres</span></a>"#
            )),
            "{html}"
        );
    }

    #[test]
    fn one_hidden_bar_is_one_autre() {
        let occs: Vec<_> = (0..4)
            .map(|n| timed(n, &format!("E{n}"), 12, 8 + n as u32))
            .collect();
        let html = render(&occs, &[], 1);
        assert!(
            html.contains(r#">+1<span class="sr-only"> autre</span></a>"#),
            "{html}"
        );
        assert!(!html.contains(r#"class="plus pc"#), "{html}");
    }

    #[test]
    fn a_title_is_escaped() {
        let html = render(&[timed(1, "<b>Fête</b>", 12, 10)], &[], 1);
        assert!(html.contains("&lt;b&gt;Fête&lt;/b&gt;"), "{html}");
        assert!(!html.contains("<b>Fête"), "{html}");
    }
}
