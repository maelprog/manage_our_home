//! The school holidays cache's daily refresh (#563): the periods of zones
//! A, B and C for the current school year and the next, read from the
//! Éducation nationale's open data (`agenda::school_holidays`) and written
//! over the cached ones of those years in one transaction.
//!
//! A failed pass — the service unreachable or slow (`outbound_http`'s
//! timeouts), an error status, an answer that is not the expected JSON or
//! that `parse_calendar` refuses — writes nothing: the cache keeps what it
//! had, the error is logged, and the next day's pass tries again.

use std::time::Duration as StdDuration;

use anyhow::Context;
use chrono::{NaiveDate, Utc};
use manage_our_home_shared::validation::auth::paris_day;
use serde_json::Value;
use sqlx::PgPool;
use tokio::time::{interval, MissedTickBehavior};

use crate::agenda::school_holidays::{
    parse_calendar, records_query, records_url, replace_cache, school_years,
};
use crate::jobs::single_runner::{exclusively, Job};

/// Once a day: the calendar changes a few times a year at most.
pub const REFRESH_INTERVAL: StdDuration = StdDuration::from_secs(24 * 3600);

/// Polling worker. The first pass runs at start-up. `pool` may be the
/// application pool: `school_holidays` has no RLS to bypass.
pub async fn run(pool: PgPool, base_url: String) {
    let mut ticker = interval(REFRESH_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let today = paris_day(Utc::now());
        let pass = refresh(&pool, crate::outbound_http::client(), &base_url, today);
        match exclusively(&pool, Job::SchoolHolidays, pass).await {
            Ok(Some(periods)) => tracing::info!(periods, "school holidays refreshed"),
            Ok(None) => tracing::debug!("school holidays refresh skipped: another process runs it"),
            Err(e) => tracing::error!(
                error = ?e,
                "school holidays refresh failed, the cached periods are kept"
            ),
        }
    }
}

/// One pass, `today` being the Paris day. Returns how many periods were
/// cached. The client and the base URL are parameters so the flow tests can
/// point it at a local stub, or at nothing.
pub async fn refresh(
    pool: &PgPool,
    client: &reqwest::Client,
    base_url: &str,
    today: NaiveDate,
) -> anyhow::Result<usize> {
    let years = school_years(today);
    let body: Value = client
        .get(records_url(base_url))
        .query(&records_query(&years))
        .send()
        .await
        .context("school calendar request")?
        .error_for_status()
        .context("school calendar status")?
        .json()
        .await
        .context("school calendar body")?;
    let periods = parse_calendar(&body, &years).context("school calendar answer")?;
    replace_cache(pool, &years, &periods)
        .await
        .context("school holidays cache")?;
    Ok(periods.len())
}
