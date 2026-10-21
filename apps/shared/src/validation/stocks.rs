//! Pure, dependency-light logic for the Stocks screens (front epic #4,
//! issue #19), shared by `apps/web`'s SSR pages. Written test-first per
//! `.claude/CLAUDE.md`'s TDD process. Two concerns live here so the UI and
//! the fixed backend (`apps/api/src/stocks/`) can never drift:
//!
//! `validate_item_form` mirrors `apps/api/src/stocks/items.rs`'s create/update
//! guards (non-empty `name`, non-negative `quantity`/`reorder_threshold`)
//! so the form rejects before a round trip. A blank `unit` is not an error:
//! `normalize_unit` turns it into `DEFAULT_UNIT` (#458).
//!
//! `is_low_stock` mirrors the backend's derived `low_stock` flag
//! (`StockItemResponse::from_row`): an item is low when it has a reorder threshold
//! and its quantity is at or below it. The flag is derived on read, never
//! stored — this is the single source of truth the list/detail badge uses.
//!
//! `expiry_status` classifies an article's expiry date (`expires_on`, #401)
//! against a given "today". `apps/api` derives it on read into
//! `StockItemResponse::expiry_status`, like `low_stock`, with today taken in
//! Europe/Paris; the web pages render that field as is. It does not read the
//! clock: the caller passes the date, which keeps this crate wasm-clean and
//! the boundaries testable.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Why a stock-item create/edit form was rejected. Ordered to match the
/// backend's check order (`create_stock_item` / `update_stock_item`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemFormError {
    NameRequired,
    QuantityNegative,
    ThresholdNegative,
}

/// Mirrors `apps/api/src/stocks/items.rs`: `name` must be non-empty after
/// trimming, `quantity` must be `>= 0`, and a present `reorder_threshold`
/// must be `>= 0`. Kept in the same order the backend validates so the first
/// error surfaced matches what a forged request would hit.
pub fn validate_item_form(
    name: &str,
    quantity: f64,
    reorder_threshold: Option<f64>,
) -> Result<(), ItemFormError> {
    if name.trim().is_empty() {
        return Err(ItemFormError::NameRequired);
    }
    if quantity < 0.0 {
        return Err(ItemFormError::QuantityNegative);
    }
    if let Some(t) = reorder_threshold {
        if t < 0.0 {
            return Err(ItemFormError::ThresholdNegative);
        }
    }
    Ok(())
}

/// The unit an article gets when none is typed (#458). The SQL column
/// default (`'unit'`, migration 0004) is never reached: `apps/api` always
/// writes the normalized value.
pub const DEFAULT_UNIT: &str = "unité";

/// The unit to store for what the user typed: trimmed, or `DEFAULT_UNIT`
/// when blank. Shared by `apps/api` (create and update) and the web forms, so
/// an empty field is never an error.
pub fn normalize_unit(raw: &str) -> String {
    let unit = raw.trim();
    if unit.is_empty() {
        DEFAULT_UNIT.to_string()
    } else {
        unit.to_string()
    }
}

/// Mirror of the backend's derived `low_stock` flag
/// (`reorder_threshold.map(|t| quantity <= t).unwrap_or(false)`): an item with
/// no threshold is never low; otherwise it is low exactly when its quantity is
/// at or below the threshold.
pub fn is_low_stock(quantity: f64, reorder_threshold: Option<f64>) -> bool {
    reorder_threshold.map(|t| quantity <= t).unwrap_or(false)
}

/// How many days ahead an expiry date counts as "bientôt" (#401): an article
/// whose date falls today or within the next `EXPIRY_SOON_DAYS` days is
/// flagged so it gets eaten first.
pub const EXPIRY_SOON_DAYS: i64 = 3;

/// Where an article stands against its expiry date. v1 keeps one date per
/// article — the nearest one — so this is a single verdict, not per batch.
/// On the wire: `"expired"`, `"soon"`, `"ok"`, `"unknown"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpiryStatus {
    /// The date is strictly before today.
    Expired,
    /// The date is today or at most `EXPIRY_SOON_DAYS` days ahead.
    Soon,
    /// The date is further ahead than that.
    Ok,
    /// No date recorded.
    Unknown,
}

