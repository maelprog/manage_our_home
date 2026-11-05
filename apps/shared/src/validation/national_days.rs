//! The "jours nationaux" of the agenda (#562): the French public holidays,
//! the two clock changes and Mother's and Father's Day, computed for a range
//! of civil dates. Pure calendar maths — no storage, no external call —
//! served by `GET /groups/:id/national-days` (`apps/api/src/agenda/`).
//!
//! Metropolitan France only: the overseas departments and the local holidays
//! other than Alsace-Moselle's are out of scope (#562).

use chrono::{Datelike, Duration, NaiveDate, Weekday};

/// What a national day is, as the API spells it in `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NationalDayKind {
    /// A public holiday ("jour férié").
    PublicHoliday,
    /// A switch to or from summer time.
    ClockChange,
    /// A day marked but not off: Mother's and Father's Day.
    Observance,
}

impl NationalDayKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NationalDayKind::PublicHoliday => "public_holiday",
            NationalDayKind::ClockChange => "clock_change",
            NationalDayKind::Observance => "observance",
        }
    }
}

/// One national day: its civil date, its kind and its French label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NationalDay {
    pub date: NaiveDate,
    pub kind: NationalDayKind,
    pub label: &'static str,
}

/// The widest range `GET /groups/:id/national-days` serves, in days counted
/// inclusively: a whole leap year. Wide enough for a year view, the month
/// grid's 42 days and the week; anything wider is a 400 (`range_too_wide`).
pub const MAX_NATIONAL_DAYS_SPAN_DAYS: i64 = 366;

/// The checks `GET /groups/:id/national-days?from&to` applies to its range,
/// both ends included: `to` before `from` is `to_before_from`, more than
/// [`MAX_NATIONAL_DAYS_SPAN_DAYS`] days is `range_too_wide`.
pub fn validate_national_days_range(from: NaiveDate, to: NaiveDate) -> Result<(), &'static str> {
    if to < from {
        return Err("to_before_from");
    }
    if (to - from).num_days() >= MAX_NATIONAL_DAYS_SPAN_DAYS {
        return Err("range_too_wide");
    }
    Ok(())
}

/// Easter Sunday of `year` (Gregorian computus, Meeus/Jones/Butcher).
/// `None` only for a year chrono cannot represent.
pub fn easter_sunday(year: i32) -> Option<NaiveDate> {
    // Euclidean division throughout, so a year chrono can hold but the
    // computus was never meant for (a negative one) still yields a date
    // rather than a panic or a nonsense month.
    let y = i64::from(year);
    let a = y.rem_euclid(19);
    let (b, c) = (y.div_euclid(100), y.rem_euclid(100));
    let (d, e) = (b.div_euclid(4), b.rem_euclid(4));
    let f = (b + 8).div_euclid(25);
    let g = (b - f + 1).div_euclid(3);
    let h = (19 * a + b - d - g + 15).rem_euclid(30);
    let (i, k) = (c.div_euclid(4), c.rem_euclid(4));
    let l = (32 + 2 * e + 2 * i - h - k).rem_euclid(7);
    let m = (a + 11 * h + 22 * l).div_euclid(451);
    let month = (h + l - 7 * m + 114).div_euclid(31);
    let day = (h + l - 7 * m + 114).rem_euclid(31) + 1;
    NaiveDate::from_ymd_opt(year, u32::try_from(month).ok()?, u32::try_from(day).ok()?)
}

/// The last `weekday` of `month` in `year`.
fn last_weekday_of(year: i32, month: u32, weekday: Weekday) -> Option<NaiveDate> {
    let (next_year, next_month) = if month == 12 {
        (year.checked_add(1)?, 1)
    } else {
        (year, month + 1)
    };
    let last = NaiveDate::from_ymd_opt(next_year, next_month, 1)?.pred_opt()?;
    let back = (7 + last.weekday().num_days_from_monday() - weekday.num_days_from_monday()) % 7;
    last.checked_sub_signed(Duration::days(i64::from(back)))
}

