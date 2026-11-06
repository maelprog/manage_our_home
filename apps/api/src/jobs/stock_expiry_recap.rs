//! The daily recap of the stock items nearing their expiry date (#566),
//! at the hour each member chose (#700).
//!
//! Every member has a recap hour, Europe/Paris (`users.expiry_digest_hour`,
//! 9 by default, migration 0049). From that hour on, once a day
//! ([`member_due`], `users.expiry_digest_sent_on`), the member gets, for
//! each of their families with items of quantity > 0 whose « à consommer
//! jusqu'au » date is at most `EXPIRY_SOON_DAYS` days ahead — expired ones
//! included, the window /stocks counts in its « bientôt » badge
//! (`expiring_until`) — and not yet announced to them for that date, one
//! notification: « 3 produits périment bientôt : Lait, Œufs… »
//! (`center::Event::StockExpiryRecap`). What it announced is marked per
//! member (`stock_expiry_announcements`, migration 0049) in the transaction
//! that writes the notifications: two members at two hours each get the
//! rows due at theirs, and a date moved is announced again.
//!
//! Once committed, the members whose channel includes notifications
//! (`users.reminder_channel`, as for the budget alerts) get a push on each
//! of their devices ([`push_recap`]): no payload, the service worker shows
//! the fixed text of `stock_expiry`. A member without a device reads the
//! recap in the notification centre only.
//!
//! It completes `jobs::stock_expiry`, which stays (user's decision of
//! 2026-11-05): that hourly pass writes one notification per item entering
//! the window, in the centre only, and skips the members a recap already
//! announced the item to for its date.

use std::collections::BTreeMap;
use std::time::Duration as StdDuration;

use anyhow::Context;
use chrono::{DateTime, NaiveDate, Timelike, Utc};
use manage_our_home_shared::validation::stocks::expiring_until;
use sqlx::PgPool;
use tokio::time::{interval, MissedTickBehavior};
use uuid::Uuid;

use crate::attachment_reconcile::ensure_bypasses_rls;
use crate::jobs::scheduled_notifications::push_devices;
use crate::jobs::single_runner::{exclusively, Job};
use crate::notifications::center::{self, Event, NewNotification};
use crate::notifications::{push, ReminderChannel};

/// How often the worker looks for the members whose recap is due: a recap
/// goes at most this late after its member's hour.
pub const RECAP_POLL_INTERVAL: StdDuration = StdDuration::from_secs(5 * 60);

/// How long a push service keeps the recap for a device that is offline:
/// half a day. Read later, it is in the notification centre anyway.
pub const RECAP_PUSH_TTL_SECS: i64 = 12 * 3600;

/// The Paris day and hour (0..23) at `now`.
pub fn paris_clock(now: DateTime<Utc>) -> (NaiveDate, u32) {
    let local = now.with_timezone(&rrule::Tz::Europe__Paris);
    (local.date_naive(), local.hour())
}

/// A member's recap is due at the Paris `today` and `hour`: their hour
/// (`digest_hour`) reached, and no recap pass for them yet today
/// (`sent_on`, the day of their last one). Reached, not equal: a process
/// down at that hour, or the hour skipped by the change to summer time,
/// sends it at the next pass, still once that day. A member who moves
/// their hour later after that day's recap gets none more that day.
pub fn member_due(
    today: NaiveDate,
    hour: u32,
    digest_hour: i16,
    sent_on: Option<NaiveDate>,
) -> bool {
    u32::try_from(digest_hour).is_ok_and(|digest_hour| digest_hour <= 23 && digest_hour <= hour)
        && sent_on != Some(today)
}

/// A stock row is in a member's recap of `today`: some left, a date at
/// most `EXPIRY_SOON_DAYS` days ahead, expired included, and that date not
/// the one already announced to them (`announced`).
pub fn in_recap(
    quantity: f64,
    expires_on: Option<NaiveDate>,
    announced: Option<NaiveDate>,
    today: NaiveDate,
) -> bool {
    quantity > 0.0
        && expires_on.is_some_and(|date| date <= expiring_until(today))
        && announced != expires_on
}

