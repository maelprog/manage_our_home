use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agenda::recurrence;
use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::AppState;

/// How far ahead recurring-event reminders are materialized into
/// `scheduled_notifications`. The worker (jobs/scheduled_notifications.rs)
/// re-runs this refill periodically so an open-ended RRULE never needs to
/// be scheduled indefinitely up front.
pub const NOTIFICATION_WINDOW_DAYS: i64 = 30;

#[derive(Deserialize)]
pub struct CreateReminderRequest {
    pub offset_minutes: i32,
}

#[derive(Serialize)]
pub struct ReminderResponse {
    pub id: Uuid,
    pub event_id: Uuid,
    pub offset_minutes: i32,
}

pub async fn create_reminder(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<CreateReminderRequest>,
) -> AppResult<impl IntoResponse> {
    if body.offset_minutes < 0 {
        return Err(AppError::BadRequest(
            "offset_minutes_must_be_non_negative".into(),
        ));
    }

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let event = sqlx::query!(
        "SELECT starts_at, ends_at, all_day, rrule FROM events WHERE id = $1 AND group_id = $2",
        event_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    let reminder = sqlx::query!(
        r#"
        INSERT INTO event_reminders (event_id, offset_minutes)
        VALUES ($1, $2)
        RETURNING id
        "#,
        event_id,
        body.offset_minutes,
    )
    .fetch_one(&mut *tx)
    .await?;

    refill_notifications(
        &mut tx,
        reminder.id,
        event_id,
        EventTimes {
            starts_at: event.starts_at,
            ends_at: event.ends_at,
            all_day: event.all_day,
            rrule: event.rrule.as_deref(),
        },
        body.offset_minutes,
    )
    .await
    .map_err(|_| AppError::Internal(anyhow::anyhow!("failed to schedule notifications")))?;

    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(ReminderResponse {
            id: reminder.id,
            event_id,
            offset_minutes: body.offset_minutes,
        }),
    ))
}

/// The columns of an event its reminders are scheduled from — what
/// `list_events` unrolls it from too (#169).
pub struct EventTimes<'a> {
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub all_day: bool,
    pub rrule: Option<&'a str>,
}

