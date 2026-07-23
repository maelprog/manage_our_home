//! Pure, dependency-light logic for the Grocery-list screens (front epic F6,
//! issue #21), shared by `apps/web`'s SSR pages. Written test-first per
//! `.claude/CLAUDE.md`'s TDD process. Four concerns live here so the UI and
//! the fixed backend (`apps/api/src/grocery_list/`) can never drift:
//!
//! - `validate_item_form` mirrors the backend's create/update guards
//!   (non-empty `name` after trim, non-negative `quantity`) in the same order
//!   the backend checks them, so the first error the form surfaces is the one
//!   a forged request would hit.
//! - `can_modify` mirrors `apps/api/src/grocery_list/mod.rs::can_modify`: the
//!   item's creator, or a group owner/admin, may edit or delete it. Any member
//!   may create/read/check off (that lighter bar is *not* gated here).
//! - `fmt_num` / `quantity_label` format the optional quantity/unit for
//!   display and form pre-fill (no trailing `.0` on whole numbers).
//! - `source_label` turns the informational `source` (`manual`/`recipe`/
//!   `low_stock`) into the French provenance badge the list shows on
//!   auto-generated rows.

/// Why a grocery-item add/edit form was rejected. Ordered to match the
/// backend's check order (`create_grocery_item` / `update_grocery_item`):
/// name first, then quantity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemFormError {
    NameRequired,
    QuantityNegative,
}

impl ItemFormError {
    /// The backend error code this maps to (the front pre-validates, but the
    /// inline message and a defensively-mapped 400 must read identically).
    pub fn code(&self) -> &'static str {
        match self {
            ItemFormError::NameRequired => "name_required",
            ItemFormError::QuantityNegative => "quantity_must_be_non_negative",
        }
    }
}

/// Mirrors `apps/api/src/grocery_list/items.rs`: `name` must be non-empty
/// after trimming, and a present `quantity` must be `>= 0`. Kept in the same
/// order the backend validates (name, then quantity) so the first error
/// surfaced matches what a forged request would hit. `unit` is free optional
/// text — the backend imposes no rule on it, so neither do we.
pub fn validate_item_form(name: &str, quantity: Option<f64>) -> Result<(), ItemFormError> {
    if name.trim().is_empty() {
        return Err(ItemFormError::NameRequired);
    }
    if let Some(q) = quantity {
        if q < 0.0 {
            return Err(ItemFormError::QuantityNegative);
        }
    }
    Ok(())
}

/// Mirror of `apps/api/src/grocery_list/mod.rs::can_modify`: the item's
/// creator, or a group owner/admin, may edit or delete it. Used to decide
/// whether to render the edit/delete controls (the backend still 403s a
/// forged request). Create/read/check-off are *not* gated by this — they're
/// open to any member.
pub fn can_modify(role: &str, is_creator: bool) -> bool {
    is_creator || role == "owner" || role == "admin"
}

/// Formats an `f64` quantity for display and form pre-fill without a trailing
/// `.0` on whole numbers (`2.0` → `"2"`, `0.5` → `"0.5"`). Mirrors the twin in
/// `validation::recipes` / `routes::stocks`.
pub fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// The human quantity+unit label shown next to an item name: `"2 kg"`,
/// `"2"` (no unit), `"kg"` (no quantity), or `""` (neither). Both fields are
/// optional on a grocery item, so every combination must render sensibly.
pub fn quantity_label(quantity: Option<f64>, unit: Option<&str>) -> String {
    match (quantity, unit.map(str::trim).filter(|u| !u.is_empty())) {
        (Some(q), Some(u)) => format!("{} {}", fmt_num(q), u),
        (Some(q), None) => fmt_num(q),
        (None, Some(u)) => u.to_string(),
        (None, None) => String::new(),
    }
}

/// French provenance badge for a generated row's `source`. `manual` rows get
/// no badge (`None`); `recipe`/`low_stock` rows show where they came from.
pub fn source_label(source: &str) -> Option<&'static str> {
    match source {
        "recipe" => Some("Recette"),
        "low_stock" => Some("Stock bas"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- validate_item_form --------------------------------------------------

    #[test]
    fn empty_name_is_rejected() {
        assert_eq!(
            validate_item_form("   ", Some(1.0)),
            Err(ItemFormError::NameRequired)
        );
        assert_eq!(
            validate_item_form("", None),
            Err(ItemFormError::NameRequired)
        );
    }

    #[test]
    fn negative_quantity_is_rejected() {
        assert_eq!(
            validate_item_form("Lait", Some(-0.1)),
            Err(ItemFormError::QuantityNegative)
        );
    }

    #[test]
    fn zero_quantity_and_absent_quantity_are_accepted() {
        assert!(validate_item_form("Lait", Some(0.0)).is_ok());
        assert!(validate_item_form("Lait", None).is_ok());
    }

    #[test]
    fn name_check_precedes_quantity_check() {
        // Empty name AND a bad quantity: the backend hits `name_required`
        // first, so we must too.
        assert_eq!(
            validate_item_form("  ", Some(-1.0)),
            Err(ItemFormError::NameRequired)
        );
    }

    #[test]
    fn error_codes_match_the_backend() {
        assert_eq!(ItemFormError::NameRequired.code(), "name_required");
        assert_eq!(
            ItemFormError::QuantityNegative.code(),
            "quantity_must_be_non_negative"
        );
    }

    // -- can_modify ----------------------------------------------------------

    #[test]
    fn creator_can_modify_regardless_of_role() {
        assert!(can_modify("standard", true));
    }

    #[test]
    fn owner_and_admin_can_modify_others_items() {
        assert!(can_modify("owner", false));
        assert!(can_modify("admin", false));
    }

    #[test]
    fn standard_member_cannot_modify_another_members_item() {
        assert!(!can_modify("standard", false));
    }

    // -- fmt_num / quantity_label -------------------------------------------

    #[test]
    fn fmt_num_trims_whole_numbers_only() {
        assert_eq!(fmt_num(2.0), "2");
        assert_eq!(fmt_num(0.5), "0.5");
        assert_eq!(fmt_num(0.0), "0");
    }

    #[test]
    fn quantity_label_covers_every_combination() {
        assert_eq!(quantity_label(Some(2.0), Some("kg")), "2 kg");
        assert_eq!(quantity_label(Some(2.0), None), "2");
        assert_eq!(quantity_label(None, Some("kg")), "kg");
        assert_eq!(quantity_label(None, None), "");
        assert_eq!(quantity_label(Some(0.5), Some("L")), "0.5 L");
        // A blank/whitespace unit is treated as absent.
        assert_eq!(quantity_label(Some(3.0), Some("  ")), "3");
    }

    // -- source_label --------------------------------------------------------

    #[test]
    fn source_label_badges_only_generated_rows() {
        assert_eq!(source_label("recipe"), Some("Recette"));
        assert_eq!(source_label("low_stock"), Some("Stock bas"));
        assert_eq!(source_label("manual"), None);
        assert_eq!(source_label("anything_else"), None);
    }
}
