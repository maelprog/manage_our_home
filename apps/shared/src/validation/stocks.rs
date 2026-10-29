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
use uuid::Uuid;

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

/// A quantity and its unit as the stock list reads them (#527): « 2
/// paquets », « 250 g », « 1,5 L », and the bare number for
/// `DEFAULT_UNIT` (« 3 », not « 3 unités »).
///
/// - The number is written the French way: a comma before the decimals,
///   at most three of them, trailing zeros dropped (`0.1 + 0.2` reads
///   « 0,3 »).
/// - A symbol (`g`, `kg`, `L`, `cl`…, in any case) or an abbreviation
///   ending in a dot never takes a plural.
/// - A named unit takes the French plural from 2 on — 1,5 paquet, 2
///   paquets: `s`, or `x` after `au`/`eu`, `aux` for `al`; one already
///   ending in `s`, `x` or `z` stays as it is. Only the first word turns
///   plural (« boîtes de conserve »). What the member typed is never
///   turned singular.
pub fn format_quantity(quantity: f64, unit: &str) -> String {
    // Three decimals at most, then the trailing zeros and the point go.
    let quantity = (quantity * 1000.0).round() / 1000.0;
    let rounded = format!("{quantity:.3}");
    let trimmed = rounded.trim_end_matches('0').trim_end_matches('.');
    let number = if trimmed == "-0" { "0" } else { trimmed }.replace('.', ",");

    let unit = unit.trim();
    if unit.is_empty() || article_key(unit) == DEFAULT_UNIT {
        return number;
    }
    const SYMBOLS: [&str; 7] = ["g", "kg", "mg", "l", "cl", "ml", "dl"];
    let invariant = SYMBOLS.contains(&article_key(unit).as_str()) || unit.ends_with('.');
    if invariant || quantity < 2.0 {
        return format!("{number} {unit}");
    }
    let (first, rest) = match unit.split_once(' ') {
        Some((first, rest)) => (first, format!(" {rest}")),
        None => (unit, String::new()),
    };
    let plural = if first.ends_with(['s', 'x', 'z']) {
        first.to_string()
    } else if first.ends_with("au") || first.ends_with("eu") {
        format!("{first}x")
    } else if let Some(stem) = first.strip_suffix("al") {
        format!("{stem}aux")
    } else {
        format!("{first}s")
    };
    format!("{number} {plural}{rest}")
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

/// How a received grocery item enters the stock (#471): added to an existing
/// line (`merge`) or as a line of its own (`new`). On the wire: `"merge"`,
/// `"new"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceptionMode {
    Merge,
    New,
}

/// A stock line a received article could be merged into: the fields the
/// proposal rule reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StockCandidate<'a> {
    pub id: Uuid,
    pub name: &'a str,
    pub unit: &'a str,
    pub expires_on: Option<NaiveDate>,
}

