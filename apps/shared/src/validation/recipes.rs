//! Pure, dependency-light logic for the Recipes screens (front epic F5,
//! issue #20), shared by `apps/web`'s SSR pages. Written test-first per
//! `.claude/CLAUDE.md`'s TDD process. Three concerns live here so the UI and
//! the fixed backend (`apps/api/src/recipes/`) can never drift:
//!
//! - `validate_recipe_name` mirrors the backend's create/update `name`
//!   guard (non-empty after trim).
//! - `parse_ingredients` / `format_ingredients` are the inverse pair behind
//!   the one-ingredient-per-line textarea (see the grammar in
//!   `docs/front-epic-5-recipes.md`). The parser mirrors the backend's
//!   `validate_ingredients` check order (name, then quantity, then seasonal
//!   month) so the first error the form surfaces is the one a forged request
//!   would hit; `format_ingredients` is its round-trip inverse, used to
//!   pre-fill the edit form.
//! - `stock_summary` turns a suggestion's matched/total required-ingredient
//!   counts into the human badge the suggestion view shows *instead of* the
//!   raw internal `score` (which the UI never renders).

use crate::dto::recipes::{IngredientInput, IngredientResponse};

/// Why a recipe create/edit form's name was rejected. Mirrors the backend's
/// `name_required` guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipeFormError {
    NameRequired,
}

/// Mirrors `apps/api/src/recipes/crud.rs`: `name` must be non-empty after
/// trimming.
pub fn validate_recipe_name(name: &str) -> Result<(), RecipeFormError> {
    if name.trim().is_empty() {
        Err(RecipeFormError::NameRequired)
    } else {
        Ok(())
    }
}

/// Why an ingredient textarea line was rejected. Carries the 1-based line
/// number so the form can point at the offending row. Ordered/mapped to the
/// backend's `validate_ingredients` error codes; `QuantityInvalid` is
/// front-only (an unparseable number can't be sent as JSON at all).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngredientParseError {
    NameRequired { line: usize },
    QuantityInvalid { line: usize },
    QuantityNegative { line: usize },
    MonthInvalid { line: usize },
}

impl IngredientParseError {
    /// The backend error code this maps to (the front pre-validates, but the
    /// inline message and a defensively-mapped 400 must read identically).
    pub fn code(&self) -> &'static str {
        match self {
            IngredientParseError::NameRequired { .. } => "ingredient_name_required",
            IngredientParseError::QuantityInvalid { .. } => "ingredient_quantity_invalid",
            IngredientParseError::QuantityNegative { .. } => {
                "ingredient_quantity_must_be_non_negative"
            }
            IngredientParseError::MonthInvalid { .. } => "invalid_seasonal_month",
        }
    }
}

/// Recognizes the field-5 "optional" flag (case-insensitive).
fn is_optional_flag(s: &str) -> bool {
    matches!(
        s.trim().to_lowercase().as_str(),
        "optionnel" | "facultatif" | "opt" | "o" | "oui" | "x"
    )
}

/// Parses the field-4 seasonal-months list: comma-separated ints in
/// `1..=12`, tolerating an optional `saison:` prefix and blank entries.
fn parse_months(field: &str, line: usize) -> Result<Vec<i32>, IngredientParseError> {
    let field = field
        .trim()
        .strip_prefix("saison:")
        .or_else(|| field.trim().strip_prefix("saison :"))
        .unwrap_or(field.trim())
        .trim();
    let mut months = Vec::new();
    for part in field.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let m = p
            .parse::<i32>()
            .map_err(|_| IngredientParseError::MonthInvalid { line })?;
        if !(1..=12).contains(&m) {
            return Err(IngredientParseError::MonthInvalid { line });
        }
        months.push(m);
    }
    Ok(months)
}

/// Parses one non-empty ingredient line into an `IngredientInput`. Fields
/// are `name | quantity | unit | months | optionnel`, positional,
/// trailing-optional.
fn parse_ingredient_line(
    line_text: &str,
    line: usize,
) -> Result<IngredientInput, IngredientParseError> {
    let mut fields = line_text.split('|');

    let name = fields.next().unwrap_or("").trim().to_string();
    if name.is_empty() {
        return Err(IngredientParseError::NameRequired { line });
    }

    let quantity = match fields.next().map(str::trim) {
        Some(q) if !q.is_empty() => {
            let v = q
                .parse::<f64>()
                .map_err(|_| IngredientParseError::QuantityInvalid { line })?;
            if v < 0.0 {
                return Err(IngredientParseError::QuantityNegative { line });
            }
            Some(v)
        }
        _ => None,
    };

    let unit = fields
        .next()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_string);

    let seasonal_months = match fields.next().map(str::trim) {
        Some(m) if !m.is_empty() => {
            let parsed = parse_months(m, line)?;
            (!parsed.is_empty()).then_some(parsed)
        }
        _ => None,
    };

    let is_optional = fields.next().map(is_optional_flag).unwrap_or(false);

    Ok(IngredientInput {
        name,
        quantity,
        unit,
        is_optional,
        seasonal_months,
    })
}

