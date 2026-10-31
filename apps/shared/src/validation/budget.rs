//! Pure, dependency-light logic for the Budget screens (front epic F7, issue
//! #22), shared by `apps/web`'s SSR pages. Written test-first per
//! `.claude/CLAUDE.md`'s TDD process. Everything money- or permission-shaped
//! lives here so the UI and the fixed backend (`apps/api/src/budget/`) can
//! never drift:
//!
//! - `validate_entry_form` mirrors the backend's create/update guards
//!   (non-empty `name` after trim, a finite non-negative `amount` of at most
//!   `AMOUNT_MAX_EUROS`, through `amount_to_cents`) in the same
//!   order the backend checks them (name, then amount), so the first error the
//!   form surfaces is the one a forged request would hit.
//! - `can_access_budget` is the bar in front of the whole budget (#531): only
//!   an owner or an admin reaches it. The backend uses this very function.
//! - `can_modify` mirrors `apps/api/src/budget/mod.rs::can_modify`: the entry's
//!   creator, or a group owner/admin, may edit or delete it — past
//!   `can_access_budget`, which already keeps a standard member out.
//! - `format_euros` / `amount_input_value` format the euro `f64` the API
//!   returns for display (two decimals, French comma + `€`) and for
//!   number-input pre-fill (two decimals, dot). The backend stores cents; the
//!   front only ever displays/parses euros, so rounding lives in one tested
//!   place.
//! - `format_period` turns a summary period (first-of-month `NaiveDate`) into
//!   the French month-and-year label the monthly summary shows.
//! - Categories and caps (#532): `validate_category_name`,
//!   `is_category_color` and `cap_to_cents` are the backend's own guards for
//!   a category and for the monthly caps (global and per category);
//!   `cap_usage` turns a month's total and a cap into the percentage and the
//!   overrun the summary returns; `parse_month` reads the summary's `month`
//!   parameter.
//! - The budget pages (#533): `month_param`, `previous_month`, `next_month`
//!   and `in_month` drive the month selector; `remaining` and
//!   `cap_share_label` write what a cap leaves and how much of it is used;
//!   `parse_cap_input` reads a cap field; `category_color_name` names a
//!   colour key in French.

use chrono::{Datelike, Months, NaiveDate};

/// Why a budget-entry add/edit form was rejected. Ordered to match the
/// backend's check order (`create_budget_entry` / `update_budget_entry`): name
/// first, then amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryFormError {
    NameRequired,
    AmountNegative,
    AmountTooLarge,
}

impl EntryFormError {
    /// The backend error code this maps to (the front pre-validates, but the
    /// inline message and a defensively-mapped 400 must read identically).
    pub fn code(&self) -> &'static str {
        match self {
            EntryFormError::NameRequired => "name_required",
            EntryFormError::AmountNegative => "amount_must_be_non_negative",
            EntryFormError::AmountTooLarge => AMOUNT_TOO_LARGE,
        }
    }
}

/// The largest amount the budget takes, in euros (user's decision of
/// 2026-10-31, #648): an expense, a grocery item's price, a monthly cap.
/// The amount itself is accepted; one cent more is not. Without it, a huge
/// `f64` saturated at `i64::MAX` cents on its way to the database and threw
/// every sum and cap percentage off.
pub const AMOUNT_MAX_EUROS: f64 = 1e10;

/// The 400's error code for an amount above [`AMOUNT_MAX_EUROS`], the same
/// for an expense, a price and a cap.
pub const AMOUNT_TOO_LARGE: &str = "amount_too_large";

/// An expense's amount, or a grocery item's price, from the euros of the API
/// boundary to the cents the database keeps (`budget_entries.amount_cents`
/// is BIGINT so that sums carry no floating-point error): finite and
/// `>= 0`, else `AmountNegative`; at most [`AMOUNT_MAX_EUROS`] once rounded
/// to the cent, else `AmountTooLarge`.
pub fn amount_to_cents(amount: f64) -> Result<i64, EntryFormError> {
    if !amount.is_finite() || amount < 0.0 {
        return Err(EntryFormError::AmountNegative);
    }
    bounded_cents(amount).ok_or(EntryFormError::AmountTooLarge)
}

