//! Expiry notifications (#534): a stock item whose « à consommer
//! jusqu'au » date comes within `EXPIRY_SOON_DAYS` — the window /stocks
//! already marks « bientôt » (`validation::stocks::expiry_status`) — writes
//! one notification for every member of its family.
//!
//! Once per item and date: the key `stock_expiry:<item>:<date>` is unique
//! per recipient (0036), so the hourly passes over the same window write
//! nothing more, and a date moved by a member is announced again. An item
//! already expired when it enters the stock, or whose quantity is zero, is
//! not announced. The window (3 days) is far inside the 30 days a
//! notification is kept, so a purged key cannot let the same date be
//! announced twice.

use std::time::Duration as StdDuration;

use anyhow::Context;
use chrono::{Duration, NaiveDate, Utc};
use manage_our_home_shared::validation::auth::paris_day;
use manage_our_home_shared::validation::stocks::EXPIRY_SOON_DAYS;
use sqlx::PgPool;
use tokio::time::{interval, MissedTickBehavior};

use crate::attachment_reconcile::ensure_bypasses_rls;
use crate::jobs::single_runner::{exclusively, Job};
use crate::notifications::center;

/// Once an hour: the window is counted in days, and the first tick after
/// a Paris midnight opens the next day's.
pub const EXPIRY_POLL_INTERVAL: StdDuration = StdDuration::from_secs(3600);

/// Polling worker. `pool` must be the `BYPASSRLS` admin pool
/// (`AppState.admin_db`): it reads `stock_items` and `group_members` and
/// writes `notifications` across every family.
pub async fn run(pool: PgPool) {
    let mut ticker = interval(EXPIRY_POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let today = paris_day(Utc::now());
        match exclusively(&pool, Job::StockExpiry, notify_expiring(&pool, today)).await {
            Ok(Some(written)) => tracing::debug!(written, "stock expiry pass done"),
            Ok(None) => tracing::debug!("stock expiry pass skipped: another process runs it"),
            Err(e) => tracing::error!(error = ?e, "stock expiry pass failed"),
        }
    }
}

/// The first and last expiry dates announced on `today`, both included:
/// from today to `EXPIRY_SOON_DAYS` days ahead, the « bientôt » window.
pub fn window(today: NaiveDate) -> (NaiveDate, NaiveDate) {
    (today, today + Duration::days(EXPIRY_SOON_DAYS))
}

/// One pass, `today` being the Paris day. Returns how many notifications
/// were written. A parameter so the flow tests can move the day.
pub async fn notify_expiring(pool: &PgPool, today: NaiveDate) -> anyhow::Result<u64> {
    let mut tx = crate::db::begin(pool).await?;
    ensure_bypasses_rls(&mut tx).await.context(
        "stock expiry pass refusing to run: `stock_items` is FORCE ROW LEVEL SECURITY, so on a \
         role that does not bypass it no item would ever be found expiring. Point \
         ADMIN_DATABASE_URL at the BYPASSRLS admin_role (see apps/api/README.md).",
    )?;
    let (from, to) = window(today);
    let items = sqlx::query!(
        r#"SELECT id, group_id, name, expires_on AS "expires_on!"
           FROM stock_items
           WHERE expires_on BETWEEN $1 AND $2 AND quantity > 0"#,
        from,
        to,
    )
    .fetch_all(&mut *tx)
    .await?;

    let mut written = 0;
    for item in items {
        written += center::insert_for_members(
            &mut tx,
            &center::NewNotification {
                group_id: item.group_id,
                event: center::Event::StockExpiry {
                    item: &item.name,
                    expires_on: item.expires_on,
                },
                object_id: Some(item.id),
                actor_id: None,
                dedup_key: Some(format!("stock_expiry:{}:{}", item.id, item.expires_on)),
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(written)
}

// The dedup key is purged with its notification: the window must end well
// before, or the same date would be announced again. Checked at compile
// time.
const _: () = assert!(
    EXPIRY_SOON_DAYS < crate::jobs::retention_purge::NOTIFICATION_RETENTION_DAYS,
    "an item could be announced twice for the same date"
);

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// The window is the one /stocks marks « bientôt »: today included, and
    /// `EXPIRY_SOON_DAYS` days ahead included.
    #[test]
    fn the_window_is_the_soon_status() {
        use manage_our_home_shared::validation::stocks::{expiry_status, ExpiryStatus};
        let today = day(2026, 10, 30);
        let (from, to) = window(today);
        assert_eq!((from, to), (day(2026, 10, 30), day(2026, 11, 2)));
        assert_eq!(expiry_status(Some(from), today), ExpiryStatus::Soon);
        assert_eq!(expiry_status(Some(to), today), ExpiryStatus::Soon);
        assert_eq!(
            expiry_status(Some(from - Duration::days(1)), today),
            ExpiryStatus::Expired
        );
        assert_eq!(
            expiry_status(Some(to + Duration::days(1)), today),
            ExpiryStatus::Ok
        );
    }
}