/// Parses the ingredient textarea (one ingredient per line, blank lines
/// skipped) into the list sent to the backend. Fails on the first offending
/// line, mirroring the backend's check order.
pub fn parse_ingredients(input: &str) -> Result<Vec<IngredientInput>, IngredientParseError> {
    let mut out = Vec::new();
    for (idx, raw) in input.lines().enumerate() {
        let line_text = raw.trim();
        if line_text.is_empty() {
            continue;
        }
        out.push(parse_ingredient_line(line_text, idx + 1)?);
    }
    Ok(out)
}

/// Formats an `f64` without a trailing `.0` on whole numbers (mirrors the
/// web-side `fmt_num`, kept here so the textarea round-trips wasm-clean).
fn fmt_qty(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// One response ingredient → its textarea line, the inverse of
/// `parse_ingredient_line`. Trailing empty fields are dropped so a simple
/// ingredient stays a bare name.
fn format_ingredient_line(ing: &IngredientResponse) -> String {
    let qty = ing.quantity.map(fmt_qty).unwrap_or_default();
    let unit = ing.unit.clone().unwrap_or_default();
    let months = ing
        .seasonal_months
        .as_ref()
        .map(|m| m.iter().map(i32::to_string).collect::<Vec<_>>().join(","))
        .unwrap_or_default();
    let optional = if ing.is_optional { "optionnel" } else { "" };

    let tail = [qty, unit, months, optional.to_string()];
    match tail.iter().rposition(|f| !f.is_empty()) {
        None => ing.name.clone(),
        Some(last) => format!("{} | {}", ing.name, tail[..=last].join(" | ")),
    }
}

/// Response ingredients → the textarea value pre-filling the edit form.
/// `parse_ingredients(&format_ingredients(x))` round-trips back to the same
/// inputs.
pub fn format_ingredients(ingredients: &[IngredientResponse]) -> String {
    ingredients
        .iter()
        .map(format_ingredient_line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Human summary of a suggestion's stock match, shown instead of the raw
/// `score`. Mirrors the backend's required-ingredient counting.
pub fn stock_summary(matched: usize, total_required: usize) -> String {
    if total_required == 0 {
        "Aucun ingrédient requis".to_string()
    } else if matched >= total_required {
        "Tous les ingrédients en stock".to_string()
    } else {
        format!("{matched}/{total_required} ingrédients en stock")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    // -- validate_recipe_name ------------------------------------------------

    #[test]
    fn empty_name_is_rejected() {
        assert_eq!(
            validate_recipe_name("   "),
            Err(RecipeFormError::NameRequired)
        );
    }

    #[test]
    fn non_empty_name_is_accepted() {
        assert!(validate_recipe_name("Tarte aux pommes").is_ok());
    }

    // -- parse_ingredients ---------------------------------------------------

    #[test]
    fn bare_name_line_parses_to_name_only() {
        let out = parse_ingredients("Farine").unwrap();
        assert_eq!(
            out,
            vec![IngredientInput {
                name: "Farine".to_string(),
                quantity: None,
                unit: None,
                is_optional: false,
                seasonal_months: None,
            }]
        );
    }

    #[test]
    fn full_line_parses_all_fields() {
        let out = parse_ingredients("Tomate | 4 | pièce | 6,7,8 | optionnel").unwrap();
        assert_eq!(
            out,
            vec![IngredientInput {
                name: "Tomate".to_string(),
                quantity: Some(4.0),
                unit: Some("pièce".to_string()),
                is_optional: true,
                seasonal_months: Some(vec![6, 7, 8]),
            }]
        );
    }

    #[test]
    fn blank_lines_are_skipped_and_lines_are_trimmed() {
        let out = parse_ingredients("  Farine | 2 | kg  \n\n   \nSucre").unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "Farine");
        assert_eq!(out[0].quantity, Some(2.0));
        assert_eq!(out[0].unit, Some("kg".to_string()));
        assert_eq!(out[1].name, "Sucre");
    }

    #[test]
    fn empty_name_field_on_a_non_blank_line_is_rejected() {
        // A line that is only a pipe has an empty name — the backend hits
        // `ingredient_name_required`, so we must too (and on line 2).
        let err = parse_ingredients("Farine\n | 2 | kg").unwrap_err();
        assert_eq!(err, IngredientParseError::NameRequired { line: 2 });
        assert_eq!(err.code(), "ingredient_name_required");
    }

    #[test]
    fn negative_quantity_is_rejected() {
        let err = parse_ingredients("Farine | -1 | kg").unwrap_err();
        assert_eq!(err, IngredientParseError::QuantityNegative { line: 1 });
        assert_eq!(err.code(), "ingredient_quantity_must_be_non_negative");
    }

    #[test]
    fn unparseable_quantity_is_rejected() {
        let err = parse_ingredients("Farine | beaucoup | kg").unwrap_err();
        assert_eq!(err, IngredientParseError::QuantityInvalid { line: 1 });
    }

    #[test]
    fn out_of_range_month_is_rejected() {
        let err = parse_ingredients("Courgette | 2 | pièce | 6,13").unwrap_err();
        assert_eq!(err, IngredientParseError::MonthInvalid { line: 1 });
        assert_eq!(err.code(), "invalid_seasonal_month");
    }

    #[test]
    fn name_check_precedes_quantity_check() {
        // Empty name AND a bad quantity on the same line: name wins, matching
        // the backend's check order.
        let err = parse_ingredients(" | -1").unwrap_err();
        assert_eq!(err, IngredientParseError::NameRequired { line: 1 });
    }

    #[test]
    fn saison_prefix_and_spacing_are_tolerated_on_months() {
        let out = parse_ingredients("Potiron | 1 | pièce | saison: 9, 10 ,11").unwrap();
        assert_eq!(out[0].seasonal_months, Some(vec![9, 10, 11]));
    }

    #[test]
    fn various_optional_flags_are_recognized() {
        for flag in ["optionnel", "OPT", "Facultatif", "x", "oui"] {
            let out = parse_ingredients(&format!("Basilic | | | | {flag}")).unwrap();
            assert!(out[0].is_optional, "flag {flag} should mark optional");
        }
        let required = parse_ingredients("Basilic | | | | non").unwrap();
        assert!(!required[0].is_optional);
    }

    // -- format_ingredients (round-trip inverse) -----------------------------

    fn resp(
        name: &str,
        quantity: Option<f64>,
        unit: Option<&str>,
        is_optional: bool,
        seasonal_months: Option<Vec<i32>>,
    ) -> IngredientResponse {
        IngredientResponse {
            id: Uuid::nil(),
            name: name.to_string(),
            quantity,
            unit: unit.map(str::to_string),
            is_optional,
            seasonal_months,
        }
    }

    #[test]
    fn format_drops_trailing_empty_fields() {
        assert_eq!(
            format_ingredients(&[resp("Farine", None, None, false, None)]),
            "Farine"
        );
        assert_eq!(
            format_ingredients(&[resp("Farine", Some(2.0), Some("kg"), false, None)]),
            "Farine | 2 | kg"
        );
    }

    #[test]
    fn format_keeps_gaps_before_a_later_field() {
        // Optional with no quantity/unit/months keeps the empty middle fields
        // so the positional parse still lands `optionnel` in field 5.
        assert_eq!(
            format_ingredients(&[resp("Basilic", None, None, true, None)]),
            "Basilic |  |  |  | optionnel"
        );
    }

    #[test]
    fn parse_then_format_round_trips() {
        let ings = vec![
            resp("Farine", Some(2.0), Some("kg"), false, None),
            resp(
                "Tomate",
                Some(4.0),
                Some("pièce"),
                true,
                Some(vec![6, 7, 8]),
            ),
            resp("Basilic", None, None, true, None),
            resp("Sel", None, Some("pincée"), false, None),
        ];
        let text = format_ingredients(&ings);
        let reparsed = parse_ingredients(&text).unwrap();
        let expected: Vec<IngredientInput> = ings
            .iter()
            .map(|i| IngredientInput {
                name: i.name.clone(),
                quantity: i.quantity,
                unit: i.unit.clone(),
                is_optional: i.is_optional,
                seasonal_months: i.seasonal_months.clone(),
            })
            .collect();
        assert_eq!(reparsed, expected);
    }

    // -- stock_summary -------------------------------------------------------

    #[test]
    fn stock_summary_covers_the_three_cases() {
        assert_eq!(stock_summary(0, 0), "Aucun ingrédient requis");
        assert_eq!(stock_summary(3, 3), "Tous les ingrédients en stock");
        assert_eq!(stock_summary(2, 4), "2/4 ingrédients en stock");
    }
}