/// Every national day of `year`, fixed holidays first, in no promised order.
fn days_of_year(year: i32, alsace_moselle: bool) -> Vec<NationalDay> {
    use NationalDayKind::{ClockChange, Observance, PublicHoliday};
    let on = |m: u32, day: u32| NaiveDate::from_ymd_opt(year, m, day);
    let easter = easter_sunday(year);
    let after_easter = |n: i64| easter.and_then(|e| e.checked_add_signed(Duration::days(n)));
    let pentecost = after_easter(49);

    // Fête des mères: the last Sunday of May, moved to the first Sunday of June when that Sunday is
    // Pentecôte.
    let mothers_day = last_weekday_of(year, 5, Weekday::Sun).and_then(|sunday| {
        if Some(sunday) == pentecost {
            NaiveDate::from_weekday_of_month_opt(year, 6, Weekday::Sun, 1)
        } else {
            Some(sunday)
        }
    });

    let mut days: Vec<(Option<NaiveDate>, NationalDayKind, &'static str)> = vec![
        (on(1, 1), PublicHoliday, "Jour de l'an"),
        (on(5, 1), PublicHoliday, "Fête du Travail"),
        (on(5, 8), PublicHoliday, "Victoire 1945"),
        (on(7, 14), PublicHoliday, "Fête nationale"),
        (on(8, 15), PublicHoliday, "Assomption"),
        (on(11, 1), PublicHoliday, "Toussaint"),
        (on(11, 11), PublicHoliday, "Armistice 1918"),
        (on(12, 25), PublicHoliday, "Noël"),
        (after_easter(1), PublicHoliday, "Lundi de Pâques"),
        (after_easter(39), PublicHoliday, "Ascension"),
        (after_easter(50), PublicHoliday, "Lundi de Pentecôte"),
        // The EU rule (directive 2000/84/EC): the last Sundays of March
        // and October.
        (
            last_weekday_of(year, 3, Weekday::Sun),
            ClockChange,
            "Passage à l'heure d'été",
        ),
        (
            last_weekday_of(year, 10, Weekday::Sun),
            ClockChange,
            "Passage à l'heure d'hiver",
        ),
        (mothers_day, Observance, "Fête des mères"),
        (
            NaiveDate::from_weekday_of_month_opt(year, 6, Weekday::Sun, 3),
            Observance,
            "Fête des pères",
        ),
    ];
    if alsace_moselle {
        days.push((after_easter(-2), PublicHoliday, "Vendredi saint"));
        days.push((on(12, 26), PublicHoliday, "Saint-Étienne"));
    }
    days.into_iter()
        .filter_map(|(date, kind, label)| {
            Some(NationalDay {
                date: date?,
                kind,
                label,
            })
        })
        .collect()
}

