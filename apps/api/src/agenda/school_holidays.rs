//! School holidays of zones A, B and C (#563), read from the Éducation
//! nationale's open data — the `fr-en-calendrier-scolaire` dataset of
//! data.education.gouv.fr, under the Licence Ouverte (Etalab) — and kept in
//! the `school_holidays` cache (migration 0047). `jobs::school_holidays`
//! refreshes the cache once a day; `GET /groups/:id/national-days` reads it
//! for the family's `school_zone` (#561). Nothing personal is sent or
//! received: the request names two school years and three zones.
//!
//! What the dataset says, as read on 2026-11-05: one record per académie
//! and period, so the same period comes once per académie of its zone;
//! `start_date` is the first day off and `end_date` the day classes resume,
//! both as Paris midnight in UTC (`2026-10-16T22:00:00+00:00` is 17
//! October). A period whose end is not published yet (« Début des Vacances
//! d'Été » of the year after next) has `end_date` equal to `start_date`.
//! Summer comes twice, for pupils (`Élèves`) and teachers (`Enseignants`);
//! the others say `-`. The Ascension bridge is listed as `Pont de
//! l'Ascension`.
//!
//! Kept: the pupils' periods (`-` and `Élèves`), without the bridges, each
//! as its official label and its first and last day off, both included.
//! Teachers' dates and bridges are out of scope (#563).

use std::cmp::Ordering;
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Duration, NaiveDate};
use manage_our_home_shared::validation::auth::paris_day;
use manage_our_home_shared::validation::groups::SchoolZone;
use serde_json::Value;
use sqlx::PgPool;

/// Production base URL. `SCHOOL_CALENDAR_BASE_URL` overrides it (unset or
/// empty: this one).
pub const DEFAULT_BASE_URL: &str = "https://data.education.gouv.fr";

/// The environment variable that overrides `DEFAULT_BASE_URL`.
pub const BASE_URL_ENV: &str = "SCHOOL_CALENDAR_BASE_URL";

/// The dataset's records endpoint (Opendatasoft Explore API v2.1).
pub const RECORDS_PATH: &str =
    "/api/explore/v2.1/catalog/datasets/fr-en-calendrier-scolaire/records";

/// Rows asked for in one request. Grouped by period, two school years of
/// three zones come to 39 rows (2026-2027 and 2027-2028, read on
/// 2026-11-05); an answer that says it holds more than it returned is
/// refused rather than cached cut short.
pub const PAGE_LIMIT: usize = 100;

/// One period off school for one zone, as cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchoolHoliday {
    pub zone: SchoolZone,
    /// The dataset's `description`, trimmed: « Vacances de Noël »…
    pub label: String,
    /// First day off.
    pub starts_on: NaiveDate,
    /// Last day off, included: the day before classes resume, or
    /// `starts_on` when the dataset does not give the end yet.
    pub ends_on: NaiveDate,
    /// `2026-2027`…
    pub school_year: String,
}

/// Why an answer is not cached. The cache is left as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalendarError {
    /// Not an object with a `results` array.
    UnexpectedShape,
    /// `total_count` says more rows exist than were returned.
    Truncated { total: u64, returned: usize },
    /// A kept row lacks a field, or holds one of the wrong type or form.
    BadField { row: usize, field: &'static str },
    /// A kept row ends before it starts.
    EndBeforeStart { row: usize },
    /// Two rows give the same period of a zone different ends.
    Conflict {
        zone: SchoolZone,
        label: String,
        starts_on: NaiveDate,
    },
    /// A zone has no period at all in the current school year.
    MissingZone {
        zone: SchoolZone,
        school_year: String,
    },
}