/// The key two article names (or two units) are compared on: trimmed and
/// lowercased, the heuristic the grocery-list generation already matches
/// stock names with.
pub fn article_key(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// The stock line a received article merges into by default, or `None` when
/// it makes a new line (#471, « choix par article »).
///
/// A line is eligible when its name and its unit match the article's
/// (`article_key`) and no date different from its own is typed: the article
/// has no date, or the same date as the line. Among several eligible lines
/// the choice is deterministic: the line whose date equals the article's
/// (an undated article prefers an undated line), then the earliest date —
/// even one already past: the rule does not read today — then the smallest
/// id.
pub fn merge_target(
    name: &str,
    unit: &str,
    expires_on: Option<NaiveDate>,
    candidates: &[StockCandidate<'_>],
) -> Option<Uuid> {
    let (name, unit) = (article_key(name), article_key(unit));
    candidates
        .iter()
        .filter(|c| article_key(c.name) == name && article_key(c.unit) == unit)
        .filter(|c| expires_on.is_none() || c.expires_on == expires_on)
        // `false` sorts first: the same date, then the earliest date, then
        // the smallest id. Past the first key the remaining lines all share
        // one date (a dated article) or are all dated (an undated article
        // whose undated lines won), so `None` never meets `Some` there.
        .min_by_key(|c| (c.expires_on != expires_on, c.expires_on, c.id))
        .map(|c| c.id)
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

    // -- merge_target (#471) -------------------------------------------------

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn line(
        n: u128,
        name: &'static str,
        unit: &'static str,
        expires_on: Option<NaiveDate>,
    ) -> StockCandidate<'static> {
        StockCandidate {
            id: id(n),
            name,
            unit,
            expires_on,
        }
    }

    #[test]
    fn an_undated_article_merges_into_the_undated_line_of_the_same_name_and_unit() {
        let stock = [line(1, "Lait", "L", None)];
        assert_eq!(merge_target("lait", "L", None, &stock), Some(id(1)));
    }

    #[test]
    fn names_and_units_compare_trimmed_and_case_insensitively() {
        let stock = [line(1, "  Lait ", "l", None)];
        assert_eq!(merge_target("LAIT", " L ", None, &stock), Some(id(1)));
    }

    #[test]
    fn a_different_unit_makes_a_new_line() {
        let stock = [line(1, "Lait", "bouteille", None)];
        assert_eq!(merge_target("lait", "L", None, &stock), None);
    }

    #[test]
    fn a_different_name_makes_a_new_line() {
        let stock = [line(1, "Lait d'avoine", "L", None)];
        assert_eq!(merge_target("lait", "L", None, &stock), None);
    }

    #[test]
    fn no_stock_at_all_makes_a_new_line() {
        assert_eq!(merge_target("lait", "L", None, &[]), None);
    }

    #[test]
    fn a_date_different_from_the_line_makes_a_new_line() {
        let stock = [line(1, "Lait", "L", Some(d(2026, 11, 1)))];
        assert_eq!(
            merge_target("lait", "L", Some(d(2026, 11, 5)), &stock),
            None
        );
    }

    #[test]
    fn a_date_typed_against_an_undated_line_makes_a_new_line() {
        let stock = [line(1, "Lait", "L", None)];
        assert_eq!(
            merge_target("lait", "L", Some(d(2026, 11, 5)), &stock),
            None
        );
    }

    #[test]
    fn the_same_date_as_the_line_merges() {
        let stock = [line(1, "Lait", "L", Some(d(2026, 11, 5)))];
        assert_eq!(
            merge_target("lait", "L", Some(d(2026, 11, 5)), &stock),
            Some(id(1))
        );
    }

    #[test]
    fn an_undated_article_merges_into_a_dated_line() {
        let stock = [line(1, "Lait", "L", Some(d(2026, 11, 5)))];
        assert_eq!(merge_target("lait", "L", None, &stock), Some(id(1)));
    }

    #[test]
    fn a_dated_article_picks_the_line_with_its_date_among_several() {
        let stock = [
            line(1, "Lait", "L", Some(d(2026, 11, 1))),
            line(2, "Lait", "L", None),
            line(3, "Lait", "L", Some(d(2026, 11, 5))),
        ];
        assert_eq!(
            merge_target("lait", "L", Some(d(2026, 11, 5)), &stock),
            Some(id(3))
        );
    }

    #[test]
    fn an_undated_article_prefers_the_undated_line() {
        let stock = [
            line(1, "Lait", "L", Some(d(2026, 11, 1))),
            line(2, "Lait", "L", None),
        ];
        assert_eq!(merge_target("lait", "L", None, &stock), Some(id(2)));
    }

    #[test]
    fn without_an_undated_line_an_undated_article_takes_the_earliest_date() {
        let stock = [
            line(1, "Lait", "L", Some(d(2026, 11, 9))),
            line(2, "Lait", "L", Some(d(2026, 11, 1))),
            line(3, "Lait", "L", Some(d(2026, 11, 5))),
        ];
        assert_eq!(merge_target("lait", "L", None, &stock), Some(id(2)));
    }

    #[test]
    fn the_earliest_date_wins_even_when_it_is_past() {
        // The rule has no "today": a line already expired is still the
        // earliest one.
        let stock = [
            line(1, "Lait", "L", Some(d(2026, 11, 5))),
            line(2, "Lait", "L", Some(d(2020, 1, 1))),
        ];
        assert_eq!(merge_target("lait", "L", None, &stock), Some(id(2)));
    }

    #[test]
    fn equal_lines_are_broken_by_the_smallest_id_whatever_their_order() {
        let a = line(7, "Lait", "L", None);
        let b = line(3, "Lait", "L", None);
        assert_eq!(merge_target("lait", "L", None, &[a, b]), Some(id(3)));
        assert_eq!(merge_target("lait", "L", None, &[b, a]), Some(id(3)));
    }

    // -- format_quantity (#527) ----------------------------------------------

    #[test]
    fn the_issue_cases_read_as_the_mock_up_writes_them() {
        assert_eq!(format_quantity(2.0, "paquet"), "2 paquets");
        assert_eq!(format_quantity(1.0, "paquet"), "1 paquet");
        assert_eq!(format_quantity(250.0, "g"), "250 g");
        assert_eq!(format_quantity(1.5, "L"), "1,5 L");
        assert_eq!(format_quantity(3.0, "unité"), "3");
    }

    #[test]
    fn the_default_unit_is_left_out_whatever_its_case_or_spaces() {
        assert_eq!(format_quantity(1.0, DEFAULT_UNIT), "1");
        assert_eq!(format_quantity(0.0, " Unité "), "0");
        assert_eq!(format_quantity(2.5, ""), "2,5");
    }

    #[test]
    fn a_symbol_never_takes_a_plural_in_any_case() {
        for unit in ["g", "kg", "mg", "L", "l", "cl", "cL", "ml", "dl"] {
            assert_eq!(format_quantity(3.0, unit), format!("3 {unit}"));
        }
        assert_eq!(format_quantity(4.0, "pcs."), "4 pcs.");
    }

    #[test]
    fn a_named_unit_is_singular_under_two() {
        assert_eq!(format_quantity(0.0, "pot"), "0 pot");
        assert_eq!(format_quantity(1.5, "paquet"), "1,5 paquet");
        assert_eq!(format_quantity(1.999, "boîte"), "1,999 boîte");
    }

    #[test]
    fn a_named_unit_takes_the_french_plural() {
        assert_eq!(format_quantity(4.0, "tranche"), "4 tranches");
        assert_eq!(format_quantity(2.0, "bocal"), "2 bocaux");
        assert_eq!(format_quantity(2.0, "pruneau"), "2 pruneaux");
        assert_eq!(format_quantity(3.0, "feu"), "3 feux");
        assert_eq!(format_quantity(2.0, "pois"), "2 pois");
        assert_eq!(format_quantity(2.0, "noix"), "2 noix");
        assert_eq!(format_quantity(2.0, "paquets"), "2 paquets");
    }

    #[test]
    fn only_the_first_word_turns_plural() {
        assert_eq!(
            format_quantity(3.0, "boîte de conserve"),
            "3 boîtes de conserve"
        );
    }

    #[test]
    fn what_was_typed_is_never_turned_singular() {
        assert_eq!(format_quantity(1.0, "paquets"), "1 paquets");
    }

    #[test]
    fn decimals_are_french_and_rounded_to_three() {
        assert_eq!(format_quantity(0.1 + 0.2, "kg"), "0,3 kg");
        assert_eq!(format_quantity(0.125, "kg"), "0,125 kg");
        assert_eq!(format_quantity(2.0004, "kg"), "2 kg");
        assert_eq!(format_quantity(1250.0, "g"), "1250 g");
    }
}
