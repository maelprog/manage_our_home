//! The budget's cap alerts (#536): an expense that takes the month's
//! spending of the family to 80 % of its cap, past its cap, or past the cap
//! of its category writes a notification to the owners and the admins of
//! the family (`can_access_budget`), each threshold once per month and per
//! scope (`budget_alerts`, migration 0037).
//!
//! What counts is the month's total before the write and after it, read
//! under the family's lock: [`watch`] before, [`Watch::fire`] after, in the
//! write's own transaction. The crossing itself is [`crossed`], pure. Only
//! the current month alerts (user's decision of 2026-10-30): an expense
//! entered or changed in a past month, or dated in a later one, writes
//! none. Each family turns either kind of alert off (`budget_settings`,
//! [`get_alert_settings`] and [`patch_alert_settings`]); both are on by
//! default.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::Json;
use chrono::{Datelike, Months, NaiveDate, Utc};
use manage_our_home_shared::dto::budget::{BudgetAlertSettings, UpdateBudgetAlertSettings};
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::budget::{can_access_budget, require_budget_access};
use crate::error::{AppError, AppResult};
use crate::notifications::center::{self, Event, NewNotification};
use crate::stocks::items::day_or_today;
use crate::AppState;

/// A threshold of a cap that an alert reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Threshold {
    /// 80 % of the cap reached — reaching it exactly counts.
    EightyPercent,
    /// The cap exceeded: strictly above it, as `cap_usage`'s `over_cap`
    /// that the summary shows. Reaching it is not exceeding it.
    OverCap,
}

impl Threshold {
    /// The value of `budget_alerts.threshold`.
    pub fn percent(self) -> i16 {
        match self {
            Threshold::EightyPercent => 80,
            Threshold::OverCap => 100,
        }
    }

    /// `total_cents` has reached this threshold of `cap_cents`, compared
    /// exactly (no rounding of the share) and without overflow.
    pub fn reached(self, total_cents: i64, cap_cents: i64) -> bool {
        let (total, cap) = (i128::from(total_cents), i128::from(cap_cents));
        match self {
            Threshold::EightyPercent => total * 100 >= cap * 80,
            Threshold::OverCap => total > cap,
        }
    }
}

/// The family's cap alerts at 80 % and past it (user's decision of
/// 2026-10-30).
pub const FAMILY_THRESHOLDS: [Threshold; 2] = [Threshold::EightyPercent, Threshold::OverCap];
/// A category's: past its cap only.
pub const CATEGORY_THRESHOLDS: [Threshold; 1] = [Threshold::OverCap];

/// The `thresholds` that a total going from `before_cents` to
/// `after_cents` crosses upward, in their order: not reached before,
/// reached after. None without a cap, nor for a cap of zero, which the API
/// never stores; none either for a total that does not grow.
pub fn crossed(
    thresholds: &[Threshold],
    before_cents: i64,
    after_cents: i64,
    cap_cents: Option<i64>,
) -> Vec<Threshold> {
    let Some(cap) = cap_cents.filter(|&cap| cap > 0) else {
        return Vec::new();
    };
    thresholds
        .iter()
        .copied()
        .filter(|t| !t.reached(before_cents, cap) && t.reached(after_cents, cap))
        .collect()
}

/// `day` falls in the month of `today`.
pub fn in_month_of(day: NaiveDate, today: NaiveDate) -> bool {
    (day.year(), day.month()) == (today.year(), today.month())
}

type Tx<'c> = sqlx::Transaction<'c, sqlx::Postgres>;

/// The family's switches, its cap, and the family's lock.
struct LockedSettings {
    monthly_cap_cents: Option<i64>,
    global_cap_alerts: bool,
    category_cap_alerts: bool,
}

/// The family's `budget_settings` row, written with its defaults on first
/// use and locked for the rest of the transaction: the family's lock, as in
/// `create_budget_category`. Two expenses of one family written at once
/// thus read their totals one after the other, and neither misses the
/// crossing the other one completes.
async fn lock_settings(tx: &mut Tx<'_>, group_id: Uuid) -> AppResult<LockedSettings> {
    sqlx::query!(
        "INSERT INTO budget_settings (group_id) VALUES ($1) ON CONFLICT (group_id) DO NOTHING",
        group_id,
    )
    .execute(&mut **tx)
    .await?;
    Ok(sqlx::query_as!(
        LockedSettings,
        r#"SELECT monthly_cap_cents, global_cap_alerts, category_cap_alerts
           FROM budget_settings WHERE group_id = $1 FOR UPDATE"#,
        group_id,
    )
    .fetch_one(&mut **tx)
    .await?)
}