/// A finite euro amount rounded to the cent, or `None` above
/// [`AMOUNT_MAX_EUROS`]. The bound is checked on the `f64` before the cast,
/// which would otherwise saturate in silence.
fn bounded_cents(amount: f64) -> Option<i64> {
    let cents = (amount * 100.0).round();
    if cents > AMOUNT_MAX_EUROS * 100.0 {
        return None;
    }
    Some(cents as i64)
}

/// Mirrors `apps/api/src/budget/entries.rs`: `name` must be non-empty after
/// trimming, and `amount` must pass `amount_to_cents` — the very check the
/// backend's `to_cents` makes: finite and `>= 0`
/// (`amount_must_be_non_negative`), at most `AMOUNT_MAX_EUROS`
/// (`amount_too_large`). Kept in the same order the backend
/// validates (name, then amount) so the first error surfaced matches what a
/// forged request would hit.
pub fn validate_entry_form(name: &str, amount: f64) -> Result<(), EntryFormError> {
    if name.trim().is_empty() {
        return Err(EntryFormError::NameRequired);
    }
    amount_to_cents(amount)?;
    Ok(())
}

/// Mirror of `apps/api/src/budget/mod.rs::can_modify`: the entry's creator, or
/// a group owner/admin, may edit or delete it. Used to decide whether to render
/// the edit/delete controls (the backend still 403s a forged request).
/// Create/read/set-price are *not* gated by this — `can_access_budget` is.
pub fn can_modify(role: &str, is_creator: bool) -> bool {
    is_creator || role == "owner" || role == "admin"
}

/// Whether a member of the given role reaches the family's budget at all
/// (#531): owners and admins do, a standard member — a child of the family —
/// does not, nor does any role this function does not name. This is the bar
/// in front of every `/groups/:id/budget-entries*` route and the grocery
/// item's price (`apps/api/src/budget/`), and the one `apps/web` reads to
/// leave out the Budget tab, the home page's budget card, the price field on
/// the grocery list and the `/budget` pages. `can_modify` only applies past
/// it.
pub fn can_access_budget(role: &str) -> bool {
    role == "owner" || role == "admin"
}

/// Formats a euro amount for display: two decimals, French decimal comma, and a
/// trailing `€` (`12.5` → `"12,50 €"`, `0.0` → `"0,00 €"`). The API speaks
/// euros as `f64`; this is the single place display rounding happens.
pub fn format_euros(amount: f64) -> String {
    format!("{:.2} €", amount).replace('.', ",")
}

/// Formats a euro amount for a `<input type=number>` pre-fill: two decimals
/// with a dot (`12.5` → `"12.50"`), which HTML number inputs accept. Kept
/// separate from `format_euros` so the display comma never leaks into a form
/// value the browser would reject.
pub fn amount_input_value(amount: f64) -> String {
    format!("{:.2}", amount)
}

/// French month name (1 = janvier … 12 = décembre). Falls back to an empty
/// string for an out-of-range month, which `chrono` never produces.
pub fn month_name_fr(month: u32) -> &'static str {
    match month {
        1 => "janvier",
        2 => "février",
        3 => "mars",
        4 => "avril",
        5 => "mai",
        6 => "juin",
        7 => "juillet",
        8 => "août",
        9 => "septembre",
        10 => "octobre",
        11 => "novembre",
        12 => "décembre",
        _ => "",
    }
}

/// French month-and-year label for a summary period (the first-of-month
/// `NaiveDate` the backend returns), e.g. `2026-07-01` → `"juillet 2026"`.
pub fn format_period(period: NaiveDate) -> String {
    format!("{} {}", month_name_fr(period.month()), period.year())
}