impl std::fmt::Display for CalendarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CalendarError::UnexpectedShape => write!(f, "no `results` array in the answer"),
            CalendarError::Truncated { total, returned } => {
                write!(f, "{returned} rows returned of {total}")
            }
            CalendarError::BadField { row, field } => {
                write!(f, "row {row}: `{field}` missing or malformed")
            }
            CalendarError::EndBeforeStart { row } => write!(f, "row {row}: ends before it starts"),
            CalendarError::Conflict {
                zone,
                label,
                starts_on,
            } => write!(
                f,
                "zone {}: two ends for « {label} » starting {starts_on}",
                zone.as_str()
            ),
            CalendarError::MissingZone { zone, school_year } => {
                write!(f, "zone {}: no period in {school_year}", zone.as_str())
            }
        }
    }
}

impl std::error::Error for CalendarError {}

/// The school year `day` falls in, `2026-2027` from 1 September 2026 to 31
/// August 2027: the summer holidays belong to the year they close.
pub fn school_year_of(day: NaiveDate) -> String {
    let first = first_year(day);
    format!("{first}-{}", first + 1)
}

/// The calendar year a school year starts in.
fn first_year(day: NaiveDate) -> i32 {
    if day.month() >= 9 {
        day.year()
    } else {
        day.year() - 1
    }
}

/// The school years the job asks for on `today`: the current one and the
/// next.
pub fn school_years(today: NaiveDate) -> [String; 2] {
    let first = first_year(today);
    [
        format!("{first}-{}", first + 1),
        format!("{}-{}", first + 1, first + 2),
    ]
}

/// `<base>` + `RECORDS_PATH`.
pub fn records_url(base: &str) -> String {
    format!("{}{RECORDS_PATH}", base.trim_end_matches('/'))
}

/// The query string of the request: the rows of zones A, B and C for
/// `years`, one per distinct period (`group_by`), so that the académies of
/// a zone do not repeat it.
pub fn records_query(years: &[String; 2]) -> Vec<(&'static str, String)> {
    let zones = SchoolZone::ALL
        .map(|z| format!("\"Zone {}\"", z.as_str()))
        .join(",");
    vec![
        (
            "where",
            format!(
                "annee_scolaire in (\"{}\",\"{}\") and zones in ({zones})",
                years[0], years[1]
            ),
        ),
        (
            "group_by",
            "zones,annee_scolaire,description,population,start_date,end_date".into(),
        ),
        ("limit", PAGE_LIMIT.to_string()),
    ]
}

/// The `population` of the rows kept: `-` for everyone, `Élèves` for the
/// pupils' summer. `Enseignants` is left out.
const KEPT_POPULATIONS: [&str; 2] = ["-", "Élèves"];

/// The periods of `years` in an answer of the records endpoint, sorted by
/// zone, first day and label, each once.
///
/// Rows of another zone or school year are ignored, as are teachers' rows
/// and bridges. Any kept row that is not as described in the module
/// documentation fails the whole answer, as does an answer cut short or
/// one missing a zone in the current school year (`years[0]`): the job
/// then keeps the cache it has.
pub fn parse_calendar(
    body: &Value,
    years: &[String; 2],
) -> Result<Vec<SchoolHoliday>, CalendarError> {
    let rows = body
        .get("results")
        .and_then(Value::as_array)
        .ok_or(CalendarError::UnexpectedShape)?;
    if let Some(total) = body.get("total_count").and_then(Value::as_u64) {
        if total > rows.len() as u64 {
            return Err(CalendarError::Truncated {
                total,
                returned: rows.len(),
            });
        }
    }

    // Keyed by zone, first day and label: the order served, and one entry
    // per period however many académies repeat it.
    let mut periods: BTreeMap<(&'static str, NaiveDate, String), SchoolHoliday> = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        let text = |field: &'static str| {
            row.get(field)
                .and_then(Value::as_str)
                .ok_or(CalendarError::BadField { row: index, field })
        };
        let Some(zone) = text("zones")?
            .strip_prefix("Zone ")
            .and_then(SchoolZone::parse)
        else {
            continue;
        };
        let school_year = text("annee_scolaire")?;
        if !years.iter().any(|y| y == school_year) {
            continue;
        }
        if !KEPT_POPULATIONS.contains(&text("population")?) {
            continue;
        }
        let label = text("description")?.trim();
        if label.is_empty() {
            return Err(CalendarError::BadField {
                row: index,
                field: "description",
            });
        }
        if label.starts_with("Pont") {
            continue;
        }
        let day = |field: &'static str| {
            DateTime::parse_from_rfc3339(text(field)?)
                .map(|instant| paris_day(instant.to_utc()))
                .map_err(|_| CalendarError::BadField { row: index, field })
        };
        let starts_on = day("start_date")?;
        let resumes_on = day("end_date")?;
        let ends_on = match resumes_on.cmp(&starts_on) {
            Ordering::Less => return Err(CalendarError::EndBeforeStart { row: index }),
            // No end published yet: the first day only.
            Ordering::Equal => starts_on,
            Ordering::Greater => resumes_on - Duration::days(1),
        };

        let period = SchoolHoliday {
            zone,
            label: label.to_string(),
            starts_on,
            ends_on,
            school_year: school_year.to_string(),
        };
        match periods.entry((zone.as_str(), starts_on, period.label.clone())) {
            Entry::Vacant(slot) => {
                slot.insert(period);
            }
            Entry::Occupied(seen) if *seen.get() == period => {}
            Entry::Occupied(_) => {
                return Err(CalendarError::Conflict {
                    zone,
                    label: period.label,
                    starts_on,
                })
            }
        }
    }

    for zone in SchoolZone::ALL {
        if !periods
            .values()
            .any(|p| p.zone == zone && p.school_year == years[0])
        {
            return Err(CalendarError::MissingZone {
                zone,
                school_year: years[0].clone(),
            });
        }
    }
    Ok(periods.into_values().collect())
}