/// The category whose cap an expense may cross.
struct WatchedCategory {
    id: Uuid,
    name: String,
    cap_cents: i64,
}

/// A month's spending: the family's, and the watched category's.
struct Totals {
    family: i64,
    category: i64,
}

async fn totals(
    tx: &mut Tx<'_>,
    group_id: Uuid,
    month: NaiveDate,
    next_month: NaiveDate,
    category_id: Option<Uuid>,
) -> AppResult<Totals> {
    let row = sqlx::query!(
        r#"SELECT COALESCE(SUM(amount_cents), 0)::bigint AS "family!",
                  COALESCE(SUM(amount_cents) FILTER (WHERE category_id = $4), 0)::bigint
                      AS "category!"
           FROM budget_entries
           WHERE group_id = $1 AND spent_at >= $2 AND spent_at < $3"#,
        group_id,
        month,
        next_month,
        category_id,
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(Totals {
        family: row.family,
        category: row.category,
    })
}

/// The month's totals read before an expense is written, and the caps they
/// are held against — see [`watch`].
pub(crate) struct Watch {
    group_id: Uuid,
    month: NaiveDate,
    next_month: NaiveDate,
    /// `None` when the family has no cap or turned these alerts off.
    family_cap_cents: Option<i64>,
    /// `None` when the expense has no category, its category no cap, or
    /// the family turned these alerts off.
    category: Option<WatchedCategory>,
    before: Totals,
}

/// To call in an expense's transaction before writing it, with the date and
/// the category it will have once written; then [`Watch::fire`] once it is.
/// `None` when no alert can follow: a date out of the current month in
/// Paris, or no cap watched. Otherwise the family's lock is taken
/// ([`lock_settings`]) and the month's totals read.
///
/// Only the expense's month after the write is watched, and only its
/// category after the write: a total the write lowers crosses nothing.
pub(crate) async fn watch(
    tx: &mut Tx<'_>,
    group_id: Uuid,
    spent_at: NaiveDate,
    category_id: Option<Uuid>,
) -> AppResult<Option<Watch>> {
    if !in_month_of(spent_at, day_or_today(None, Utc::now())) {
        return Ok(None);
    }
    let settings = lock_settings(tx, group_id).await?;
    let family_cap_cents = settings
        .monthly_cap_cents
        .filter(|_| settings.global_cap_alerts);
    let category = match category_id {
        Some(id) if settings.category_cap_alerts => sqlx::query!(
            "SELECT name, monthly_cap_cents FROM budget_categories WHERE id = $1 AND group_id = $2",
            id,
            group_id,
        )
        .fetch_optional(&mut **tx)
        .await?
        .and_then(|c| {
            c.monthly_cap_cents.map(|cap_cents| WatchedCategory {
                id,
                name: c.name,
                cap_cents,
            })
        }),
        _ => None,
    };
    if family_cap_cents.is_none() && category.is_none() {
        return Ok(None);
    }
    let month = spent_at.with_day(1).expect("day 1 exists");
    let next_month = month
        .checked_add_months(Months::new(1))
        .ok_or_else(|| AppError::BadRequest("invalid_spent_at".into()))?;
    let before = totals(
        tx,
        group_id,
        month,
        next_month,
        category.as_ref().map(|c| c.id),
    )
    .await?;
    Ok(Some(Watch {
        group_id,
        month,
        next_month,
        family_cap_cents,
        category,
        before,
    }))
}

impl Watch {
    /// Reads the month's totals again, now that the expense is written,
    /// and sends an alert for each threshold crossed.
    pub(crate) async fn fire(self, tx: &mut Tx<'_>) -> AppResult<()> {
        let after = totals(
            tx,
            self.group_id,
            self.month,
            self.next_month,
            self.category.as_ref().map(|c| c.id),
        )
        .await?;
        for threshold in crossed(
            &FAMILY_THRESHOLDS,
            self.before.family,
            after.family,
            self.family_cap_cents,
        ) {
            self.send(tx, None, threshold).await?;
        }
        if let Some(category) = &self.category {
            for threshold in crossed(
                &CATEGORY_THRESHOLDS,
                self.before.category,
                after.category,
                Some(category.cap_cents),
            ) {
                self.send(tx, Some(category), threshold).await?;
            }
        }
        Ok(())
    }

    /// One alert, unless this family already had it this month
    /// (`budget_alerts`): a notification for each owner and admin whose
    /// account is active, the author of the expense included — the alert
    /// is about the month's spending, not about their own action.
    async fn send(
        &self,
        tx: &mut Tx<'_>,
        category: Option<&WatchedCategory>,
        threshold: Threshold,
    ) -> AppResult<()> {
        let first = sqlx::query_scalar!(
            r#"INSERT INTO budget_alerts (group_id, month, category_id, threshold)
               VALUES ($1, $2, $3, $4)
               ON CONFLICT DO NOTHING
               RETURNING true AS "first!""#,
            self.group_id,
            self.month,
            category.map(|c| c.id),
            threshold.percent(),
        )
        .fetch_optional(&mut **tx)
        .await?
        .is_some();
        if !first {
            return Ok(());
        }
        let recipients: Vec<Uuid> = sqlx::query!(
            r#"SELECT gm.user_id, gm.role::text AS "role!"
               FROM group_members gm
               JOIN users u ON u.id = gm.user_id
               WHERE gm.group_id = $1
                 AND u.deactivated_at IS NULL AND u.deleted_at IS NULL"#,
            self.group_id,
        )
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .filter(|m| can_access_budget(&m.role))
        .map(|m| m.user_id)
        .collect();
        let event = match (category, threshold) {
            (Some(c), _) => Event::BudgetCategoryCapExceeded { category: &c.name },
            (None, Threshold::EightyPercent) => Event::BudgetCapNear,
            (None, Threshold::OverCap) => Event::BudgetCapExceeded,
        };
        center::insert_for(
            tx,
            &recipients,
            &NewNotification {
                group_id: self.group_id,
                event,
                object_id: category.map(|c| c.id),
                actor_id: None,
                dedup_key: None,
            },
        )
        .await?;
        Ok(())
    }
}

/// The family's switches; a family without a settings row has both on.
pub async fn get_alert_settings(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<Json<BudgetAlertSettings>> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_budget_access(&mut tx, group_id, auth.user_id).await?;
    let settings = sqlx::query_as!(
        BudgetAlertSettings,
        "SELECT global_cap_alerts, category_cap_alerts FROM budget_settings WHERE group_id = $1",
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or(BudgetAlertSettings {
        global_cap_alerts: true,
        category_cap_alerts: true,
    });
    tx.commit().await?;
    Ok(Json(settings))
}

/// Sets the switches the body names and keeps the other. Same bar as every
/// budget route, before the body is looked at (#531).
pub async fn patch_alert_settings(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    body: Result<Json<UpdateBudgetAlertSettings>, JsonRejection>,
) -> AppResult<Json<BudgetAlertSettings>> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_budget_access(&mut tx, group_id, auth.user_id).await?;
    let Json(body) = body?;
    let settings = sqlx::query_as!(
        BudgetAlertSettings,
        r#"INSERT INTO budget_settings (group_id, global_cap_alerts, category_cap_alerts)
           VALUES ($1, COALESCE($2, true), COALESCE($3, true))
           ON CONFLICT (group_id) DO UPDATE SET
               global_cap_alerts = COALESCE($2, budget_settings.global_cap_alerts),
               category_cap_alerts = COALESCE($3, budget_settings.category_cap_alerts),
               updated_at = now()
           RETURNING global_cap_alerts, category_cap_alerts"#,
        group_id,
        body.global_cap_alerts,
        body.category_cap_alerts,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(settings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use Threshold::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// Reaching 80 % exactly is crossing it: 800,00 € of a 1 000,00 € cap.
    #[test]
    fn reaching_eighty_percent_exactly_crosses_it() {
        assert_eq!(
            crossed(&FAMILY_THRESHOLDS, 70_000, 80_000, Some(100_000)),
            [EightyPercent]
        );
        // One cent short does not.
        assert!(crossed(&FAMILY_THRESHOLDS, 70_000, 79_999, Some(100_000)).is_empty());
    }

    /// 80 % of a cap that is no multiple of 5 cents: the share is compared
    /// exactly, not rounded (80 % of 999 cents is 799.2).
    #[test]
    fn eighty_percent_is_compared_without_rounding() {
        assert!(crossed(&FAMILY_THRESHOLDS, 0, 799, Some(999)).is_empty());
        assert_eq!(
            crossed(&FAMILY_THRESHOLDS, 0, 800, Some(999)),
            [EightyPercent]
        );
    }

    /// Reaching a cap is not exceeding it (`cap_usage`'s `over_cap`); one
    /// cent above is.
    #[test]
    fn a_cap_is_exceeded_strictly_above_it() {
        assert!(crossed(&CATEGORY_THRESHOLDS, 9_000, 10_000, Some(10_000)).is_empty());
        assert_eq!(
            crossed(&CATEGORY_THRESHOLDS, 9_000, 10_001, Some(10_000)),
            [OverCap]
        );
        // From exactly the cap to above it: crossed now.
        assert_eq!(
            crossed(&CATEGORY_THRESHOLDS, 10_000, 10_001, Some(10_000)),
            [OverCap]
        );
    }

    /// One expense that jumps both thresholds of the family's cap reports
    /// both, in their order.
    #[test]
    fn an_expense_can_cross_both_thresholds() {
        assert_eq!(
            crossed(&FAMILY_THRESHOLDS, 50_000, 120_000, Some(100_000)),
            [EightyPercent, OverCap]
        );
    }

    /// Past 80 % already, only the cap is left to cross.
    #[test]
    fn a_threshold_already_reached_is_not_crossed_again() {
        assert_eq!(
            crossed(&FAMILY_THRESHOLDS, 85_000, 101_000, Some(100_000)),
            [OverCap]
        );
        assert!(crossed(&FAMILY_THRESHOLDS, 85_000, 95_000, Some(100_000)).is_empty());
        assert!(crossed(&FAMILY_THRESHOLDS, 110_000, 130_000, Some(100_000)).is_empty());
    }

    /// An expense lowered, or deleted, never alerts — whatever it lowers
    /// from or to.
    #[test]
    fn a_lower_total_never_crosses() {
        for (before, after) in [(120_000, 50_000), (90_000, 79_000), (101_000, 100_000)] {
            assert!(
                crossed(&FAMILY_THRESHOLDS, before, after, Some(100_000)).is_empty(),
                "{before} -> {after}"
            );
        }
        assert!(crossed(&FAMILY_THRESHOLDS, 80_000, 80_000, Some(100_000)).is_empty());
    }

    /// No cap, no alert; nor for a cap of zero, which the API never stores.
    #[test]
    fn no_cap_no_alert() {
        assert!(crossed(&FAMILY_THRESHOLDS, 0, 1_000_000, None).is_empty());
        assert!(crossed(&FAMILY_THRESHOLDS, 0, 1_000_000, Some(0)).is_empty());
    }

    /// Totals near `i64::MAX` do not overflow the comparison.
    #[test]
    fn huge_totals_do_not_overflow() {
        assert_eq!(
            crossed(&FAMILY_THRESHOLDS, 0, i64::MAX, Some(i64::MAX)),
            [EightyPercent]
        );
        assert_eq!(
            crossed(&FAMILY_THRESHOLDS, 0, i64::MAX, Some(i64::MAX / 2)),
            [EightyPercent, OverCap]
        );
    }

    /// Only the current month alerts (user's decision of 2026-10-30): a
    /// day of a past month, or of a later one, does not.
    #[test]
    fn only_a_day_of_the_current_month_alerts() {
        let today = day(2026, 10, 30);
        assert!(in_month_of(day(2026, 10, 1), today));
        assert!(in_month_of(day(2026, 10, 31), today));
        assert!(!in_month_of(day(2026, 9, 30), today));
        assert!(!in_month_of(day(2026, 11, 1), today));
        assert!(!in_month_of(day(2025, 10, 15), today));
    }
}