/// Longest category name, in characters, after trimming (#532).
pub const CATEGORY_NAME_MAX: usize = 40;

/// Most categories a family may have (#532). The API refuses the 31st.
pub const CATEGORIES_MAX: i64 = 30;

/// The closed list of colours a category may take (#532), as named keys:
/// the database stores the key, never a hex value, and `apps/web` maps each
/// key onto a `DESIGN.md` token (#533). The five are the swatches of the
/// budget mock-up, in its order — Alimentation `#2F7A9E`, Enfants `#7A5EA8`,
/// Santé `#457F44`, Maison `#B35C2E`, Loisirs `#8E630F` (light theme) — given
/// here only to say which key is which. The migration's `CHECK` repeats the
/// list (`0035_budget_categories.sql`).
pub const CATEGORY_COLORS: [&str; 5] = ["blue", "violet", "green", "orange", "ochre"];

/// Why a category, or a cap, was refused (#532), in the order the backend
/// checks a category: name, colour, cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryError {
    NameRequired,
    NameTooLong,
    InvalidColor,
    CapNotPositive,
    AmountTooLarge,
}

impl CategoryError {
    /// The 400's error code.
    pub fn code(&self) -> &'static str {
        match self {
            CategoryError::NameRequired => "name_required",
            CategoryError::NameTooLong => "name_too_long",
            CategoryError::InvalidColor => "invalid_color",
            CategoryError::CapNotPositive => "cap_must_be_positive",
            CategoryError::AmountTooLarge => AMOUNT_TOO_LARGE,
        }
    }
}

/// A category's name, trimmed: 1 to `CATEGORY_NAME_MAX` characters. Its
/// uniqueness within the family, ignoring case, is the database's.
pub fn validate_category_name(name: &str) -> Result<&str, CategoryError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CategoryError::NameRequired);
    }
    if name.chars().count() > CATEGORY_NAME_MAX {
        return Err(CategoryError::NameTooLong);
    }
    Ok(name)
}

/// Whether `color` is one of `CATEGORY_COLORS`, exactly.
pub fn is_category_color(color: &str) -> bool {
    CATEGORY_COLORS.contains(&color)
}

/// A monthly cap, global or per category, from the euros of the API boundary
/// to the cents the database keeps: finite, and at least one cent once
/// rounded — a cap of zero would make every expense an overrun — and at
/// most [`AMOUNT_MAX_EUROS`] (`AmountTooLarge`). Removing a cap is sending
/// `null`, not `0`.
pub fn cap_to_cents(amount: f64) -> Result<i64, CategoryError> {
    if !amount.is_finite() {
        return Err(CategoryError::CapNotPositive);
    }
    let cents = bounded_cents(amount).ok_or(CategoryError::AmountTooLarge)?;
    if cents <= 0 {
        return Err(CategoryError::CapNotPositive);
    }
    Ok(cents)
}

/// How much of a cap a month's spend uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapUsage {
    /// The total as a share of the cap, in percent, rounded down — so 100
    /// means the cap is reached, and an overrun of less than one percent
    /// still reads 100. `None` without a cap.
    pub percent: Option<i64>,
    /// The total is strictly above the cap: reaching it is not exceeding it.
    pub over_cap: bool,
}

/// `total_cents` against `cap_cents`. No cap, or a cap of zero (which the
/// API never stores), gives no percentage and no overrun.
pub fn cap_usage(total_cents: i64, cap_cents: Option<i64>) -> CapUsage {
    match cap_cents {
        Some(cap) if cap > 0 => {
            let percent = i128::from(total_cents) * 100 / i128::from(cap);
            CapUsage {
                percent: Some(i64::try_from(percent).unwrap_or(i64::MAX)),
                over_cap: total_cents > cap,
            }
        }
        _ => CapUsage {
            percent: None,
            over_cap: false,
        },
    }
}

