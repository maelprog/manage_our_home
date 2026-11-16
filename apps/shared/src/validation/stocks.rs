//! Pure, dependency-light logic for the Stocks screens (front epic #4,
//! issue #19), shared by `apps/web`'s SSR pages. Written test-first per
//! `.claude/CLAUDE.md`'s TDD process. Two concerns live here so the UI and
//! the fixed backend (`apps/api/src/stocks/`) can never drift:
//!
//! `validate_item_form` mirrors `apps/api/src/stocks/items.rs`'s create/update
//! guards (non-empty `name`, `quantity`/`reorder_threshold` non-negative,
//! finite and at most `MAX_QUANTITY`) so the form rejects before a round trip. A blank `unit` is not an error:
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

use super::recipes::without_binary_noise;

/// Why a stock-item create/edit form was rejected. Ordered to match the
/// backend's check order (`create_stock_item` / `update_stock_item`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemFormError {
    NameRequired,
    QuantityNegative,
    QuantityTooLarge,
    ThresholdNegative,
    ThresholdTooLarge,
}

/// The largest quantity, or reorder threshold, a stock line may carry (#649):
/// far above any household stock, far below where `float8` overflows (a sum
/// of two huge quantities used to end in a 500). The one bound of the stocks,
/// read by `apps/api` for a creation, a full edit, a quantity-only adjustment
/// and a reception, and by the web forms. The bound itself is accepted.
pub const MAX_QUANTITY: f64 = 1_000_000_000.0;

impl ItemFormError {
    /// The error code `apps/api` answers for this error, and the web forms
    /// read their message from.
    pub fn code(self) -> &'static str {
        match self {
            ItemFormError::NameRequired => "name_required",
            ItemFormError::QuantityNegative => "quantity_must_be_non_negative",
            // The reception's code: a value that is not a number at all
            // (`NaN`, infinity) is refused under it too.
            ItemFormError::QuantityTooLarge => "quantity_too_large",
            ItemFormError::ThresholdNegative => "reorder_threshold_must_be_non_negative",
            ItemFormError::ThresholdTooLarge => "reorder_threshold_too_large",
        }
    }
}

/// The quantity and the reorder threshold of a stock line: each `>= 0`,
/// finite and at most `MAX_QUANTITY`. The quantity is checked first; a
/// negative infinity is negative before it is infinite. `NaN` fails both
/// comparisons, so it is caught by the finiteness test, not by `< 0.0`.
pub fn check_quantities(
    quantity: f64,
    reorder_threshold: Option<f64>,
) -> Result<(), ItemFormError> {
    if quantity < 0.0 {
        return Err(ItemFormError::QuantityNegative);
    }
    if !quantity.is_finite() || quantity > MAX_QUANTITY {
        return Err(ItemFormError::QuantityTooLarge);
    }
    if let Some(t) = reorder_threshold {
        if t < 0.0 {
            return Err(ItemFormError::ThresholdNegative);
        }
        if !t.is_finite() || t > MAX_QUANTITY {
            return Err(ItemFormError::ThresholdTooLarge);
        }
    }
    Ok(())
}

