//! The daily recap of the stock items nearing their expiry date (#566).
//!
//! Once a day, at [`RECAP_HOUR`] Europe/Paris, every family with items of
//! quantity > 0 whose « à consommer jusqu'au » date is at most
//! `EXPIRY_SOON_DAYS` days ahead — expired ones included, the window
//! /stocks counts in its « bientôt » badge (`expiring_until`) — and not yet
//! in a recap for that date gets one notification per member: « 3 produits
//! périment bientôt : Lait, Œufs… » (`center::Event::StockExpiryRecap`).
//! The rows are marked (`stock_items.expiry_notified_on`, migration 0048)
//! in the transaction that writes the notifications: a second pass the same
//! day writes nothing, and a date moved is announced again.
//!
//! Once committed, the members whose channel includes notifications
//! (`users.reminder_channel`, as for the budget alerts) get a push on each
//! of their devices ([`push_recap`]): no payload, the service worker shows
//! the fixed text of `stock_expiry`. A member without a device reads the
//! recap in the notification centre only.
//!
//! It completes `jobs::stock_expiry`, which stays (user's decision of
//! 2026-11-05): that hourly pass writes one notification per item entering
//! the window, in the centre only, and skips the rows a recap already
//! announced for their date.

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

/// The Paris hour the recap goes at, fixed (user's decision of 2026-11-05;
/// a per-account setting is left to a later issue).
pub const RECAP_HOUR: u32 = 9;

/// How often the worker checks whether the day's recap is due: the recap
/// goes at most this late after [`RECAP_HOUR`].
pub const RECAP_POLL_INTERVAL: StdDuration = StdDuration::from_secs(5 * 60);

/// How long a push service keeps the recap for a device that is offline:
/// half a day. Read later, it is in the notification centre anyway.
pub const RECAP_PUSH_TTL_SECS: i64 = 12 * 3600;

/// The Paris day whose recap is due at `now`, if any: from [`RECAP_HOUR`]
/// on, once per day — `last` is the day of the last recap this process
/// ran. A process started after the hour runs the day's recap at once; the
/// rows already announced are not announced again.
pub fn due(now: DateTime<Utc>, last: Option<NaiveDate>) -> Option<NaiveDate> {
    let local = now.with_timezone(&rrule::Tz::Europe__Paris);
    let today = local.date_naive();
    (local.hour() >= RECAP_HOUR && last != Some(today)).then_some(today)
}

/// A stock row is in the recap of `today`: some left, a date at most
/// `EXPIRY_SOON_DAYS` days ahead, expired included, and that date not yet
/// announced by a recap.
pub fn in_recap(
    quantity: f64,
    expires_on: Option<NaiveDate>,
    notified_on: Option<NaiveDate>,
    today: NaiveDate,
) -> bool {
    quantity > 0.0
        && expires_on.is_some_and(|date| date <= expiring_until(today))
        && notified_on != expires_on
}