/// The national days falling in `[from, to]`, both ends included, sorted by
/// date. With `alsace_moselle`, Good Friday and 26 December are holidays too.
///
/// Every year the range touches is computed, so a caller bounds the range
/// first ([`validate_national_days_range`]).
pub fn national_days(from: NaiveDate, to: NaiveDate, alsace_moselle: bool) -> Vec<NationalDay> {
    if to < from {
        return Vec::new();
    }
    let mut days: Vec<NationalDay> = (from.year()..=to.year())
        .flat_map(|year| days_of_year(year, alsace_moselle))
        .filter(|n| n.date >= from && n.date <= to)
        .collect();
    // Stable: two days on the same date keep the order above, fixed
    // holiday first (1 May before an Ascension falling on it).
    days.sort_by_key(|n| n.date);
    days
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn year(y: i32, alsace_moselle: bool) -> Vec<NationalDay> {
        national_days(d(y, 1, 1), d(y, 12, 31), alsace_moselle)
    }

    /// The dates carrying `label` in year `y`.
    fn dates_of(y: i32, label: &str, alsace_moselle: bool) -> Vec<NaiveDate> {
        year(y, alsace_moselle)
            .into_iter()
            .filter(|n| n.label == label)
            .map(|n| n.date)
            .collect()
    }

    fn holidays(y: i32, alsace_moselle: bool) -> Vec<(NaiveDate, &'static str)> {
        year(y, alsace_moselle)
            .into_iter()
            .filter(|n| n.kind == NationalDayKind::PublicHoliday)
            .map(|n| (n.date, n.label))
            .collect()
    }

    // -- easter_sunday ------------------------------------------------------

    #[test]
    fn easter_falls_on_its_known_dates() {
        assert_eq!(easter_sunday(2027), Some(d(2027, 3, 28)));
        assert_eq!(easter_sunday(2024), Some(d(2024, 3, 31)));
        assert_eq!(easter_sunday(2025), Some(d(2025, 4, 20)));
        assert_eq!(easter_sunday(2026), Some(d(2026, 4, 5)));
        assert_eq!(easter_sunday(2019), Some(d(2019, 4, 21)));
        assert_eq!(easter_sunday(2000), Some(d(2000, 4, 23)));
        assert_eq!(easter_sunday(1900), Some(d(1900, 4, 15)));
    }

    /// The computus' two extremes: 22 March (2285) and 25 April (2038).
    #[test]
    fn easter_reaches_both_ends_of_its_window() {
        assert_eq!(easter_sunday(2285), Some(d(2285, 3, 22)));
        assert_eq!(easter_sunday(2038), Some(d(2038, 4, 25)));
    }

    #[test]
    fn easter_is_always_a_sunday_between_22_march_and_25_april() {
        for y in 1583..=2600 {
            let easter = easter_sunday(y).unwrap();
            assert_eq!(easter.weekday(), Weekday::Sun, "{y}");
            assert!(easter >= d(y, 3, 22) && easter <= d(y, 4, 25), "{y}");
        }
    }

    // -- public holidays ----------------------------------------------------

    #[test]
    fn the_eleven_national_holidays_of_2026() {
        assert_eq!(
            holidays(2026, false),
            vec![
                (d(2026, 1, 1), "Jour de l'an"),
                (d(2026, 4, 6), "Lundi de Pâques"),
                (d(2026, 5, 1), "Fête du Travail"),
                (d(2026, 5, 8), "Victoire 1945"),
                (d(2026, 5, 14), "Ascension"),
                (d(2026, 5, 25), "Lundi de Pentecôte"),
                (d(2026, 7, 14), "Fête nationale"),
                (d(2026, 8, 15), "Assomption"),
                (d(2026, 11, 1), "Toussaint"),
                (d(2026, 11, 11), "Armistice 1918"),
                (d(2026, 12, 25), "Noël"),
            ]
        );
    }

    #[test]
    fn the_movable_holidays_follow_easter_over_several_years() {
        assert_eq!(dates_of(2026, "Ascension", false), [d(2026, 5, 14)]);
        assert_eq!(dates_of(2027, "Lundi de Pâques", false), [d(2027, 3, 29)]);
        assert_eq!(dates_of(2027, "Ascension", false), [d(2027, 5, 6)]);
        // Pentecôte 2027 is Sunday 16 May; the holiday is the Monday after.
        assert_eq!(
            dates_of(2027, "Lundi de Pentecôte", false),
            [d(2027, 5, 17)]
        );
        assert_eq!(dates_of(2024, "Lundi de Pâques", false), [d(2024, 4, 1)]);
        assert_eq!(dates_of(2024, "Ascension", false), [d(2024, 5, 9)]);
        assert_eq!(
            dates_of(2024, "Lundi de Pentecôte", false),
            [d(2024, 5, 20)]
        );
        assert_eq!(dates_of(2025, "Lundi de Pâques", false), [d(2025, 4, 21)]);
        assert_eq!(dates_of(2025, "Ascension", false), [d(2025, 5, 29)]);
        assert_eq!(dates_of(2025, "Lundi de Pentecôte", false), [d(2025, 6, 9)]);
    }

    /// Ascension 2008 fell on 1 May: both holidays are listed, the day twice.
    #[test]
    fn two_holidays_on_the_same_day_are_both_listed() {
        let first_of_may: Vec<&str> = year(2008, false)
            .into_iter()
            .filter(|n| n.date == d(2008, 5, 1))
            .map(|n| n.label)
            .collect();
        assert_eq!(first_of_may, ["Fête du Travail", "Ascension"]);
    }

    #[test]
    fn good_friday_and_26_december_only_with_alsace_moselle() {
        for y in [2024, 2026, 2027] {
            assert!(dates_of(y, "Vendredi saint", false).is_empty(), "{y}");
            assert!(dates_of(y, "Saint-Étienne", false).is_empty(), "{y}");
            assert_eq!(holidays(y, false).len(), 11, "{y}");
            assert_eq!(dates_of(y, "Saint-Étienne", true), [d(y, 12, 26)], "{y}");
            assert_eq!(holidays(y, true).len(), 13, "{y}");
        }
        assert_eq!(dates_of(2026, "Vendredi saint", true), [d(2026, 4, 3)]);
        assert_eq!(dates_of(2027, "Vendredi saint", true), [d(2027, 3, 26)]);
        assert_eq!(dates_of(2024, "Vendredi saint", true), [d(2024, 3, 29)]);
    }

    #[test]
    fn alsace_moselle_holidays_are_public_holidays() {
        for n in year(2026, true) {
            if n.label == "Vendredi saint" || n.label == "Saint-Étienne" {
                assert_eq!(n.kind, NationalDayKind::PublicHoliday);
            }
        }
    }

    // -- clock changes ------------------------------------------------------

    #[test]
    fn the_clocks_change_on_the_last_sundays_of_march_and_october() {
        let changes = |y| -> Vec<(NaiveDate, &'static str)> {
            year(y, false)
                .into_iter()
                .filter(|n| n.kind == NationalDayKind::ClockChange)
                .map(|n| (n.date, n.label))
                .collect()
        };
        assert_eq!(
            changes(2026),
            vec![
                (d(2026, 3, 29), "Passage à l'heure d'été"),
                (d(2026, 10, 25), "Passage à l'heure d'hiver"),
            ]
        );
        assert_eq!(
            changes(2025),
            vec![
                (d(2025, 3, 30), "Passage à l'heure d'été"),
                (d(2025, 10, 26), "Passage à l'heure d'hiver"),
            ]
        );
        // 31 March and 31 October are themselves the last Sundays.
        assert_eq!(
            changes(2024),
            vec![
                (d(2024, 3, 31), "Passage à l'heure d'été"),
                (d(2024, 10, 27), "Passage à l'heure d'hiver"),
            ]
        );
        assert_eq!(
            changes(2027),
            vec![
                (d(2027, 3, 28), "Passage à l'heure d'été"),
                (d(2027, 10, 31), "Passage à l'heure d'hiver"),
            ]
        );
    }

    // -- Mother's and Father's Day ------------------------------------------

    #[test]
    fn mothers_day_is_the_last_sunday_of_may() {
        assert_eq!(dates_of(2024, "Fête des mères", false), [d(2024, 5, 26)]);
        assert_eq!(dates_of(2025, "Fête des mères", false), [d(2025, 5, 25)]);
        assert_eq!(dates_of(2026, "Fête des mères", false), [d(2026, 5, 31)]);
        assert_eq!(dates_of(2027, "Fête des mères", false), [d(2027, 5, 30)]);
    }

    /// Pentecôte 2023 was Sunday 28 May, 2012's Sunday 27 May: both the last
    /// Sunday of May, so Mother's Day moved to the first Sunday of June.
    #[test]
    fn mothers_day_moves_to_june_when_it_meets_pentecost() {
        assert_eq!(dates_of(2023, "Fête des mères", false), [d(2023, 6, 4)]);
        assert_eq!(dates_of(2012, "Fête des mères", false), [d(2012, 6, 3)]);
    }

    #[test]
    fn fathers_day_is_the_third_sunday_of_june() {
        assert_eq!(dates_of(2024, "Fête des pères", false), [d(2024, 6, 16)]);
        assert_eq!(dates_of(2025, "Fête des pères", false), [d(2025, 6, 15)]);
        assert_eq!(dates_of(2026, "Fête des pères", false), [d(2026, 6, 21)]);
        assert_eq!(dates_of(2027, "Fête des pères", false), [d(2027, 6, 20)]);
        // 1 June 2025 is a Sunday: the third is the 15th, not the 22nd.
    }

    #[test]
    fn mothers_and_fathers_day_are_observances() {
        for n in year(2026, false) {
            if n.label.starts_with("Fête des") {
                assert_eq!(n.kind, NationalDayKind::Observance);
            }
        }
        assert_eq!(
            year(2026, false)
                .iter()
                .filter(|n| n.kind == NationalDayKind::Observance)
                .count(),
            2
        );
    }

    // -- the range ----------------------------------------------------------

    #[test]
    fn a_year_holds_fifteen_days_and_seventeen_in_alsace_moselle() {
        assert_eq!(year(2026, false).len(), 15);
        assert_eq!(year(2026, true).len(), 17);
    }

    #[test]
    fn days_are_sorted_by_date() {
        let days = national_days(d(2025, 11, 1), d(2027, 2, 1), true);
        assert!(days.windows(2).all(|w| w[0].date <= w[1].date));
    }

    #[test]
    fn both_bounds_are_included_and_nothing_outside_is() {
        let days = national_days(d(2026, 5, 1), d(2026, 5, 14), false);
        let labels: Vec<&str> = days.iter().map(|n| n.label).collect();
        assert_eq!(labels, ["Fête du Travail", "Victoire 1945", "Ascension"]);
        assert!(national_days(d(2026, 5, 2), d(2026, 5, 7), false).is_empty());
    }

    #[test]
    fn a_range_across_new_year_takes_from_both_years() {
        let days = national_days(d(2026, 12, 20), d(2027, 1, 5), true);
        assert_eq!(
            days.iter().map(|n| (n.date, n.label)).collect::<Vec<_>>(),
            vec![
                (d(2026, 12, 25), "Noël"),
                (d(2026, 12, 26), "Saint-Étienne"),
                (d(2027, 1, 1), "Jour de l'an"),
            ]
        );
    }

    #[test]
    fn a_single_day_range_and_a_backwards_one() {
        assert_eq!(
            national_days(d(2026, 7, 14), d(2026, 7, 14), false),
            vec![NationalDay {
                date: d(2026, 7, 14),
                kind: NationalDayKind::PublicHoliday,
                label: "Fête nationale",
            }]
        );
        assert!(national_days(d(2026, 7, 15), d(2026, 7, 14), false).is_empty());
    }

    #[test]
    fn the_last_representable_years_do_not_panic() {
        let _ = national_days(NaiveDate::MAX - Duration::days(400), NaiveDate::MAX, true);
        let _ = national_days(NaiveDate::MIN, NaiveDate::MIN + Duration::days(400), true);
    }

    #[test]
    fn kinds_are_spelled_for_the_api() {
        assert_eq!(NationalDayKind::PublicHoliday.as_str(), "public_holiday");
        assert_eq!(NationalDayKind::ClockChange.as_str(), "clock_change");
        assert_eq!(NationalDayKind::Observance.as_str(), "observance");
    }

    // -- validate_national_days_range ---------------------------------------

    #[test]
    fn a_whole_leap_year_is_the_widest_range() {
        assert_eq!(
            validate_national_days_range(d(2028, 1, 1), d(2028, 12, 31)),
            Ok(())
        );
        assert_eq!(
            validate_national_days_range(d(2028, 1, 1), d(2029, 1, 1)),
            Err("range_too_wide")
        );
        assert_eq!(
            validate_national_days_range(d(2026, 7, 14), d(2026, 7, 14)),
            Ok(())
        );
    }

    #[test]
    fn a_backwards_range_is_refused() {
        assert_eq!(
            validate_national_days_range(d(2026, 7, 15), d(2026, 7, 14)),
            Err("to_before_from")
        );
    }
}