/// Mirrors `apps/api/src/stocks/items.rs`: `name` must be non-empty after
/// trimming, then `check_quantities`. Kept in the same order the backend
/// validates so the first error surfaced matches what a forged request would
/// hit.
pub fn validate_item_form(
    name: &str,
    quantity: f64,
    reorder_threshold: Option<f64>,
) -> Result<(), ItemFormError> {
    if name.trim().is_empty() {
        return Err(ItemFormError::NameRequired);
    }
    check_quantities(quantity, reorder_threshold)
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

/// The metric symbols, in lower case: never a plural in `format_quantity`,
/// the only units `parse_pack_size` reads.
const METRIC_SYMBOLS: [&str; 7] = ["g", "kg", "mg", "l", "cl", "ml", "dl"];

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
///   paquets: `s`, or `x` after `au`/`eu`, `aux` for `al`, save the
///   few words that take an `s` all the same (« pneus », « festivals »);
///   one already ending in `s`, `x` or `z` stays as it is. A unit in
///   capitals takes its mark in capitals (« PAQUETS »). Only the first word turns
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
    let invariant = METRIC_SYMBOLS.contains(&article_key(unit).as_str()) || unit.ends_with('.');
    if invariant || quantity < 2.0 {
        return format!("{number} {unit}");
    }
    let (first, rest) = match unit.split_once(' ') {
        Some((first, rest)) => (first, format!(" {rest}")),
        None => (unit, String::new()),
    };
    // Compared in lower case, the mark written in the word's own case:
    // « PAQUET » reads « PAQUETS ».
    let lower = first.to_lowercase();
    let capitals = first.chars().any(char::is_alphabetic) && first == first.to_uppercase();
    let mark = |suffix: &str| {
        if capitals {
            suffix.to_uppercase()
        } else {
            suffix.to_string()
        }
    };
    // The words that take a plain `s` despite their ending.
    const S_PLURAL: [&str; 11] = [
        "pneu", "bleu", "émeu", "landau", "sarrau", "bal", "carnaval", "chacal", "festival",
        "récital", "régal",
    ];
    let plural = if lower.ends_with(['s', 'x', 'z']) {
        first.to_string()
    } else if S_PLURAL.contains(&lower.as_str()) {
        format!("{first}{}", mark("s"))
    } else if lower.ends_with("au") || lower.ends_with("eu") {
        format!("{first}{}", mark("x"))
    } else if lower.ends_with("al") {
        // `al` is two ASCII bytes whatever the case.
        format!("{}{}", &first[..first.len() - 2], mark("aux"))
    } else {
        format!("{first}{}", mark("s"))
    };
    format!("{number} {plural}{rest}")
}

/// A pack size read from Open Food Facts' `quantity` text (#805): the
/// amount and the unit the stock form is pre-filled with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PackSize {
    pub quantity: f64,
    pub unit: String,
}

/// Reads an Open Food Facts pack size (`quantity`, free text) as an amount
/// and a unit (#805): « 500 g », « 500g », « 1,5 L », and a multipack
/// multiplied out (« 2 x 75 g » is 150 g; `x`, `X` or `×`).
///
/// - The unit is a metric symbol (`METRIC_SYMBOLS`, in any case), written
///   one way: `L` for the litre, the others in lower case — a value
///   `normalize_unit` keeps as it is.
/// - The decimals take a comma or a point; the amount is rounded to three
///   decimals, like `format_quantity` writes it.
/// - A trailing estimated sign (`℮`) is dropped.
/// - Anything else is not read (`None`): a count of named things (« 6
///   tranches », « 12 oeufs » — no conversion to guess), other text before
///   or after the size, an amount of zero or above `MAX_QUANTITY`. The
///   caller then keeps the text as it is.
pub fn parse_pack_size(raw: &str) -> Option<PackSize> {
    let text = raw.trim();
    let text = text.strip_suffix('℮').unwrap_or(text).trim_end();
    let (first, rest) = leading_number(text)?;
    let rest = rest.trim_start();
    let (quantity, rest) = match rest.strip_prefix(['x', 'X', '×']) {
        Some(after) => {
            let (each, rest) = leading_number(after.trim_start())?;
            (first * each, rest.trim_start())
        }
        None => (first, rest),
    };
    let unit = article_key(rest);
    if !METRIC_SYMBOLS.contains(&unit.as_str()) {
        return None;
    }
    let quantity = (quantity * 1000.0).round() / 1000.0;
    if !(quantity > 0.0 && quantity <= MAX_QUANTITY) {
        return None;
    }
    let unit = if unit == "l" { "L".to_string() } else { unit };
    Some(PackSize { quantity, unit })
}

/// How much of each metric unit makes the base one of its kind (#824):
/// the gram for a mass, the millilitre for a volume. Keyed like
/// `METRIC_SYMBOLS`, in lower case.
fn metric_scale(unit: &str) -> Option<(char, f64)> {
    match article_key(unit).as_str() {
        "mg" => Some(('m', 0.001)),
        "g" => Some(('m', 1.0)),
        "kg" => Some(('m', 1000.0)),
        "ml" => Some(('v', 1.0)),
        "cl" => Some(('v', 10.0)),
        "dl" => Some(('v', 100.0)),
        "l" => Some(('v', 1000.0)),
        _ => None,
    }
}