/// Polling worker. `pool` must be the `BYPASSRLS` admin pool
/// (`AppState.admin_db`): it reads `stock_items`, `group_members` and
/// `users` and writes `notifications` across every family. Without a VAPID
/// key the recap goes to the notification centre only.
pub async fn run(pool: PgPool, vapid: Option<std::sync::Arc<push::Vapid>>) {
    let mut ticker = interval(RECAP_POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let client = push::client();
    loop {
        ticker.tick().await;
        let (today, hour) = paris_clock(Utc::now());
        let recipients =
            match exclusively(&pool, Job::StockExpiryRecap, recap(&pool, today, hour)).await {
                Ok(Some(recap)) => {
                    tracing::debug!(written = recap.written, "stock expiry recap done");
                    recap.recipients
                }
                Ok(None) => {
                    tracing::debug!("stock expiry recap skipped: another process runs it");
                    continue;
                }
                Err(e) => {
                    tracing::error!(error = ?e, "stock expiry recap failed");
                    continue;
                }
            };
        if recipients.is_empty() {
            continue;
        }
        if let Some(vapid) = &vapid {
            let send_push = |endpoint: String, ttl_secs: i64| {
                let (client, vapid) = (client.clone(), vapid.clone());
                async move { push::send(&client, &vapid, &endpoint, ttl_secs).await }
            };
            if let Err(e) = push_recap(&pool, &send_push, &recipients).await {
                tracing::error!(error = ?e, "failed to push the stock expiry recap");
            }
        }
    }
}

/// What a recap pass did.
#[derive(Debug, Default)]
pub struct Recap {
    /// Notifications written.
    pub written: u64,
    /// The members notified, each once, for [`push_recap`].
    pub recipients: Vec<Uuid>,
}

/// One pass at the Paris `today` and `hour`: parameters so the flow tests
/// can move the clock. Every member whose recap is due ([`member_due`])
/// has their pass marked, recap or not: rows due later that day wait for
/// the next one. [`Recap::recipients`] are the members who got one.
pub async fn recap(pool: &PgPool, today: NaiveDate, hour: u32) -> anyhow::Result<Recap> {
    let mut tx = crate::db::begin(pool).await?;
    ensure_bypasses_rls(&mut tx).await.context(
        "stock expiry recap refusing to run: `stock_items` is FORCE ROW LEVEL SECURITY, so on a \
         role that does not bypass it no item would ever be found expiring. Point \
         ADMIN_DATABASE_URL at the BYPASSRLS admin_role (see apps/api/README.md).",
    )?;
    // The candidates; `member_due` decides. Locked, so that a second pass
    // waits for this one and then finds them done.
    let candidates = sqlx::query!(
        r#"SELECT id, expiry_digest_hour, expiry_digest_sent_on FROM users
           WHERE expiry_digest_hour <= $1
             AND expiry_digest_sent_on IS DISTINCT FROM $2
             AND deactivated_at IS NULL AND deleted_at IS NULL
           ORDER BY id
           FOR UPDATE"#,
        i16::try_from(hour).unwrap_or(i16::MAX),
        today,
    )
    .fetch_all(&mut *tx)
    .await?;
    let due: Vec<Uuid> = candidates
        .into_iter()
        .filter(|m| member_due(today, hour, m.expiry_digest_hour, m.expiry_digest_sent_on))
        .map(|m| m.id)
        .collect();
    let mut done = Recap::default();
    if due.is_empty() {
        tx.commit().await?;
        return Ok(done);
    }

    // The rows of their families; `in_recap` decides. The date read is the
    // one marked announced: a date moved meanwhile is due again.
    let rows = sqlx::query!(
        r#"SELECT gm.user_id, s.id, s.group_id, s.name, s.quantity,
                  s.expires_on AS "expires_on!", a.expires_on AS "announced?"
           FROM group_members gm
           JOIN stock_items s ON s.group_id = gm.group_id
           LEFT JOIN stock_expiry_announcements a
                  ON a.user_id = gm.user_id AND a.stock_item_id = s.id
           WHERE gm.user_id = ANY($1)
             AND s.expires_on <= $2 AND s.quantity > 0
             AND a.expires_on IS DISTINCT FROM s.expires_on
           ORDER BY gm.user_id, s.group_id, s.expires_on, s.name, s.id"#,
        &due,
        expiring_until(today),
    )
    .fetch_all(&mut *tx)
    .await?;

    // Per member and family: the rows' ids, dates and names.
    type Rows = (Vec<Uuid>, Vec<NaiveDate>, Vec<String>);
    let mut recaps: BTreeMap<(Uuid, Uuid), Rows> = BTreeMap::new();
    for row in rows {
        if in_recap(row.quantity, Some(row.expires_on), row.announced, today) {
            let (ids, dates, names) = recaps.entry((row.user_id, row.group_id)).or_default();
            ids.push(row.id);
            dates.push(row.expires_on);
            names.push(row.name);
        }
    }

    for ((user_id, group_id), (ids, dates, names)) in recaps {
        done.written += center::insert_for(
            &mut tx,
            &[user_id],
            &NewNotification {
                group_id,
                event: Event::StockExpiryRecap { items: &names },
                object_id: None,
                actor_id: None,
                dedup_key: None,
            },
        )
        .await?;
        sqlx::query!(
            r#"INSERT INTO stock_expiry_announcements (user_id, stock_item_id, expires_on)
               SELECT $1, item, date FROM unnest($2::uuid[], $3::date[]) AS t(item, date)
               ON CONFLICT (user_id, stock_item_id)
               DO UPDATE SET expires_on = EXCLUDED.expires_on"#,
            user_id,
            &ids,
            &dates,
        )
        .execute(&mut *tx)
        .await?;
        done.recipients.push(user_id);
    }
    sqlx::query!(
        "UPDATE users SET expiry_digest_sent_on = $2 WHERE id = ANY($1)",
        &due,
        today,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    done.recipients.dedup();
    Ok(done)
}

/// Pushes the devices of the `recipients` whose channel includes
/// notifications (`ReminderChannel::includes_push`), recording each
/// device's answer as the reminders do (`push_devices`). `send_push` is the
/// push itself, a parameter so that a test can stand in for the push
/// services.
pub async fn push_recap<P, PFut>(
    pool: &PgPool,
    send_push: &P,
    recipients: &[Uuid],
) -> anyhow::Result<()>
where
    P: Fn(String, i64) -> PFut,
    PFut: std::future::Future<Output = push::PushOutcome>,
{
    let members = sqlx::query!(
        r#"SELECT id, reminder_channel FROM users
           WHERE id = ANY($1) AND deactivated_at IS NULL AND deleted_at IS NULL"#,
        recipients,
    )
    .fetch_all(pool)
    .await?;
    for member in members {
        let pushes = ReminderChannel::parse(&member.reminder_channel)
            .is_some_and(ReminderChannel::includes_push);
        if pushes {
            push_devices(
                pool,
                send_push,
                member.id,
                push::PushKind::StockExpiry,
                RECAP_PUSH_TTL_SECS,
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    // -- the hour ------------------------------------------------------------

    /// 9:00 in Paris is 8:00 UTC in winter (5 November 2026)…
    #[test]
    fn the_paris_clock_in_winter() {
        let at = |h, m| Utc.with_ymd_and_hms(2026, 11, 5, h, m, 0).unwrap();
        assert_eq!(paris_clock(at(7, 59)), (day(2026, 11, 5), 8));
        assert_eq!(paris_clock(at(8, 0)), (day(2026, 11, 5), 9));
        // 23:00 UTC is already midnight of the 6th in Paris.
        assert_eq!(paris_clock(at(23, 0)), (day(2026, 11, 6), 0));
    }

    /// …and 7:00 UTC in summer.
    #[test]
    fn the_paris_clock_in_summer() {
        let at = |h, m| Utc.with_ymd_and_hms(2026, 7, 1, h, m, 0).unwrap();
        assert_eq!(paris_clock(at(6, 59)), (day(2026, 7, 1), 8));
        assert_eq!(paris_clock(at(7, 0)), (day(2026, 7, 1), 9));
        assert_eq!(paris_clock(at(21, 59)), (day(2026, 7, 1), 23));
        assert_eq!(paris_clock(at(22, 0)), (day(2026, 7, 2), 0));
    }

    /// Each member at their own hour: not before it, from it on.
    #[test]
    fn a_member_is_due_from_their_own_hour() {
        let today = day(2026, 11, 5);
        for digest_hour in [0, 7, 9, 20, 23] {
            for hour in 0..24 {
                assert_eq!(
                    member_due(today, hour, digest_hour, None),
                    hour >= digest_hour as u32,
                    "{digest_hour} h at {hour} h"
                );
            }
        }
    }

    /// Two members of one family, at 7 and at 20: at 7 the first only; at
    /// 20 the second only, the first having had theirs.
    #[test]
    fn two_members_at_two_hours_are_due_at_theirs() {
        let today = day(2026, 11, 5);
        assert!(member_due(today, 7, 7, None));
        assert!(!member_due(today, 7, 20, None));
        assert!(!member_due(today, 20, 7, Some(today)));
        assert!(member_due(today, 20, 20, None));
    }

    /// Once a day: not again the day it ran, again the next day from the
    /// hour; a recap missed for days is today's, once.
    #[test]
    fn a_member_is_due_once_a_day() {
        let today = day(2026, 11, 5);
        let yesterday = day(2026, 11, 4);
        assert!(!member_due(today, 9, 9, Some(today)));
        assert!(!member_due(today, 23, 9, Some(today)));
        assert!(!member_due(today, 8, 9, Some(yesterday)));
        assert!(member_due(today, 9, 9, Some(yesterday)));
        assert!(member_due(today, 12, 9, Some(day(2026, 10, 1))));
    }

    /// A pass missed at the hour — a process down, or 2:00 skipped by the
    /// change to summer time — sends at the next one, the same day.
    #[test]
    fn a_missed_hour_is_caught_up_the_same_day() {
        let spring = day(2027, 3, 28);
        // 1:00 UTC on 28 March 2027 is 3:00 in Paris: 2:00 never happens.
        let at = Utc.with_ymd_and_hms(2027, 3, 28, 1, 0, 0).unwrap();
        assert_eq!(paris_clock(at), (spring, 3));
        assert!(member_due(spring, 3, 2, Some(day(2027, 3, 27))));
    }

    /// An hour outside the day, which the `CHECK` refuses anyway, is never
    /// due.
    #[test]
    fn an_hour_outside_the_day_is_never_due() {
        let today = day(2026, 11, 5);
        assert!(!member_due(today, 23, 24, None));
        assert!(!member_due(today, 23, -1, None));
    }

    // -- the rows ------------------------------------------------------------

    /// The window: expired rows included, up to `EXPIRY_SOON_DAYS` days
    /// ahead included, nothing further.
    #[test]
    fn the_recap_takes_the_soon_window_expired_included() {
        let today = day(2026, 11, 5);
        let on = |n: i64| Some(today + Duration::days(n));
        for n in [-400, -1, 0, 1, 3] {
            assert!(in_recap(1.0, on(n), None, today), "J{n:+}");
        }
        for n in [4, 30] {
            assert!(!in_recap(1.0, on(n), None, today), "J{n:+}");
        }
        assert!(!in_recap(1.0, None, None, today), "no date");
    }

    /// Nothing left, nothing to eat: never in a recap.
    #[test]
    fn a_row_without_quantity_is_never_in_the_recap() {
        let today = day(2026, 11, 5);
        assert!(!in_recap(0.0, Some(today), None, today));
        assert!(in_recap(0.25, Some(today), None, today));
    }

    /// Once per date: a row announced for its date is not again; its date
    /// moved, it is, the old one being the one announced.
    #[test]
    fn a_row_is_announced_once_per_date() {
        let today = day(2026, 11, 5);
        let soon = Some(today + Duration::days(2));
        assert!(!in_recap(1.0, soon, soon, today));
        assert!(in_recap(1.0, soon, Some(today + Duration::days(1)), today));
        assert!(in_recap(1.0, Some(today), soon, today));
    }
}