/// Replaces the cached periods of `years` with `periods`, in one
/// transaction: a reader sees the old periods or the new ones, never a
/// mix, and an error leaves the old ones. The years before are kept, so a
/// past month still shows its holidays.
pub async fn replace_cache(
    pool: &PgPool,
    years: &[String; 2],
    periods: &[SchoolHoliday],
) -> Result<(), sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    sqlx::query!(
        "DELETE FROM school_holidays WHERE school_year = ANY($1)",
        &years[..]
    )
    .execute(&mut *tx)
    .await?;
    let zones: Vec<String> = periods.iter().map(|p| p.zone.as_str().into()).collect();
    let labels: Vec<String> = periods.iter().map(|p| p.label.clone()).collect();
    let starts: Vec<NaiveDate> = periods.iter().map(|p| p.starts_on).collect();
    let ends: Vec<NaiveDate> = periods.iter().map(|p| p.ends_on).collect();
    let school_years: Vec<String> = periods.iter().map(|p| p.school_year.clone()).collect();
    sqlx::query!(
        r#"INSERT INTO school_holidays (zone, label, starts_on, ends_on, school_year)
           SELECT * FROM UNNEST($1::text[], $2::text[], $3::date[], $4::date[], $5::text[])"#,
        &zones,
        &labels,
        &starts,
        &ends,
        &school_years,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// One cached period, as served.
pub struct CachedPeriod {
    pub label: String,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
}

/// The cached periods of `zone` that share at least one day with
/// `[from, to]`, by first day. A period is returned whole, its first day
/// possibly before `from` and its last after `to`.
pub async fn periods_overlapping(
    conn: &mut sqlx::PgConnection,
    zone: SchoolZone,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<CachedPeriod>, sqlx::Error> {
    sqlx::query_as!(
        CachedPeriod,
        r#"SELECT label, starts_on, ends_on
           FROM school_holidays
           WHERE zone = $1 AND starts_on <= $3 AND ends_on >= $2
           ORDER BY starts_on, label"#,
        zone.as_str(),
        from,
        to,
    )
    .fetch_all(conn)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The answer to `records_query(school_years(2026-11-05))`, recorded on
    /// 2026-11-05.
    const RECORDED: &str =
        include_str!("../../tests/fixtures/school_calendar/2026-2027_2027-2028.json");

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn years() -> [String; 2] {
        ["2026-2027".to_string(), "2027-2028".to_string()]
    }

    fn recorded() -> Value {
        serde_json::from_str(RECORDED).unwrap()
    }

    fn row(zone: &str, year: &str, label: &str, population: &str, start: &str, end: &str) -> Value {
        json!({
            "zones": zone,
            "annee_scolaire": year,
            "description": label,
            "population": population,
            "start_date": start,
            "end_date": end,
        })
    }

    /// One Toussaint per zone for the current year: the least an answer
    /// must hold to be cached.
    fn minimal_rows() -> Vec<Value> {
        ["Zone A", "Zone B", "Zone C"]
            .into_iter()
            .map(|z| {
                row(
                    z,
                    "2026-2027",
                    "Vacances de la Toussaint",
                    "-",
                    "2026-10-16T22:00:00+00:00",
                    "2026-11-01T23:00:00+00:00",
                )
            })
            .collect()
    }

    fn answer(rows: Vec<Value>) -> Value {
        json!({ "total_count": rows.len(), "results": rows })
    }

    fn periods_of(all: &[SchoolHoliday], zone: SchoolZone) -> Vec<(&str, NaiveDate, NaiveDate)> {
        all.iter()
            .filter(|h| h.zone == zone)
            .map(|h| (h.label.as_str(), h.starts_on, h.ends_on))
            .collect()
    }

    // -- school years ---------------------------------------------------------

    #[test]
    fn the_school_year_turns_on_the_first_of_september() {
        assert_eq!(school_year_of(day(2026, 9, 1)), "2026-2027");
        assert_eq!(school_year_of(day(2026, 11, 5)), "2026-2027");
        assert_eq!(school_year_of(day(2027, 1, 1)), "2026-2027");
        assert_eq!(school_year_of(day(2027, 8, 31)), "2026-2027");
        assert_eq!(school_year_of(day(2027, 9, 1)), "2027-2028");
    }

    #[test]
    fn the_job_asks_for_the_current_school_year_and_the_next() {
        assert_eq!(school_years(day(2026, 11, 5)), years());
        assert_eq!(school_years(day(2027, 8, 31)), years());
        assert_eq!(
            school_years(day(2027, 9, 1)),
            ["2027-2028".to_string(), "2028-2029".to_string()]
        );
    }

    // -- request --------------------------------------------------------------

    #[test]
    fn the_request_names_the_dataset_the_zones_and_the_years_only() {
        assert_eq!(
            records_url("https://data.education.gouv.fr/"),
            "https://data.education.gouv.fr/api/explore/v2.1/catalog/datasets/fr-en-calendrier-scolaire/records"
        );
        let query = records_query(&years());
        let get = |k: &str| {
            query
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.as_str())
                .unwrap_or_else(|| panic!("no `{k}`"))
        };
        assert_eq!(
            get("where"),
            r#"annee_scolaire in ("2026-2027","2027-2028") and zones in ("Zone A","Zone B","Zone C")"#
        );
        assert_eq!(
            get("group_by"),
            "zones,annee_scolaire,description,population,start_date,end_date"
        );
        assert_eq!(get("limit"), PAGE_LIMIT.to_string());
    }

    // -- the recorded answer --------------------------------------------------

    /// Christmas is the same for every zone; winter is not. Each period is
    /// kept once per zone, with its last day the day before classes resume.
    #[test]
    fn christmas_comes_once_per_zone_and_winter_differs() {
        let all = parse_calendar(&recorded(), &years()).unwrap();
        for zone in SchoolZone::ALL {
            let christmas: Vec<_> = all
                .iter()
                .filter(|h| h.zone == zone && h.label == "Vacances de Noël")
                .map(|h| (h.starts_on, h.ends_on, h.school_year.as_str()))
                .collect();
            assert_eq!(
                christmas,
                [
                    (day(2026, 12, 19), day(2027, 1, 3), "2026-2027"),
                    (day(2027, 12, 18), day(2028, 1, 2), "2027-2028"),
                ],
                "{zone:?}"
            );
        }
        let winter_2027 = |zone| {
            all.iter()
                .find(|h| {
                    h.zone == zone && h.label == "Vacances d'Hiver" && h.school_year == "2026-2027"
                })
                .map(|h| (h.starts_on, h.ends_on))
                .unwrap()
        };
        assert_eq!(
            winter_2027(SchoolZone::A),
            (day(2027, 2, 13), day(2027, 2, 28))
        );
        assert_eq!(
            winter_2027(SchoolZone::B),
            (day(2027, 2, 20), day(2027, 3, 7))
        );
        assert_eq!(
            winter_2027(SchoolZone::C),
            (day(2027, 2, 6), day(2027, 2, 21))
        );
    }

    /// Zone B's periods in full: summer for pupils only, no bridge, and the
    /// summer of 2028 reduced to its first day, its end not being out yet.
    /// Toussaint 2026 starts in summer time and ends in winter time: both
    /// offsets land on the Paris day.
    #[test]
    fn zone_b_in_full() {
        let all = parse_calendar(&recorded(), &years()).unwrap();
        assert_eq!(
            periods_of(&all, SchoolZone::B),
            [
                (
                    "Vacances de la Toussaint",
                    day(2026, 10, 17),
                    day(2026, 11, 1)
                ),
                ("Vacances de Noël", day(2026, 12, 19), day(2027, 1, 3)),
                ("Vacances d'Hiver", day(2027, 2, 20), day(2027, 3, 7)),
                ("Vacances de Printemps", day(2027, 4, 17), day(2027, 5, 2)),
                ("Vacances d'Été", day(2027, 7, 3), day(2027, 9, 1)),
                (
                    "Vacances de la Toussaint",
                    day(2027, 10, 23),
                    day(2027, 11, 7)
                ),
                ("Vacances de Noël", day(2027, 12, 18), day(2028, 1, 2)),
                ("Vacances d'Hiver", day(2028, 2, 5), day(2028, 2, 20)),
                ("Vacances de Printemps", day(2028, 4, 8), day(2028, 4, 23)),
                ("Début des Vacances d'Été", day(2028, 7, 4), day(2028, 7, 4)),
            ]
        );
        assert_eq!(all.len(), 30);
        assert!(!all.iter().any(|h| h.label.contains("Pont")));
    }

    #[test]
    fn the_answer_is_sorted_by_zone_then_first_day() {
        let all = parse_calendar(&recorded(), &years()).unwrap();
        let keys: Vec<_> = all.iter().map(|h| (h.zone.as_str(), h.starts_on)).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    // -- rows kept and rows left out -------------------------------------------

    /// Ungrouped, the dataset repeats a period once per académie: kept once.
    #[test]
    fn a_period_repeated_by_several_academies_is_kept_once() {
        let mut rows = minimal_rows();
        rows.extend(minimal_rows());
        let all = parse_calendar(&answer(rows), &years()).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn other_zones_other_years_teachers_and_bridges_are_left_out() {
        let mut rows = minimal_rows();
        rows.push(row(
            "Corse",
            "2026-2027",
            "Vacances de Noël",
            "-",
            "2026-12-18T23:00:00+00:00",
            "2027-01-03T23:00:00+00:00",
        ));
        rows.push(row(
            "Zone A",
            "2025-2026",
            "Vacances de Noël",
            "-",
            "2025-12-19T23:00:00+00:00",
            "2026-01-04T23:00:00+00:00",
        ));
        rows.push(row(
            "Zone A",
            "2026-2027",
            "Vacances d'Été",
            "Enseignants",
            "2027-07-02T22:00:00+00:00",
            "2027-08-31T22:00:00+00:00",
        ));
        rows.push(row(
            "Zone A",
            "2026-2027",
            "Pont de l'Ascension",
            "-",
            "2027-05-06T22:00:00+00:00",
            "2027-05-06T22:00:00+00:00",
        ));
        let all = parse_calendar(&answer(rows), &years()).unwrap();
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|h| h.label == "Vacances de la Toussaint"));
    }

    #[test]
    fn the_label_is_trimmed() {
        let mut rows = minimal_rows();
        rows.push(row(
            "Zone A",
            "2026-2027",
            "  Vacances de Noël ",
            "-",
            "2026-12-18T23:00:00+00:00",
            "2027-01-03T23:00:00+00:00",
        ));
        let all = parse_calendar(&answer(rows), &years()).unwrap();
        assert!(all.iter().any(|h| h.label == "Vacances de Noël"));
    }

    /// The next year may not be published yet: only the current one is
    /// required.
    #[test]
    fn the_next_school_year_may_be_absent() {
        let all = parse_calendar(&answer(minimal_rows()), &years()).unwrap();
        assert!(all.iter().all(|h| h.school_year == "2026-2027"));
    }

    // -- answers refused --------------------------------------------------------

    #[test]
    fn an_answer_of_another_shape_is_refused() {
        for body in [
            json!(null),
            json!([]),
            json!({}),
            json!({"results": {}}),
            json!({"error_code": "ODSQLError", "message": "Unknown field"}),
        ] {
            assert_eq!(
                parse_calendar(&body, &years()),
                Err(CalendarError::UnexpectedShape),
                "{body}"
            );
        }
    }

    #[test]
    fn an_answer_cut_short_is_refused() {
        let mut body = answer(minimal_rows());
        body["total_count"] = json!(150);
        assert_eq!(
            parse_calendar(&body, &years()),
            Err(CalendarError::Truncated {
                total: 150,
                returned: 3
            })
        );
    }

    #[test]
    fn a_kept_row_with_a_missing_or_malformed_field_is_refused() {
        for (field, value) in [
            ("start_date", Value::Null),
            ("end_date", Value::Null),
            ("start_date", json!("2026-10-17")),
            ("end_date", json!("demain")),
            ("description", json!("   ")),
            ("description", json!(12)),
            ("population", Value::Null),
            ("annee_scolaire", Value::Null),
            ("zones", Value::Null),
        ] {
            let mut rows = minimal_rows();
            rows[1][field] = value.clone();
            assert_eq!(
                parse_calendar(&answer(rows), &years()),
                Err(CalendarError::BadField { row: 1, field }),
                "{field} = {value}"
            );
        }
    }

    #[test]
    fn a_kept_row_ending_before_it_starts_is_refused() {
        let mut rows = minimal_rows();
        rows[2]["end_date"] = json!("2026-10-15T22:00:00+00:00");
        assert_eq!(
            parse_calendar(&answer(rows), &years()),
            Err(CalendarError::EndBeforeStart { row: 2 })
        );
    }

    #[test]
    fn two_ends_for_one_period_are_refused() {
        let mut rows = minimal_rows();
        let mut other = rows[0].clone();
        other["end_date"] = json!("2026-11-08T23:00:00+00:00");
        rows.push(other);
        assert_eq!(
            parse_calendar(&answer(rows), &years()),
            Err(CalendarError::Conflict {
                zone: SchoolZone::A,
                label: "Vacances de la Toussaint".into(),
                starts_on: day(2026, 10, 17),
            })
        );
    }

    /// An empty answer, or one without a zone, would empty that zone's
    /// cache: refused.
    #[test]
    fn a_zone_without_any_period_this_year_is_refused() {
        assert_eq!(
            parse_calendar(&answer(vec![]), &years()),
            Err(CalendarError::MissingZone {
                zone: SchoolZone::A,
                school_year: "2026-2027".into(),
            })
        );
        let mut rows = minimal_rows();
        rows.remove(2);
        rows.push(row(
            "Zone C",
            "2027-2028",
            "Vacances de la Toussaint",
            "-",
            "2027-10-22T22:00:00+00:00",
            "2027-11-07T23:00:00+00:00",
        ));
        assert_eq!(
            parse_calendar(&answer(rows), &years()),
            Err(CalendarError::MissingZone {
                zone: SchoolZone::C,
                school_year: "2026-2027".into(),
            })
        );
    }
}