/// `quantity` written in `from`, converted into `to` (#824).
///
/// - Two units that read the same (`article_key`: « G » and « g ») keep
///   the quantity as it is, whatever they are.
/// - Two metric units of one kind convert: a mass (`mg`, `g`, `kg`) into a
///   mass, a volume (`ml`, `cl`, `dl`, `L`) into a volume; 40 cl is 0,4 L.
///   The result carries no binary noise.
/// - Anything else does not convert (`None`): a mass and a volume, a named
///   unit (« paquet », « unité ») against another one.
pub fn convert_quantity(quantity: f64, from: &str, to: &str) -> Option<f64> {
    if article_key(from) == article_key(to) {
        return Some(quantity);
    }
    let (from_kind, from_scale) = metric_scale(from)?;
    let (to_kind, to_scale) = metric_scale(to)?;
    (from_kind == to_kind).then(|| without_binary_noise(quantity * from_scale / to_scale))
}

/// A need counted in whole packs (#824): `count` packs of `size`, the
/// smallest number of packs that holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PackRounding {
    pub count: u64,
    pub size: PackSize,
}

/// The packs of a scanned article a grocery line comes to (#824): its pack
/// size (`pack`, Open Food Facts' text, read by `parse_pack_size`) and the
/// line's need converted into its unit (`convert_quantity`), counted up to
/// the next whole pack. 380 g of a 500 g pack is 1 pack, 1 kg is 2.
///
/// `None`, the line shown as it is, when the line has no quantity or a
/// zero one, when its unit does not convert into the pack's, or when the
/// pack size does not read.
pub fn round_to_packs(
    quantity: Option<f64>,
    unit: Option<&str>,
    pack: &str,
) -> Option<PackRounding> {
    let quantity = quantity.filter(|q| *q > 0.0)?;
    let size = parse_pack_size(pack)?;
    let need = convert_quantity(quantity, unit?, &size.unit)?;
    // 2.1 / 0.7 is 3.0000000000000004: the noise goes before the ceiling,
    // or a whole number of packs takes one too many.
    let count = without_binary_noise(need / size.quantity).ceil();
    (count.is_finite() && count >= 1.0).then_some(PackRounding {
        count: count as u64,
        size,
    })
}

/// The packs as the grocery list writes them after the need (#824): « 1
/// paquet (500 g) », « 3 paquets (500 g) ».
pub fn packs_label(packs: &PackRounding) -> String {
    format!(
        "{} ({})",
        format_quantity(packs.count as f64, "paquet"),
        format_quantity(packs.size.quantity, &packs.size.unit)
    )
}

/// The packs as the reception's hint words them under the unit (#824):
/// « ≈ 1 paquet de 1 kg ».
pub fn packs_hint(packs: &PackRounding) -> String {
    format!(
        "≈ {} de {}",
        format_quantity(packs.count as f64, "paquet"),
        format_quantity(packs.size.quantity, &packs.size.unit)
    )
}