/// Polling worker. `pool` must be the `BYPASSRLS` admin pool
/// (`AppState.admin_db`): it reads `stock_items` and `group_members` and
/// writes `notifications` across every family. Without a VAPID key the
/// recap goes to the notification centre only.
pub async fn run(pool: PgPool, vapid: Option<std::sync::Arc<push::Vapid>>) {
    let mut ticker = interval(RECAP_POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let client = push::client();
    let mut last = None;
    loop {
        ticker.tick().await;
        let Some(today) = due(Utc::now(), last) else {
            continue;
        };
        let recipients = match exclusively(&pool, Job::StockExpiryRecap, recap(&pool, today)).await
        {
            Ok(Some(recap)) => {
                tracing::debug!(written = recap.written, "stock expiry recap done");
                recap.recipients
            }
            Ok(None) => {
                tracing::debug!("stock expiry recap skipped: another process runs it");
                Vec::new()
            }
            Err(e) => {
                tracing::error!(error = ?e, "stock expiry recap failed");
                continue;
            }
        };
        last = Some(today);
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

/// One pass, `today` being the Paris day: a parameter so the flow tests
/// can move the day.
pub async fn recap(pool: &PgPool, today: NaiveDate) -> anyhow::Result<Recap> {
    let mut tx = crate::db::begin(pool).await?;
    ensure_bypasses_rls(&mut tx).await.context(
        "stock expiry recap refusing to run: `stock_items` is FORCE ROW LEVEL SECURITY, so on a \
         role that does not bypass it no item would ever be found expiring. Point \
         ADMIN_DATABASE_URL at the BYPASSRLS admin_role (see apps/api/README.md).",
    )?;
    // The candidates; `in_recap` decides. Locked, so that a member's edit
    // waits for the pass rather than being marked announced unseen.
    let rows = sqlx::query!(
        r#"SELECT id, group_id, name, quantity, expires_on, expiry_notified_on
           FROM stock_items
           WHERE expires_on <= $1 AND quantity > 0
             AND expiry_notified_on IS DISTINCT FROM expires_on
           ORDER BY group_id, expires_on, name, id
           FOR UPDATE"#,
        expiring_until(today),
    )
    .fetch_all(&mut *tx)
    .await?;

    let mut families: BTreeMap<Uuid, (Vec<Uuid>, Vec<String>)> = BTreeMap::new();
    for row in rows {
        if in_recap(row.quantity, row.expires_on, row.expiry_notified_on, today) {
            let (ids, names) = families.entry(row.group_id).or_default();
            ids.push(row.id);
            names.push(row.name);
        }
    }

    let mut done = Recap::default();
    for (group_id, (ids, names)) in families {
        let members: Vec<Uuid> = sqlx::query_scalar!(
            r#"SELECT gm.user_id FROM group_members gm
               JOIN users u ON u.id = gm.user_id
               WHERE gm.group_id = $1
                 AND u.deactivated_at IS NULL AND u.deleted_at IS NULL"#,
            group_id,
        )
        .fetch_all(&mut *tx)
        .await?;
        done.written += center::insert_for(
            &mut tx,
            &members,
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
            "UPDATE stock_items SET expiry_notified_on = expires_on WHERE id = ANY($1)",
            &ids,
        )
        .execute(&mut *tx)
        .await?;
        done.recipients.extend(members);
    }
    tx.commit().await?;
    done.recipients.sort_unstable();
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
    fn the_recap_is_due_from_nine_in_paris_in_winter() {
        let at = |h, m| Utc.with_ymd_and_hms(2026, 11, 5, h, m, 0).unwrap();
        assert_eq!(due(at(7, 59), None), None);
        assert_eq!(due(at(8, 0), None), Some(day(2026, 11, 5)));
        assert_eq!(due(at(22, 59), None), Some(day(2026, 11, 5)));
    }

    /// …and 7:00 UTC in summer.
    #[test]
    fn the_recap_is_due_from_nine_in_paris_in_summer() {
        let at = |h, m| Utc.with_ymd_and_hms(2026, 7, 1, h, m, 0).unwrap();
        assert_eq!(due(at(6, 59), None), None);
        assert_eq!(due(at(7, 0), None), Some(day(2026, 7, 1)));
    }

    /// Once a day: not again the day it ran, again the next day from 9:00.
    #[test]
    fn the_recap_is_due_once_a_day() {
        let today = day(2026, 11, 5);
        let at = |d, h| Utc.with_ymd_and_hms(2026, 11, d, h, 0, 0).unwrap();
        assert_eq!(due(at(5, 12), Some(today)), None);
        // 23:00 UTC is already the 6th in Paris (midnight), but before 9:00.
        assert_eq!(due(at(5, 23), Some(today)), None);
        assert_eq!(due(at(6, 7), Some(today)), None);
        assert_eq!(due(at(6, 8), Some(today)), Some(day(2026, 11, 6)));
        // A recap missed for days: today's, once.
        assert_eq!(due(at(9, 8), Some(today)), Some(day(2026, 11, 9)));
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
