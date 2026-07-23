//! Pure, dependency-light logic for the Budget screens (front epic F7, issue
//! #22), shared by `apps/web`'s SSR pages. Written test-first per
//! `.claude/CLAUDE.md`'s TDD process. Everything money- or permission-shaped
//! lives here so the UI and the fixed backend (`apps/api/src/budget/`) can
//! never drift:
//!
//! - `validate_entry_form` mirrors the backend's create/update guards
//!   (non-empty `name` after trim, a finite non-negative `amount`) in the same
//!   order the backend checks them (name, then amount), so the first error the
//!   form surfaces is the one a forged request would hit.
//! - `can_modify` mirrors `apps/api/src/budget/mod.rs::can_modify`: the entry's
//!   creator, or a group owner/admin, may edit or delete it. Any member may
//!   create/read/set a price (that lighter bar is *not* gated here).
//! - `format_euros` / `amount_input_value` format the euro `f64` the API
//!   returns for display (two decimals, French comma + `€`) and for
//!   number-input pre-fill (two decimals, dot). The backend stores cents; the
//!   front only ever displays/parses euros, so rounding lives in one tested
//!   place.
//! - `format_period` turns a summary period (first-of-month `NaiveDate`) into
//!   the French month-and-year label the monthly summary shows.

use chrono::{Datelike, NaiveDate};

/// Why a budget-entry add/edit form was rejected. Ordered to match the
/// backend's check order (`create_budget_entry` / `update_budget_entry`): name
/// first, then amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryFormError {
    NameRequired,
    AmountNegative,
}

impl EntryFormError {
    /// The backend error code this maps to (the front pre-validates, but the
    /// inline message and a defensively-mapped 400 must read identically).
    pub fn code(&self) -> &'static str {
        match self {
            EntryFormError::NameRequired => "name_required",
            EntryFormError::AmountNegative => "amount_must_be_non_negative",
        }
    }
}

/// Mirrors `apps/api/src/budget/entries.rs`: `name` must be non-empty after
/// trimming, and `amount` must be a finite value `>= 0` (the backend's
/// `to_cents` rejects non-finite or negative amounts as
/// `amount_must_be_non_negative`). Kept in the same order the backend
/// validates (name, then amount) so the first error surfaced matches what a
/// forged request would hit.
pub fn validate_entry_form(name: &str, amount: f64) -> Result<(), EntryFormError> {
    if name.trim().is_empty() {
        return Err(EntryFormError::NameRequired);
    }
    if !amount.is_finite() || amount < 0.0 {
        return Err(EntryFormError::AmountNegative);
    }
    Ok(())
}

/// Mirror of `apps/api/src/budget/mod.rs::can_modify`: the entry's creator, or
/// a group owner/admin, may edit or delete it. Used to decide whether to render
/// the edit/delete controls (the backend still 403s a forged request).
/// Create/read/set-price are *not* gated by this — they're open to any member.
pub fn can_modify(role: &str, is_creator: bool) -> bool {
    is_creator || role == "owner" || role == "admin"
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

#[cfg(test)]
mod tests {
    use super::*;

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