/// The summary's `month` parameter, `YYYY-MM`, as the first day of that
/// month. Anything else — a day, a missing zero, a month 13 — is `None`.
pub fn parse_month(month: &str) -> Option<NaiveDate> {
    let (year, mon) = month.split_once('-')?;
    if year.len() != 4 || mon.len() != 2 {
        return None;
    }
    if !year.bytes().chain(mon.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    NaiveDate::from_ymd_opt(year.parse().ok()?, mon.parse().ok()?, 1)
}

// -- the budget pages (#533) --------------------------------------------------

/// `month` as the summary's `?month=` reads it (`parse_month`): `YYYY-MM`.
pub fn month_param(month: NaiveDate) -> String {
    format!("{:04}-{:02}", month.year(), month.month())
}

/// The first day of the month before `month`'s, whatever day `month` is.
pub fn previous_month(month: NaiveDate) -> Option<NaiveDate> {
    month.with_day(1)?.checked_sub_months(Months::new(1))
}

/// The first day of the month after `month`'s, whatever day `month` is.
pub fn next_month(month: NaiveDate) -> Option<NaiveDate> {
    month.with_day(1)?.checked_add_months(Months::new(1))
}

/// Whether `day` falls in the month `month` is a day of: the expenses the
/// page lists under the month the summary is about.
pub fn in_month(day: NaiveDate, month: NaiveDate) -> bool {
    day.year() == month.year() && day.month() == month.month()
}

/// What is left of `cap` once `total` is spent, in euros; negative once
/// the cap is exceeded. Both are rounded to the cent and subtracted as
/// cents, as the backend sums them, so the remainder never shows a binary
/// floating-point residue.
pub fn remaining(total: f64, cap: f64) -> f64 {
    let cents = |euros: f64| (euros * 100.0).round() as i64;
    (cents(cap) - cents(total)) as f64 / 100.0
}

/// The share of a cap a month's spend uses, as a bar's caption:
/// « 25 % de 200,00 € ». `percent` is the summary's, rounded down.
pub fn cap_share_label(percent: i64, cap: f64) -> String {
    format!("{percent} % de {}", format_euros(cap))
}

/// A cap field of the settings page, global or per category: empty is no
/// cap; anything else is euros, with a dot or a French comma, and must pass
/// `cap_to_cents` — the very check the API makes, `AmountTooLarge` above
/// `AMOUNT_MAX_EUROS` included — or it is `CapNotPositive`, unparseable
/// included.
pub fn parse_cap_input(input: &str) -> Result<Option<f64>, CategoryError> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }
    let amount: f64 = input
        .replace(',', ".")
        .parse()
        .map_err(|_| CategoryError::CapNotPositive)?;
    cap_to_cents(amount)?;
    Ok(Some(amount))
}