/// Classifies `expires_on` against `today`. The expiry date itself is still
/// "à consommer jusqu'au" — the article becomes `Expired` the day after.
pub fn expiry_status(expires_on: Option<NaiveDate>, today: NaiveDate) -> ExpiryStatus {
    let Some(date) = expires_on else {
        return ExpiryStatus::Unknown;
    };
    let days_left = (date - today).num_days();
    if days_left < 0 {
        ExpiryStatus::Expired
    } else if days_left <= EXPIRY_SOON_DAYS {
        ExpiryStatus::Soon
    } else {
        ExpiryStatus::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- validate_item_form --------------------------------------------------

    #[test]
    fn empty_name_is_rejected() {
        assert_eq!(
            validate_item_form("   ", 1.0, None),
            Err(ItemFormError::NameRequired)
        );
    }

    #[test]
    fn negative_quantity_is_rejected() {
        assert_eq!(
            validate_item_form("Farine", -0.1, None),
            Err(ItemFormError::QuantityNegative)
        );
    }

    #[test]
    fn negative_threshold_is_rejected() {
        assert_eq!(
            validate_item_form("Farine", 1.0, Some(-1.0)),
            Err(ItemFormError::ThresholdNegative)
        );
    }

    #[test]
    fn zero_quantity_and_zero_threshold_are_accepted() {
        assert!(validate_item_form("Farine", 0.0, Some(0.0)).is_ok());
    }

    #[test]
    fn valid_form_without_threshold_is_accepted() {
        assert!(validate_item_form("Lait", 2.0, None).is_ok());
    }

    // -- normalize_unit ------------------------------------------------------

    #[test]
    fn default_unit_is_the_french_word() {
        assert_eq!(DEFAULT_UNIT, "unité");
    }

    #[test]
    fn empty_unit_becomes_the_default() {
        assert_eq!(normalize_unit(""), "unité");
    }

    #[test]
    fn blank_unit_becomes_the_default() {
        assert_eq!(normalize_unit("  \t "), "unité");
    }

    #[test]
    fn a_filled_unit_is_kept_trimmed() {
        assert_eq!(normalize_unit("  kg "), "kg");
        assert_eq!(normalize_unit("L"), "L");
    }

    // -- is_low_stock --------------------------------------------------------

    #[test]
    fn no_threshold_is_never_low() {
        assert!(!is_low_stock(0.0, None));
        assert!(!is_low_stock(100.0, None));
    }

    #[test]
    fn quantity_at_or_below_threshold_is_low() {
        assert!(is_low_stock(0.5, Some(0.5))); // boundary: at threshold
        assert!(is_low_stock(0.2, Some(0.5))); // below
    }

    #[test]
    fn quantity_above_threshold_is_not_low() {
        assert!(!is_low_stock(0.6, Some(0.5)));
    }

    // -- expiry_status -------------------------------------------------------

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn no_date_is_unknown() {
        assert_eq!(expiry_status(None, d(2026, 10, 7)), ExpiryStatus::Unknown);
    }

    #[test]
    fn a_date_before_today_is_expired() {
        assert_eq!(
            expiry_status(Some(d(2026, 10, 6)), d(2026, 10, 7)),
            ExpiryStatus::Expired
        );
        assert_eq!(
            expiry_status(Some(d(2025, 1, 1)), d(2026, 10, 7)),
            ExpiryStatus::Expired
        );
    }

    #[test]
    fn the_expiry_day_itself_is_soon_not_expired() {
        assert_eq!(
            expiry_status(Some(d(2026, 10, 7)), d(2026, 10, 7)),
            ExpiryStatus::Soon
        );
    }

    #[test]
    fn up_to_the_threshold_is_soon() {
        assert_eq!(EXPIRY_SOON_DAYS, 3);
        assert_eq!(
            expiry_status(Some(d(2026, 10, 8)), d(2026, 10, 7)),
            ExpiryStatus::Soon
        );
        assert_eq!(
            expiry_status(Some(d(2026, 10, 10)), d(2026, 10, 7)),
            ExpiryStatus::Soon
        );
    }

    #[test]
    fn one_day_past_the_threshold_is_ok() {
        assert_eq!(
            expiry_status(Some(d(2026, 10, 11)), d(2026, 10, 7)),
            ExpiryStatus::Ok
        );
    }

    #[test]
    fn the_threshold_counts_across_a_month_boundary() {
        assert_eq!(
            expiry_status(Some(d(2026, 11, 2)), d(2026, 10, 30)),
            ExpiryStatus::Soon
        );
        assert_eq!(
            expiry_status(Some(d(2026, 11, 3)), d(2026, 10, 30)),
            ExpiryStatus::Ok
        );
    }
}