/// The quantity and the unit a received article enters the stock in when
/// a scanned article bears its name (#824): converted into that article's
/// unit, so that it merges into it (`merge_target` compares the units as
/// they are). `scanned` holds the family's scanned articles only.
///
/// `None`, the article entered as typed, when no scanned article bears its
/// name, when one of them already has its unit, or when its unit converts
/// into none of theirs. Among several, the smallest id; a conversion that
/// would leave `(0, MAX_QUANTITY]` is not made.
pub fn scanned_entry(
    name: &str,
    quantity: f64,
    unit: &str,
    scanned: &[StockCandidate<'_>],
) -> Option<(f64, String)> {
    let key = article_key(name);
    let mut namesakes: Vec<&StockCandidate<'_>> = scanned
        .iter()
        .filter(|c| article_key(c.name) == key)
        .collect();
    if namesakes
        .iter()
        .any(|c| article_key(c.unit) == article_key(unit))
    {
        return None;
    }
    namesakes.sort_by_key(|c| c.id);
    namesakes.into_iter().find_map(|c| {
        convert_quantity(quantity, unit, c.unit)
            .filter(|q| *q > 0.0 && *q <= MAX_QUANTITY)
            .map(|q| (q, c.unit.to_string()))
    })
}

/// The number `text` starts with — digits, then at most one comma or point
/// followed by digits — and what comes after it.
fn leading_number(text: &str) -> Option<(f64, &str)> {
    let int_end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    if int_end == 0 {
        return None;
    }
    let mut end = int_end;
    if let Some(after) = text[int_end..].strip_prefix([',', '.']) {
        let decimals = after
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after.len());
        if decimals == 0 {
            return None;
        }
        end = int_end + 1 + decimals;
    }
    let (number, rest) = text.split_at(end);
    // A second separator (« 1,2,3 ») is never a number.
    if rest.starts_with([',', '.']) {
        return None;
    }
    let value = number.replace(',', ".").parse::<f64>().ok()?;
    Some((value, rest))
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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

/// The last expiry date the Stocks tab's pastille counts on `today` (#564):
/// an article dated up to it, past dates included, is one `expiry_status`
/// calls `Expired` or `Soon` — the API's `expiry_status` field and the
/// detail page's badge. `apps/api` counts
/// `expires_on <= expiring_until(today)` in SQL rather than classifying
/// every line.
pub fn expiring_until(today: NaiveDate) -> NaiveDate {
    today + chrono::Duration::days(EXPIRY_SOON_DAYS)
}

/// How a received grocery item enters the stock (#471): added to an existing
/// line (`merge`) or as a line of its own (`new`). On the wire: `"merge"`,
/// `"new"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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

    // -- quantity upper bound (#649) -----------------------------------------

    #[test]
    fn the_bound_is_the_receptions_one_billion() {
        assert_eq!(MAX_QUANTITY, 1_000_000_000.0);
    }

    #[test]
    fn a_quantity_and_a_threshold_at_the_bound_are_accepted() {
        assert!(validate_item_form("Farine", MAX_QUANTITY, Some(MAX_QUANTITY)).is_ok());
        assert!(check_quantities(MAX_QUANTITY, Some(MAX_QUANTITY)).is_ok());
    }

    #[test]
    fn a_quantity_above_the_bound_is_rejected() {
        for q in [MAX_QUANTITY + 1.0, 1e300, f64::MAX] {
            assert_eq!(
                validate_item_form("Farine", q, None),
                Err(ItemFormError::QuantityTooLarge),
                "{q}"
            );
            assert_eq!(
                check_quantities(q, None),
                Err(ItemFormError::QuantityTooLarge),
                "{q}"
            );
        }
    }

    #[test]
    fn a_quantity_that_is_not_finite_is_rejected() {
        for q in [f64::NAN, f64::INFINITY] {
            assert_eq!(
                validate_item_form("Farine", q, None),
                Err(ItemFormError::QuantityTooLarge),
                "{q}"
            );
        }
        // Negative infinity is first of all negative.
        assert_eq!(
            check_quantities(f64::NEG_INFINITY, None),
            Err(ItemFormError::QuantityNegative)
        );
    }

    #[test]
    fn a_threshold_above_the_bound_is_rejected() {
        for t in [MAX_QUANTITY + 1.0, 1e300] {
            assert_eq!(
                validate_item_form("Farine", 1.0, Some(t)),
                Err(ItemFormError::ThresholdTooLarge),
                "{t}"
            );
        }
    }

    #[test]
    fn a_threshold_that_is_not_finite_is_rejected() {
        for t in [f64::NAN, f64::INFINITY] {
            assert_eq!(
                check_quantities(1.0, Some(t)),
                Err(ItemFormError::ThresholdTooLarge),
                "{t}"
            );
        }
        assert_eq!(
            check_quantities(1.0, Some(f64::NEG_INFINITY)),
            Err(ItemFormError::ThresholdNegative)
        );
    }

    #[test]
    fn the_quantity_is_checked_before_the_threshold() {
        assert_eq!(
            check_quantities(f64::NAN, Some(f64::NAN)),
            Err(ItemFormError::QuantityTooLarge)
        );
    }

    #[test]
    fn each_error_carries_the_code_the_api_answers() {
        assert_eq!(ItemFormError::NameRequired.code(), "name_required");
        assert_eq!(
            ItemFormError::QuantityNegative.code(),
            "quantity_must_be_non_negative"
        );
        assert_eq!(ItemFormError::QuantityTooLarge.code(), "quantity_too_large");
        assert_eq!(
            ItemFormError::ThresholdNegative.code(),
            "reorder_threshold_must_be_non_negative"
        );
        assert_eq!(
            ItemFormError::ThresholdTooLarge.code(),
            "reorder_threshold_too_large"
        );
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

    // -- expiring_until (#564) -----------------------------------------------

    #[test]
    fn the_last_counted_day_is_the_last_soon_day() {
        assert_eq!(expiring_until(d(2026, 10, 7)), d(2026, 10, 10));
        assert_eq!(expiring_until(d(2026, 10, 30)), d(2026, 11, 2));
    }

    #[test]
    fn a_date_up_to_the_last_counted_day_is_exactly_an_expired_or_soon_one() {
        // The pastille's `count(*)` (`expires_on <= expiring_until(today)`)
        // and `expiry_status`'s Expired/Soon must name the same dates.
        let today = d(2026, 10, 7);
        for offset in -10..=10 {
            let date = today + chrono::Duration::days(offset);
            let flagged = matches!(
                expiry_status(Some(date), today),
                ExpiryStatus::Expired | ExpiryStatus::Soon
            );
            assert_eq!(date <= expiring_until(today), flagged, "J{offset:+}");
        }
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
    fn the_plural_of_the_au_eu_al_exceptions_takes_an_s() {
        assert_eq!(format_quantity(4.0, "pneu"), "4 pneus");
        assert_eq!(format_quantity(2.0, "landau"), "2 landaus");
        assert_eq!(format_quantity(2.0, "festival"), "2 festivals");
    }

    #[test]
    fn a_unit_in_capitals_takes_its_plural_in_capitals() {
        assert_eq!(format_quantity(2.0, "PAQUET"), "2 PAQUETS");
        assert_eq!(format_quantity(2.0, "BOCAL"), "2 BOCAUX");
        assert_eq!(format_quantity(2.0, "PRUNEAU"), "2 PRUNEAUX");
        assert_eq!(format_quantity(2.0, "Paquet"), "2 Paquets");
        assert_eq!(format_quantity(2.0, "PNEU"), "2 PNEUS");
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

    // -- parse_pack_size (#805) ----------------------------------------------

    fn pack(quantity: f64, unit: &str) -> Option<PackSize> {
        Some(PackSize {
            quantity,
            unit: unit.to_string(),
        })
    }

    #[test]
    fn the_issue_cases_read_as_an_amount_and_a_unit() {
        let cases: [(&str, Option<PackSize>); 9] = [
            ("500g", pack(500.0, "g")),
            ("500 g", pack(500.0, "g")),
            ("2x75g", pack(150.0, "g")),
            ("2 x 75 g", pack(150.0, "g")),
            ("1,5 L", pack(1.5, "L")),
            ("33 cl", pack(33.0, "cl")),
            ("12 oeufs", None),
            ("", None),
            ("Paquet familial", None),
        ];
        for (raw, expected) in cases {
            assert_eq!(parse_pack_size(raw), expected, "{raw:?}");
        }
    }

    #[test]
    fn a_count_of_named_things_is_not_read() {
        for raw in ["6 tranches", "4 pots", "12", "g", "   "] {
            assert_eq!(parse_pack_size(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn every_metric_symbol_is_read_and_written_one_way() {
        let cases = [
            ("250 G", "g"),
            ("1 kg", "kg"),
            ("1 KG", "kg"),
            ("300 mg", "mg"),
            ("1 l", "L"),
            ("1L", "L"),
            ("25 cL", "cl"),
            ("750 ml", "ml"),
            ("5 dl", "dl"),
        ];
        for (raw, unit) in cases {
            assert_eq!(
                parse_pack_size(raw).map(|p| p.unit),
                Some(unit.to_string()),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn the_unit_read_is_one_normalize_unit_keeps() {
        for raw in ["500 g", "1,5 L", "33 cl", "2 x 75 g", "1 kg"] {
            let unit = parse_pack_size(raw).unwrap().unit;
            assert_eq!(normalize_unit(&unit), unit, "{raw:?}");
        }
    }

    #[test]
    fn decimals_take_a_comma_or_a_point_and_spaces_around_are_dropped() {
        assert_eq!(parse_pack_size(" 1.5 L "), pack(1.5, "L"));
        assert_eq!(parse_pack_size("0,25 kg"), pack(0.25, "kg"));
    }

    #[test]
    fn the_estimated_sign_after_the_size_is_dropped() {
        assert_eq!(parse_pack_size("500 g ℮"), pack(500.0, "g"));
        assert_eq!(parse_pack_size("1L℮"), pack(1.0, "L"));
    }

    #[test]
    fn a_multipack_multiplies_whatever_its_sign() {
        assert_eq!(parse_pack_size("4 X 125 g"), pack(500.0, "g"));
        assert_eq!(parse_pack_size("6 × 1,5 L"), pack(9.0, "L"));
        // Rounded to three decimals, like the list writes it.
        assert_eq!(parse_pack_size("3 x 0,1 l"), pack(0.3, "L"));
    }

    #[test]
    fn a_size_with_other_text_around_it_is_not_read() {
        for raw in [
            "500 g e",
            "env. 500 g",
            "500 g (2 x 250 g)",
            "2 x 75",
            "x 75 g",
            "500 g 2",
        ] {
            assert_eq!(parse_pack_size(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn a_zero_or_out_of_bound_size_is_not_read() {
        for raw in [
            "0 g",
            "0 x 75 g",
            "2 x 0 g",
            "2000000000 g",
            "1,2,3 g",
            "-5 g",
        ] {
            assert_eq!(parse_pack_size(raw), None, "{raw:?}");
        }
    }

    // -- convert_quantity (#824) ---------------------------------------------

    #[test]
    fn the_same_unit_keeps_the_quantity_whatever_it_is() {
        assert_eq!(convert_quantity(380.0, "g", "g"), Some(380.0));
        assert_eq!(convert_quantity(2.0, " Paquet ", "paquet"), Some(2.0));
        assert_eq!(convert_quantity(3.0, "unité", "UNITÉ"), Some(3.0));
    }

    #[test]
    fn a_mass_converts_into_a_mass_and_a_volume_into_a_volume() {
        assert_eq!(convert_quantity(40.0, "cl", "L"), Some(0.4));
        assert_eq!(convert_quantity(800.0, "g", "kg"), Some(0.8));
        assert_eq!(convert_quantity(1.2, "kg", "g"), Some(1200.0));
        assert_eq!(convert_quantity(250.0, "mg", "g"), Some(0.25));
        assert_eq!(convert_quantity(3.0, "dl", "ml"), Some(300.0));
        assert_eq!(convert_quantity(1.5, "l", "CL"), Some(150.0));
    }

    #[test]
    fn a_conversion_carries_no_binary_noise() {
        // 0.3 × 1000 is 300.00000000000006 in f64.
        assert_eq!(convert_quantity(0.3, "kg", "g"), Some(300.0));
        assert_eq!(convert_quantity(70.0, "cl", "L"), Some(0.7));
    }

    #[test]
    fn units_of_two_kinds_or_named_ones_do_not_convert() {
        assert_eq!(convert_quantity(300.0, "g", "cl"), None);
        assert_eq!(convert_quantity(1.0, "L", "kg"), None);
        assert_eq!(convert_quantity(2.0, "unité", "g"), None);
        assert_eq!(convert_quantity(2.0, "paquet", "boîte"), None);
        assert_eq!(convert_quantity(2.0, "", "g"), None);
    }

    // -- round_to_packs (#824) -----------------------------------------------

    fn packs(count: u64, quantity: f64, unit: &str) -> Option<PackRounding> {
        Some(PackRounding {
            count,
            size: PackSize {
                quantity,
                unit: unit.to_string(),
            },
        })
    }

    #[test]
    fn a_need_with_a_remainder_takes_the_next_whole_pack() {
        assert_eq!(
            round_to_packs(Some(380.0), Some("g"), "500 g"),
            packs(1, 500.0, "g")
        );
        assert_eq!(
            round_to_packs(Some(1250.0), Some("g"), "400 g"),
            packs(4, 400.0, "g")
        );
    }

    #[test]
    fn a_divisible_need_takes_just_its_packs() {
        assert_eq!(
            round_to_packs(Some(1000.0), Some("g"), "500 g"),
            packs(2, 500.0, "g")
        );
        assert_eq!(
            round_to_packs(Some(2.0), Some("L"), "1 L"),
            packs(2, 1.0, "L")
        );
        assert_eq!(
            round_to_packs(Some(0.3), Some("L"), "10 cl"),
            packs(3, 10.0, "cl")
        );
        // 2.1 / 0.7 is 3.0000000000000004 in f64: still 3 packs, not 4.
        assert_eq!(
            round_to_packs(Some(2.1), Some("L"), "0,7 L"),
            packs(3, 0.7, "L")
        );
    }

    #[test]
    fn a_need_in_another_unit_of_its_kind_is_converted() {
        assert_eq!(
            round_to_packs(Some(40.0), Some("cl"), "1 L"),
            packs(1, 1.0, "L")
        );
        assert_eq!(
            round_to_packs(Some(1.2), Some("kg"), "500 g"),
            packs(3, 500.0, "g")
        );
        assert_eq!(
            round_to_packs(Some(750.0), Some("G"), "1 kg"),
            packs(1, 1.0, "kg")
        );
    }

    #[test]
    fn a_need_in_an_incompatible_unit_is_not_rounded() {
        assert_eq!(round_to_packs(Some(300.0), Some("g"), "20 cl"), None);
        assert_eq!(round_to_packs(Some(2.0), Some("unité"), "500 g"), None);
        assert_eq!(round_to_packs(Some(2.0), None, "500 g"), None);
    }

    #[test]
    fn an_unreadable_pack_size_rounds_nothing() {
        for pack in ["env. 4 portions", "6 tranches", "", "0 g"] {
            assert_eq!(round_to_packs(Some(2.0), Some("g"), pack), None, "{pack:?}");
        }
    }

    #[test]
    fn a_zero_or_absent_quantity_is_not_rounded() {
        assert_eq!(round_to_packs(Some(0.0), Some("g"), "500 g"), None);
        assert_eq!(round_to_packs(None, Some("g"), "500 g"), None);
        assert_eq!(round_to_packs(Some(-1.0), Some("g"), "500 g"), None);
        assert_eq!(round_to_packs(Some(f64::NAN), Some("g"), "500 g"), None);
    }

    // -- packs_label / packs_hint (#824) -------------------------------------

    #[test]
    fn the_packs_read_in_french_with_their_size() {
        let one = packs(1, 500.0, "g").unwrap();
        let three = packs(3, 1.5, "L").unwrap();
        assert_eq!(packs_label(&one), "1 paquet (500 g)");
        assert_eq!(packs_label(&three), "3 paquets (1,5 L)");
        assert_eq!(packs_hint(&one), "≈ 1 paquet de 500 g");
        assert_eq!(packs_hint(&three), "≈ 3 paquets de 1,5 L");
    }

    // -- scanned_entry (#824) ------------------------------------------------

    #[test]
    fn a_received_article_takes_the_unit_of_the_scanned_one_of_its_name() {
        let scanned = [line(7, "Sucre en poudre", "kg", None)];
        assert_eq!(
            scanned_entry(" sucre en POUDRE ", 800.0, "g", &scanned),
            Some((0.8, "kg".to_string()))
        );
    }

    #[test]
    fn a_received_article_already_in_the_scanned_unit_is_left_as_typed() {
        let scanned = [line(7, "Sucre", "kg", None), line(8, "Sucre", "g", None)];
        assert_eq!(scanned_entry("Sucre", 800.0, "g", &scanned), None);
    }

    #[test]
    fn no_scanned_article_of_the_name_or_of_the_kind_converts_nothing() {
        let scanned = [line(7, "Farine", "kg", None), line(8, "Sucre", "L", None)];
        assert_eq!(scanned_entry("Sucre", 800.0, "g", &scanned), None);
        assert_eq!(scanned_entry("Sucre", 2.0, "paquet", &scanned), None);
        assert_eq!(scanned_entry("Sucre", 800.0, "g", &[]), None);
    }

    #[test]
    fn among_several_scanned_articles_the_smallest_id_gives_the_unit() {
        let scanned = [line(9, "Lait", "cl", None), line(8, "Lait", "L", None)];
        assert_eq!(
            scanned_entry("Lait", 500.0, "ml", &scanned),
            Some((0.5, "L".to_string()))
        );
    }

    #[test]
    fn a_conversion_out_of_bounds_is_not_made() {
        let scanned = [line(7, "Sel", "mg", None)];
        assert_eq!(scanned_entry("Sel", MAX_QUANTITY, "kg", &scanned), None);
    }
}