/// Materializes `scheduled_notifications` rows for every occurrence of
/// `event_id` falling within the next `NOTIFICATION_WINDOW_DAYS`, for the
/// given reminder. Idempotent via the `(event_reminder_id, occurrence_at)`
/// unique constraint — safe to call again on refill.
pub async fn refill_notifications(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    reminder_id: Uuid,
    event_id: Uuid,
    event: EventTimes<'_>,
    offset_minutes: i32,
) -> anyhow::Result<()> {
    let now = Utc::now();
    let window_end = now + Duration::days(NOTIFICATION_WINDOW_DAYS);

    let occurrences = reminder_occurrences(
        event.rrule,
        event.all_day,
        event.starts_at,
        event.ends_at,
        now,
        window_end,
    )?;

    for occurrence_at in occurrences {
        let fire_at = occurrence_at - Duration::minutes(offset_minutes as i64);
        sqlx::query!(
            r#"
            INSERT INTO scheduled_notifications (event_reminder_id, event_id, occurrence_at, fire_at)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (event_reminder_id, occurrence_at) DO NOTHING
            "#,
            reminder_id,
            event_id,
            occurrence_at,
            fire_at,
        )
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}

pub async fn delete_reminder(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id, reminder_id)): Path<(Uuid, Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let result = sqlx::query!(
        "DELETE FROM event_reminders WHERE id = $1 AND event_id = $2",
        reminder_id,
        event_id,
    )
    .execute(&mut *tx)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The occurrence starts a reminder is scheduled on inside `[from, to]`:
/// the row itself for a one-off, its unrolled occurrences for a series.
///
/// A series is unrolled by `recurrence::expand_series`, the unroll
/// `list_events` renders, so a reminder is scheduled on the occurrences the
/// agenda lists and on no other (#169). Unrolled on instants whatever the
/// row, an all-day series with `UNTIL` at the end of its last UTC day got
/// one reminder past it.
fn reminder_occurrences(
    rrule: Option<&str>,
    all_day: bool,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<DateTime<Utc>>, rrule::RRuleError> {
    match rrule {
        Some(r) => Ok(
            recurrence::expand_series(r, all_day, starts_at, ends_at, from, to)?
                .into_iter()
                .map(|(start, _)| start)
                .collect(),
        ),
        None => Ok(vec![starts_at]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use manage_our_home_shared::validation::agenda::{paris_date, paris_start_of_day};

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// Paris midnight opening `y-m-d` — what the row stores for an all-day
    /// event on that date.
    fn midnight(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        paris_start_of_day(day(y, m, d))
    }

    /// What the agenda lists for the same row: `list_events`' unroll.
    fn listed(
        rrule: &str,
        all_day: bool,
        starts_at: DateTime<Utc>,
        ends_at: DateTime<Utc>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Vec<DateTime<Utc>> {
        recurrence::expand_series(rrule, all_day, starts_at, ends_at, from, to)
            .unwrap()
            .into_iter()
            .map(|(start, _)| start)
            .collect()
    }

    /// An all-day daily series from `first` with what `build_rrule` writes
    /// for « Jusqu'au `last` » — `UNTIL` at 23:59:59Z on that date — has to
    /// be reminded on exactly the Paris days `first..=last`, and on the
    /// instants the agenda lists.
    fn assert_until_end_of_utc_day(first: NaiveDate, last: NaiveDate) {
        let rule = format!("FREQ=DAILY;UNTIL={}T235959Z", last.format("%Y%m%d"));
        let starts_at = paris_start_of_day(first);
        let ends_at = paris_start_of_day(first.succ_opt().unwrap());
        let (from, to) = (
            starts_at - Duration::days(3),
            starts_at + Duration::days(60),
        );

        let reminded =
            reminder_occurrences(Some(&rule), true, starts_at, ends_at, from, to).unwrap();
        let expected: Vec<DateTime<Utc>> = first
            .iter_days()
            .take_while(|d| *d <= last)
            .map(paris_start_of_day)
            .collect();
        assert_eq!(
            reminded.iter().map(|s| paris_date(*s)).collect::<Vec<_>>(),
            expected.iter().map(|s| paris_date(*s)).collect::<Vec<_>>(),
            "{rule} from {first}"
        );
        assert_eq!(reminded, expected, "{rule} from {first}");
        assert_eq!(
            reminded,
            listed(&rule, true, starts_at, ends_at, from, to),
            "{rule} from {first}: the reminders and the agenda part"
        );
    }

    // -- an all-day series is reminded on the days the agenda lists (#169) ---
    //
    // Paris midnight on the day after the `UNTIL` date is 22:00Z (summer) or
    // 23:00Z (winter) on that date — before 23:59:59Z. Unrolled on instants
    // from the stored Paris midnight, the series got one day too many.

    #[test]
    fn an_all_day_series_until_the_end_of_a_utc_day_is_not_reminded_past_it_in_summer() {
        // The reproduction from #169: « Jusqu'au 10/10 » from 1 October,
        // Paris on UTC+2 throughout.
        assert_until_end_of_utc_day(day(2026, 10, 1), day(2026, 10, 10));
    }

    #[test]
    fn an_all_day_series_until_the_end_of_a_utc_day_is_not_reminded_past_it_in_winter() {
        // Paris on UTC+1 throughout.
        assert_until_end_of_utc_day(day(2026, 12, 1), day(2026, 12, 10));
    }

    #[test]
    fn an_all_day_series_until_the_end_of_a_utc_day_is_not_reminded_past_it_across_a_change() {
        // Starts in summer time, ends in winter time (2026-10-25)…
        assert_until_end_of_utc_day(day(2026, 10, 20), day(2026, 10, 30));
        // …and the other way round (2026-03-29).
        assert_until_end_of_utc_day(day(2026, 3, 25), day(2026, 4, 2));
    }

    #[test]
    fn the_reminders_read_every_series_the_way_the_agenda_lists_it() {
        let rules = [
            "FREQ=DAILY",
            "FREQ=DAILY;COUNT=5",
            "FREQ=WEEKLY;BYDAY=SA",
            "FREQ=MONTHLY",
            "FREQ=DAILY;UNTIL=20261010T235959Z",
            "FREQ=DAILY;UNTIL=20261010T215959Z",
            "FREQ=DAILY;UNTIL=20261010T220000Z",
            "FREQ=DAILY;UNTIL=20261210T225959Z",
            "FREQ=DAILY;UNTIL=20261210T230000Z",
            "FREQ=DAILY;UNTIL=20261210T235959Z",
        ];
        let window = (midnight(2026, 9, 1), midnight(2027, 1, 31));
        for rule in rules {
            for first in [midnight(2026, 10, 1), midnight(2026, 12, 1)] {
                for all_day in [true, false] {
                    let ends_at = first + Duration::days(1);
                    let Ok(listed) = recurrence::expand_series(
                        rule, all_day, first, ends_at, window.0, window.1,
                    ) else {
                        // An UNTIL before this anchor: refused on write.
                        continue;
                    };
                    let listed: Vec<DateTime<Utc>> = listed.into_iter().map(|(s, _)| s).collect();
                    assert_eq!(
                        reminder_occurrences(
                            Some(rule),
                            all_day,
                            first,
                            ends_at,
                            window.0,
                            window.1
                        )
                        .unwrap(),
                        listed,
                        "{rule} from {first}, all_day = {all_day}"
                    );
                }
            }
        }
    }

    /// What the reminders keep of `reminder_occurrences`: one
    /// `scheduled_notifications` row per instant, `ON CONFLICT
    /// (event_reminder_id, occurrence_at) DO NOTHING` dropping the copies.
    fn stored(occurrences: Vec<DateTime<Utc>>) -> Vec<DateTime<Utc>> {
        occurrences
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    // -- an all-day series the API takes is read alike by both readers (#171)
    //
    // Both readers unroll through `expand_series`, but they do not keep the
    // same thing of it: the agenda lists every occurrence, the reminders
    // store one row per instant. They part as soon as an all-day unroll
    // gives the same day twice, which it does for any rule stepping by less
    // than a day or naming a time of day — `FREQ=HOURLY;COUNT=5` is five
    // entries on the agenda and one reminder. Compared here on what each
    // reader ends up with, for every rule `validate` lets through, rather
    // than on which rules it refuses.

    #[test]
    fn every_all_day_series_accepted_on_write_is_listed_as_often_as_it_is_reminded() {
        let frequencies = [
            "YEARLY", "MONTHLY", "WEEKLY", "DAILY", "HOURLY", "MINUTELY", "SECONDLY",
        ];
        let parts = [
            "",
            ";BYHOUR=12",
            ";BYHOUR=9,17",
            ";BYMINUTE=0,30",
            ";BYSECOND=0,15",
            ";BYDAY=SA",
            ";BYMONTHDAY=5,6",
        ];
        let ends = ["", ";COUNT=5", ";UNTIL=20261020T235959Z"];

        let mut accepted = 0;
        let mut parted = Vec::new();
        for frequency in frequencies {
            for part in parts {
                for end in ends {
                    let rule = format!("FREQ={frequency}{part}{end}");
                    for first in [midnight(2026, 9, 5), midnight(2026, 10, 20)] {
                        let ends_at = first + Duration::days(1);
                        let (from, to) = (first - Duration::days(1), first + Duration::days(30));
                        let Ok(listed) =
                            recurrence::expand_series(&rule, true, first, ends_at, from, to)
                        else {
                            continue;
                        };
                        let listed: Vec<DateTime<Utc>> =
                            listed.into_iter().map(|(s, _)| s).collect();
                        let reminded = stored(
                            reminder_occurrences(Some(&rule), true, first, ends_at, from, to)
                                .unwrap(),
                        );
                        if recurrence::validate(&rule, first, true).is_ok() {
                            accepted += 1;
                            assert_eq!(
                                listed,
                                reminded,
                                "{rule} from {first}: listed {} times, reminded {} times",
                                listed.len(),
                                reminded.len()
                            );
                        } else if listed != reminded {
                            parted.push((rule.clone(), first, listed.len(), reminded.len()));
                        }
                    }
                }
            }
        }
        // Neither half of the comparison may be empty: the corpus holds rules
        // the API takes, and rules on which the two readers would part — five
        // entries against one reminder, and an unbounded `FREQ=MINUTELY`
        // spending `MAX_OCCURRENCES` on one day.
        assert!(accepted > 0);
        let first = midnight(2026, 9, 5);
        let premises = [("FREQ=HOURLY;COUNT=5", 5, 1), ("FREQ=MINUTELY", 1000, 1)];
        for (rule, listed, reminded) in premises {
            let premise = (rule.to_string(), first, listed, reminded);
            assert!(
                parted.contains(&premise),
                "the premise {premise:?}, parted = {parted:?}"
            );
        }
    }

    #[test]
    fn a_one_off_is_reminded_on_its_own_start() {
        let at = midnight(2026, 10, 1);
        assert_eq!(
            reminder_occurrences(None, true, at, at + Duration::days(1), at, at).unwrap(),
            vec![at]
        );
    }
}