/// The French name of a category colour key, as the pages label it: a
/// colour is never told by its swatch alone. Empty for a key that is not
/// one of `CATEGORY_COLORS`.
pub fn category_color_name(color: &str) -> &'static str {
    match color {
        "blue" => "Bleu",
        "violet" => "Violet",
        "green" => "Vert",
        "orange" => "Orange",
        "ochre" => "Ocre",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- categories and caps (#532) ------------------------------------------

    #[test]
    fn a_category_name_is_trimmed() {
        assert_eq!(
            validate_category_name("  Alimentation "),
            Ok("Alimentation")
        );
    }

    #[test]
    fn a_blank_category_name_is_rejected() {
        assert_eq!(
            validate_category_name("   "),
            Err(CategoryError::NameRequired)
        );
        assert_eq!(validate_category_name(""), Err(CategoryError::NameRequired));
    }

    #[test]
    fn a_category_name_holds_forty_characters_not_forty_one() {
        // Characters, not bytes: « é » is two bytes and one character.
        let forty = "é".repeat(40);
        assert_eq!(validate_category_name(&forty), Ok(forty.as_str()));
        let forty_one = "é".repeat(41);
        assert_eq!(
            validate_category_name(&forty_one),
            Err(CategoryError::NameTooLong)
        );
        // The bound applies after trimming.
        let padded = format!("  {forty}  ");
        assert!(validate_category_name(&padded).is_ok());
    }

    #[test]
    fn the_category_colours_are_the_closed_list_of_the_mockup() {
        for colour in ["blue", "violet", "green", "orange", "ochre"] {
            assert!(is_category_color(colour), "{colour}");
        }
        assert_eq!(CATEGORY_COLORS.len(), 5);
        // Named keys only: no hex, no other case.
        assert!(!is_category_color("#2f7a9e"));
        assert!(!is_category_color("Blue"));
        assert!(!is_category_color(""));
    }

    #[test]
    fn a_cap_is_a_positive_number_of_cents() {
        assert_eq!(cap_to_cents(500.0), Ok(50_000));
        assert_eq!(cap_to_cents(12.345), Ok(1_235));
        assert_eq!(cap_to_cents(0.01), Ok(1));
    }

    #[test]
    fn a_zero_negative_or_non_finite_cap_is_rejected() {
        for cap in [0.0, -1.0, 0.004, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                cap_to_cents(cap),
                Err(CategoryError::CapNotPositive),
                "{cap}"
            );
        }
    }

    #[test]
    fn category_error_codes() {
        assert_eq!(CategoryError::NameRequired.code(), "name_required");
        assert_eq!(CategoryError::NameTooLong.code(), "name_too_long");
        assert_eq!(CategoryError::InvalidColor.code(), "invalid_color");
        assert_eq!(CategoryError::CapNotPositive.code(), "cap_must_be_positive");
        assert_eq!(CategoryError::AmountTooLarge.code(), "amount_too_large");
    }

    // -- the upper bound (#648) ----------------------------------------------

    /// One cent above the bound, as the API would receive it.
    const JUST_ABOVE: f64 = AMOUNT_MAX_EUROS + 0.01;

    #[test]
    fn a_cap_of_exactly_the_bound_is_accepted() {
        assert_eq!(cap_to_cents(AMOUNT_MAX_EUROS), Ok(1_000_000_000_000));
        assert_eq!(cap_to_cents(1e10), Ok(1_000_000_000_000));
    }

    #[test]
    fn a_cap_above_the_bound_is_too_large_not_saturated() {
        for cap in [JUST_ABOVE, 1e11, 1e300, f64::MAX] {
            assert_eq!(
                cap_to_cents(cap),
                Err(CategoryError::AmountTooLarge),
                "{cap}"
            );
        }
    }

    #[test]
    fn a_non_finite_cap_is_still_not_positive() {
        for cap in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                cap_to_cents(cap),
                Err(CategoryError::CapNotPositive),
                "{cap}"
            );
        }
    }

    #[test]
    fn a_cap_field_above_the_bound_is_too_large() {
        assert_eq!(
            parse_cap_input("10000000000,01"),
            Err(CategoryError::AmountTooLarge)
        );
        assert_eq!(parse_cap_input("1e300"), Err(CategoryError::AmountTooLarge));
        assert_eq!(parse_cap_input("10000000000"), Ok(Some(1e10)));
    }

    #[test]
    fn an_amount_is_its_cents() {
        assert_eq!(amount_to_cents(0.0), Ok(0));
        assert_eq!(amount_to_cents(12.5), Ok(1_250));
        assert_eq!(amount_to_cents(12.345), Ok(1_235));
        assert_eq!(amount_to_cents(AMOUNT_MAX_EUROS), Ok(1_000_000_000_000));
    }

    #[test]
    fn an_amount_above_the_bound_is_too_large_not_saturated() {
        for amount in [JUST_ABOVE, 1e11, 1e300, f64::MAX] {
            assert_eq!(
                amount_to_cents(amount),
                Err(EntryFormError::AmountTooLarge),
                "{amount}"
            );
        }
    }

    #[test]
    fn a_negative_or_non_finite_amount_is_still_not_non_negative() {
        for amount in [-0.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                amount_to_cents(amount),
                Err(EntryFormError::AmountNegative),
                "{amount}"
            );
        }
    }

    #[test]
    fn the_entry_form_refuses_an_amount_above_the_bound() {
        assert!(validate_entry_form("Courses", AMOUNT_MAX_EUROS).is_ok());
        assert_eq!(
            validate_entry_form("Courses", JUST_ABOVE),
            Err(EntryFormError::AmountTooLarge)
        );
        // The name still comes first.
        assert_eq!(
            validate_entry_form(" ", 1e300),
            Err(EntryFormError::NameRequired)
        );
    }

    #[test]
    fn the_percentage_is_the_share_of_the_cap_rounded_down() {
        assert_eq!(
            cap_usage(41_000, Some(50_000)),
            CapUsage {
                percent: Some(82),
                over_cap: false
            }
        );
        // 99.99 % has not reached the cap yet.
        assert_eq!(cap_usage(9_999, Some(10_000)).percent, Some(99));
        assert_eq!(cap_usage(0, Some(10_000)).percent, Some(0));
    }

    #[test]
    fn reaching_the_cap_is_not_exceeding_it() {
        assert_eq!(
            cap_usage(10_000, Some(10_000)),
            CapUsage {
                percent: Some(100),
                over_cap: false
            }
        );
        assert_eq!(
            cap_usage(10_001, Some(10_000)),
            CapUsage {
                percent: Some(100),
                over_cap: true
            }
        );
        assert_eq!(
            cap_usage(10_400, Some(10_000)),
            CapUsage {
                percent: Some(104),
                over_cap: true
            }
        );
    }

    #[test]
    fn no_cap_or_a_zero_cap_gives_no_percentage_and_no_overrun() {
        let none = CapUsage {
            percent: None,
            over_cap: false,
        };
        assert_eq!(cap_usage(12_345, None), none);
        assert_eq!(cap_usage(12_345, Some(0)), none);
        assert_eq!(cap_usage(0, None), none);
    }

    #[test]
    fn the_percentage_does_not_overflow_on_huge_totals() {
        assert_eq!(
            cap_usage(i64::MAX, Some(1)).percent,
            Some(i64::MAX),
            "saturates rather than wrapping"
        );
        assert!(cap_usage(i64::MAX, Some(1)).over_cap);
    }

    #[test]
    fn a_month_is_read_as_year_dash_month() {
        assert_eq!(
            parse_month("2026-10"),
            Some(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap())
        );
        assert_eq!(
            parse_month("2025-01"),
            Some(NaiveDate::from_ymd_opt(2025, 1, 1).unwrap())
        );
    }

    #[test]
    fn a_malformed_month_is_rejected() {
        for bad in [
            "",
            "2026",
            "2026-13",
            "2026-00",
            "2026-1",
            "2026-10-01",
            "26-10",
            "abcd-ef",
            "2026/10",
        ] {
            assert_eq!(parse_month(bad), None, "{bad:?}");
        }
    }

    // -- validate_entry_form -------------------------------------------------

    #[test]
    fn empty_name_is_rejected() {
        assert_eq!(
            validate_entry_form("   ", 1.0),
            Err(EntryFormError::NameRequired)
        );
        assert_eq!(
            validate_entry_form("", 0.0),
            Err(EntryFormError::NameRequired)
        );
    }

    #[test]
    fn negative_amount_is_rejected() {
        assert_eq!(
            validate_entry_form("Courses", -0.01),
            Err(EntryFormError::AmountNegative)
        );
    }

    #[test]
    fn non_finite_amount_is_rejected() {
        assert_eq!(
            validate_entry_form("Courses", f64::NAN),
            Err(EntryFormError::AmountNegative)
        );
        assert_eq!(
            validate_entry_form("Courses", f64::INFINITY),
            Err(EntryFormError::AmountNegative)
        );
    }

    #[test]
    fn zero_amount_is_accepted() {
        assert!(validate_entry_form("Courses", 0.0).is_ok());
        assert!(validate_entry_form("Courses", 12.5).is_ok());
    }

    #[test]
    fn name_check_precedes_amount_check() {
        // Empty name AND a bad amount: the backend hits `name_required` first
        // (it trims/checks the name before `to_cents`), so we must too.
        assert_eq!(
            validate_entry_form("  ", -1.0),
            Err(EntryFormError::NameRequired)
        );
    }

    #[test]
    fn error_codes_match_the_backend() {
        assert_eq!(EntryFormError::NameRequired.code(), "name_required");
        assert_eq!(
            EntryFormError::AmountNegative.code(),
            "amount_must_be_non_negative"
        );
        assert_eq!(EntryFormError::AmountTooLarge.code(), "amount_too_large");
    }

    // -- can_modify ----------------------------------------------------------

    #[test]
    fn creator_can_modify_regardless_of_role() {
        assert!(can_modify("standard", true));
    }

    #[test]
    fn owner_and_admin_can_modify_others_entries() {
        assert!(can_modify("owner", false));
        assert!(can_modify("admin", false));
    }

    #[test]
    fn standard_member_cannot_modify_another_members_entry() {
        assert!(!can_modify("standard", false));
    }

    // -- can_access_budget ---------------------------------------------------

    #[test]
    fn owner_and_admin_reach_the_budget() {
        assert!(can_access_budget("owner"));
        assert!(can_access_budget("admin"));
    }

    #[test]
    fn a_standard_member_does_not_reach_the_budget() {
        assert!(!can_access_budget("standard"));
    }

    #[test]
    fn an_unknown_role_does_not_reach_the_budget() {
        // Closed by default: a role added later is kept out until it is
        // named here.
        assert!(!can_access_budget(""));
        assert!(!can_access_budget("Owner"));
    }

    // -- format_euros / amount_input_value -----------------------------------

    #[test]
    fn format_euros_uses_two_decimals_and_a_french_comma() {
        assert_eq!(format_euros(12.5), "12,50 €");
        assert_eq!(format_euros(0.0), "0,00 €");
        assert_eq!(format_euros(3.0), "3,00 €");
        assert_eq!(format_euros(1234.5), "1234,50 €");
        // Rounds to the nearest cent (display only — the backend stores cents).
        assert_eq!(format_euros(2.005), "2,00 €");
        assert_eq!(format_euros(2.006), "2,01 €");
    }

    #[test]
    fn amount_input_value_is_a_dot_two_decimal_number() {
        assert_eq!(amount_input_value(12.5), "12.50");
        assert_eq!(amount_input_value(0.0), "0.00");
        assert_eq!(amount_input_value(3.0), "3.00");
        // No comma — must stay a valid `<input type=number>` value.
        assert!(!amount_input_value(9.9).contains(','));
    }

    // -- format_period -------------------------------------------------------

    #[test]
    fn format_period_is_french_month_and_year() {
        assert_eq!(
            format_period(NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()),
            "juillet 2026"
        );
        assert_eq!(
            format_period(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()),
            "janvier 2026"
        );
        assert_eq!(
            format_period(NaiveDate::from_ymd_opt(2025, 12, 1).unwrap()),
            "décembre 2025"
        );
    }

    // -- the budget pages (#533) ----------------------------------------------

    fn first(y: i32, m: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, 1).unwrap()
    }

    #[test]
    fn a_month_is_named_in_the_parameter_the_summary_reads() {
        assert_eq!(month_param(first(2026, 3)), "2026-03");
        assert_eq!(month_param(first(2026, 12)), "2026-12");
        // What it writes, `parse_month` reads back.
        assert_eq!(
            parse_month(&month_param(first(2026, 3))),
            Some(first(2026, 3))
        );
    }

    #[test]
    fn the_months_either_side_cross_the_year() {
        assert_eq!(previous_month(first(2026, 1)), Some(first(2025, 12)));
        assert_eq!(next_month(first(2025, 12)), Some(first(2026, 1)));
        assert_eq!(previous_month(first(2026, 3)), Some(first(2026, 2)));
        assert_eq!(next_month(first(2026, 3)), Some(first(2026, 4)));
    }

    #[test]
    fn a_day_mid_month_still_steps_to_the_first_of_the_next() {
        let mid = NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();
        assert_eq!(next_month(mid), Some(first(2026, 2)));
        assert_eq!(previous_month(mid), Some(first(2025, 12)));
    }

    #[test]
    fn a_day_belongs_to_its_own_month_only() {
        let march = first(2026, 3);
        assert!(in_month(
            NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
            march
        ));
        assert!(in_month(
            NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            march
        ));
        assert!(!in_month(
            NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
            march
        ));
        assert!(!in_month(
            NaiveDate::from_ymd_opt(2025, 3, 15).unwrap(),
            march
        ));
    }

    #[test]
    fn what_is_left_is_counted_in_cents() {
        // 0.1 + 0.2 is not 0.3 in binary: in cents it is.
        assert_eq!(remaining(0.1 + 0.2, 1.0), 0.7);
        // Subtracted in floats, this is 5.55e-17, not zero.
        assert_eq!(remaining(0.1 + 0.2, 0.3), 0.0);
        // 0.29 * 100 is 28.999999999999996: truncated, 72 cents are left.
        assert_eq!(remaining(0.29, 1.0), 0.71);
        assert_eq!(remaining(250.0, 1000.0), 750.0);
        assert_eq!(remaining(1000.0, 1000.0), 0.0);
    }

    #[test]
    fn an_overrun_leaves_a_negative_remainder() {
        assert_eq!(remaining(1012.5, 1000.0), -12.5);
    }

    #[test]
    fn the_share_of_a_cap_reads_percent_then_cap() {
        assert_eq!(cap_share_label(25, 200.0), "25 % de 200,00 €");
        assert_eq!(cap_share_label(104, 50.0), "104 % de 50,00 €");
    }

    #[test]
    fn an_empty_cap_field_is_no_cap() {
        assert_eq!(parse_cap_input(""), Ok(None));
        assert_eq!(parse_cap_input("   "), Ok(None));
    }

    #[test]
    fn a_cap_field_is_euros_with_a_dot_or_a_comma() {
        assert_eq!(parse_cap_input("300"), Ok(Some(300.0)));
        assert_eq!(parse_cap_input(" 12.5 "), Ok(Some(12.5)));
        assert_eq!(parse_cap_input("12,5"), Ok(Some(12.5)));
    }

    #[test]
    fn a_cap_field_that_is_no_positive_amount_is_refused() {
        for bad in ["0", "-3", "abc", "0.004", "NaN", "inf"] {
            assert_eq!(
                parse_cap_input(bad),
                Err(CategoryError::CapNotPositive),
                "{bad}"
            );
        }
    }

    #[test]
    fn every_category_colour_has_a_french_name() {
        let names: Vec<&str> = CATEGORY_COLORS
            .iter()
            .map(|c| category_color_name(c))
            .collect();
        assert_eq!(names, ["Bleu", "Violet", "Vert", "Orange", "Ocre"]);
        assert_eq!(category_color_name("teal"), "");
    }

    #[test]
    fn month_name_covers_every_month() {
        let names = [
            "janvier",
            "février",
            "mars",
            "avril",
            "mai",
            "juin",
            "juillet",
            "août",
            "septembre",
            "octobre",
            "novembre",
            "décembre",
        ];
        for (i, expected) in names.iter().enumerate() {
            assert_eq!(month_name_fr(i as u32 + 1), *expected);
        }
    }
}
