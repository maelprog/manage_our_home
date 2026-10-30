//! Pure, dependency-light logic for the Recipes screens (front epic F5,
//! issue #20), shared by `apps/web`'s SSR pages. Written test-first per
//! `.claude/CLAUDE.md`'s TDD process. Three concerns live here so the UI and
//! the fixed backend (`apps/api/src/recipes/`) can never drift:
//!
//! - `validate_recipe_name` mirrors the backend's create/update `name`
//!   guard (non-empty after trim).
//! - `parse_ingredient_fields` / `ingredient_fields` are the inverse pair
//!   behind the form's ingredient lines (#539: quantity, unit, name,
//!   seasonal months, « Facultatif »). The parser mirrors the backend's
//!   `validate_ingredients` check order (name, then quantity, then seasonal
//!   month) so the first error the form surfaces is the one a forged request
//!   would hit; `ingredient_fields` pre-fills the edit form. `steps_of` /
//!   `join_steps` do the same for the numbered steps.
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

/// Why an ingredient line of the form was rejected. Carries the 1-based line
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

/// Parses a line's seasonal-months field: comma-separated ints in `1..=12`,
/// blank entries tolerated.
fn parse_months(field: &str, line: usize) -> Result<Vec<i32>, IngredientParseError> {
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

/// One ingredient line of the recipe form (#539), as typed: quantity,
/// unit, name, seasonal months (comma-separated numbers) and the
/// « Facultatif » box.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IngredientFields {
    pub quantity: String,
    pub unit: String,
    pub name: String,
    pub months: String,
    pub optional: bool,
}

impl IngredientFields {
    /// Nothing typed in any text field: an unused line of the form.
    pub fn is_blank(&self) -> bool {
        [&self.quantity, &self.unit, &self.name, &self.months]
            .iter()
            .all(|f| f.trim().is_empty())
    }
}

/// One non-blank line → the ingredient sent to the backend, checked in the
/// backend's order (name, quantity, seasonal month). `line` is 1-based.
fn parse_line(f: &IngredientFields, line: usize) -> Result<IngredientInput, IngredientParseError> {
    let name = f.name.trim();
    if name.is_empty() {
        return Err(IngredientParseError::NameRequired { line });
    }
    let quantity = match f.quantity.trim() {
        "" => None,
        q => {
            let v = q
                .replace(',', ".")
                .parse::<f64>()
                .map_err(|_| IngredientParseError::QuantityInvalid { line })?;
            if v < 0.0 {
                return Err(IngredientParseError::QuantityNegative { line });
            }
            Some(v)
        }
    };
    let unit = Some(f.unit.trim())
        .filter(|u| !u.is_empty())
        .map(str::to_string);
    let months = parse_months(&f.months, line)?;
    Ok(IngredientInput {
        name: name.to_string(),
        quantity,
        unit,
        is_optional: f.optional,
        seasonal_months: (!months.is_empty()).then_some(months),
    })
}

/// The form's ingredient lines → the list sent to the backend. Blank lines
/// are left out; the first line in error fails it, with its number.
pub fn parse_ingredient_fields(
    lines: &[IngredientFields],
) -> Result<Vec<IngredientInput>, IngredientParseError> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.is_blank())
        .map(|(i, f)| parse_line(f, i + 1))
        .collect()
}

/// Formats an `f64` without a trailing `.0` on whole numbers (mirrors the
/// web-side `fmt_num`).
fn fmt_qty(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Stored ingredients → the lines pre-filling the edit form, seasonal
/// months included. [`parse_ingredient_fields`] reads them back unchanged.
pub fn ingredient_fields(ingredients: &[IngredientResponse]) -> Vec<IngredientFields> {
    ingredients
        .iter()
        .map(|i| IngredientFields {
            quantity: i.quantity.map(fmt_qty).unwrap_or_default(),
            unit: i.unit.clone().unwrap_or_default(),
            name: i.name.clone(),
            months: i
                .seasonal_months
                .as_ref()
                .map(|m| m.iter().map(i32::to_string).collect::<Vec<_>>().join(","))
                .unwrap_or_default(),
            optional: i.is_optional,
        })
        .collect()
}

/// The numbered steps of a recipe: the lines of its instructions, trimmed,
/// blank ones left out. A line's own number (« 1. », « 2) ») goes, the
/// steps being numbered where they are shown: a recipe imported before
/// #539 was stored that way.
pub fn steps_of(instructions: &str) -> Vec<String> {
    instructions
        .lines()
        .map(|l| without_step_number(l.trim()))
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// `line` without a leading « N. » or « N) » followed by a space (or
/// nothing else): « 12.5 g » and « 2 œufs » keep their number.
fn without_step_number(line: &str) -> &str {
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return line;
    }
    let rest = &line[digits..];
    match rest.strip_prefix(['.', ')']) {
        Some(after) if after.is_empty() || after.starts_with(char::is_whitespace) => {
            after.trim_start()
        }
        _ => line,
    }
}

/// The form's steps → the instructions stored, one step per line. A line
/// break typed inside a step becomes a space, so that the step reads back
/// as one ([`steps_of`]); blank steps are left out, and none at all is
/// `None`.
pub fn join_steps(steps: &[String]) -> Option<String> {
    let lines: Vec<String> = steps
        .iter()
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty())
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
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

/// The most people a recipe or a selection may be counted for. Mirrors
/// `apps/api/src/recipes/selections.rs::SERVINGS_MAX` and the `CHECK` of
/// `recipes.servings` (0034) and `recipe_selections.servings` (0033).
pub const SERVINGS_MAX: i32 = 50;

/// The "Personnes" field of the recipe form and of the
/// « À réaliser » form (#470): blank → `None` (not said), a whole number
/// from 1 to [`SERVINGS_MAX`] → `Some`, anything else → `Err` carrying the
/// backend's `invalid_servings` code.
pub fn parse_servings(input: &str) -> Result<Option<i32>, &'static str> {
    let t = input.trim();
    if t.is_empty() {
        return Ok(None);
    }
    match t.parse::<i32>() {
        Ok(n) if (1..=SERVINGS_MAX).contains(&n) => Ok(Some(n)),
        _ => Err("invalid_servings"),
    }
}

/// "1 personne", "4 personnes".
pub fn servings_label(servings: i32) -> String {
    if servings == 1 {
        "1 personne".to_string()
    } else {
        format!("{servings} personnes")
    }
}

/// The longest a recipe may say it takes, in minutes: a day. Mirrors the
/// `CHECK` of `recipes.duration_minutes` (0041) and
/// `apps/api/src/recipes/crud.rs::DURATION_MAX`.
pub const DURATION_MAX: i32 = 1440;

/// The « Temps » field of the recipe form (#539), in minutes: blank →
/// `None` (not said), a whole number from 1 to [`DURATION_MAX`] → `Some`,
/// anything else → `Err` carrying the backend's `invalid_duration` code.
pub fn parse_duration(input: &str) -> Result<Option<i32>, &'static str> {
    let t = input.trim();
    if t.is_empty() {
        return Ok(None);
    }
    match t.parse::<i32>() {
        Ok(n) if (1..=DURATION_MAX).contains(&n) => Ok(Some(n)),
        _ => Err("invalid_duration"),
    }
}

/// "45 min", "1 h", "1 h 30", "2 h 05".
pub fn duration_label(minutes: i32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m:02}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    // -- parse_servings ------------------------------------------------------

    #[test]
    fn blank_servings_are_not_said() {
        assert_eq!(parse_servings(""), Ok(None));
        assert_eq!(parse_servings("   "), Ok(None));
    }

    #[test]
    fn servings_from_one_to_the_maximum_are_accepted() {
        assert_eq!(parse_servings("1"), Ok(Some(1)));
        assert_eq!(parse_servings(" 4 "), Ok(Some(4)));
        assert_eq!(parse_servings("50"), Ok(Some(SERVINGS_MAX)));
    }

    #[test]
    fn servings_out_of_bounds_or_not_whole_are_refused() {
        for input in ["0", "-2", "51", "2.5", "quatre", "4 personnes"] {
            assert_eq!(parse_servings(input), Err("invalid_servings"), "{input}");
        }
    }

    #[test]
    fn servings_label_agrees_in_number() {
        assert_eq!(servings_label(1), "1 personne");
        assert_eq!(servings_label(4), "4 personnes");
    }

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

    // -- parse_ingredient_fields ---------------------------------------------

    fn fields(
        quantity: &str,
        unit: &str,
        name: &str,
        months: &str,
        optional: bool,
    ) -> IngredientFields {
        IngredientFields {
            quantity: quantity.into(),
            unit: unit.into(),
            name: name.into(),
            months: months.into(),
            optional,
        }
    }

    #[test]
    fn a_line_with_a_name_only_is_that_ingredient() {
        let out = parse_ingredient_fields(&[fields("", "", "Farine", "", false)]).unwrap();
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
    fn a_full_line_reads_every_field_trimmed() {
        let out = parse_ingredient_fields(&[fields(" 4 ", " pièce ", " Tomate ", "6, 7 ,8", true)])
            .unwrap();
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
    fn a_decimal_comma_is_a_decimal_point() {
        let out = parse_ingredient_fields(&[fields("0,5", "L", "Lait", "", false)]).unwrap();
        assert_eq!(out[0].quantity, Some(0.5));
    }

    #[test]
    fn blank_lines_are_left_out_even_ticked_facultatif() {
        let out = parse_ingredient_fields(&[
            fields("", "", "", "", false),
            fields("2", "kg", "Farine", "", false),
            fields(" ", " ", " ", " ", true),
            fields("", "", "Sucre", "", false),
        ])
        .unwrap();
        let names: Vec<_> = out.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["Farine", "Sucre"]);
    }

    #[test]
    fn a_line_without_a_name_is_refused_with_its_number() {
        // Line 2 has a quantity and no name: the backend's
        // `ingredient_name_required`, on that line.
        let err = parse_ingredient_fields(&[
            fields("", "", "Farine", "", false),
            fields("2", "kg", " ", "", false),
        ])
        .unwrap_err();
        assert_eq!(err, IngredientParseError::NameRequired { line: 2 });
        assert_eq!(err.code(), "ingredient_name_required");
    }

    #[test]
    fn a_negative_or_unreadable_quantity_is_refused() {
        let err = parse_ingredient_fields(&[fields("-1", "kg", "Farine", "", false)]).unwrap_err();
        assert_eq!(err, IngredientParseError::QuantityNegative { line: 1 });
        assert_eq!(err.code(), "ingredient_quantity_must_be_non_negative");
        let err =
            parse_ingredient_fields(&[fields("beaucoup", "", "Farine", "", false)]).unwrap_err();
        assert_eq!(err, IngredientParseError::QuantityInvalid { line: 1 });
    }

    #[test]
    fn a_month_out_of_range_is_refused() {
        let err =
            parse_ingredient_fields(&[fields("2", "", "Courgette", "6,13", false)]).unwrap_err();
        assert_eq!(err, IngredientParseError::MonthInvalid { line: 1 });
        assert_eq!(err.code(), "invalid_seasonal_month");
    }

    #[test]
    fn the_name_is_checked_before_the_quantity() {
        let err = parse_ingredient_fields(&[fields("-1", "", "", "", false)]).unwrap_err();
        assert_eq!(err, IngredientParseError::NameRequired { line: 1 });
    }

    // -- ingredient_fields (the edit form's lines) ---------------------------

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
    fn a_stored_ingredient_fills_its_line() {
        assert_eq!(
            ingredient_fields(&[
                resp(
                    "Tomate",
                    Some(4.0),
                    Some("pièce"),
                    true,
                    Some(vec![6, 7, 8])
                ),
                resp("Lait", Some(0.5), None, false, None),
            ]),
            vec![
                fields("4", "pièce", "Tomate", "6,7,8", true),
                fields("0.5", "", "Lait", "", false),
            ]
        );
    }

    #[test]
    fn the_lines_round_trip_seasonal_months_included() {
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
        let reparsed = parse_ingredient_fields(&ingredient_fields(&ings)).unwrap();
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

    // -- steps ---------------------------------------------------------------

    #[test]
    fn the_steps_are_the_lines_of_the_instructions() {
        assert_eq!(
            steps_of("Mélanger.\n\n  Cuire 10 min.  \r\nServir."),
            ["Mélanger.", "Cuire 10 min.", "Servir."]
        );
        assert!(steps_of("").is_empty());
        assert!(steps_of(" \n ").is_empty());
    }

    #[test]
    fn a_step_numbered_by_hand_or_by_an_old_import_loses_its_number() {
        // Before #539 an imported recipe was stored as « 1. Mélanger. »:
        // the list numbers its steps itself.
        assert_eq!(
            steps_of("1. Mélanger.\n2) Cuire 10 min.\n10.  Servir.\n3."),
            ["Mélanger.", "Cuire 10 min.", "Servir."]
        );
        // A number that is not a step's is kept.
        assert_eq!(
            steps_of("12.5 g de sucre.\n2 œufs.\n1.Mélanger."),
            ["12.5 g de sucre.", "2 œufs.", "1.Mélanger."]
        );
    }

    #[test]
    fn the_steps_are_stored_one_per_line_blanks_left_out() {
        let steps: Vec<String> = ["Mélanger.", "  ", "Cuire\r\n10 min. ", ""]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // A line break typed inside a step stays in that step.
        assert_eq!(
            join_steps(&steps),
            Some("Mélanger.\nCuire 10 min.".to_string())
        );
        assert_eq!(join_steps(&["  ".to_string()]), None);
        assert_eq!(join_steps(&[]), None);
        // And they read back as typed.
        assert_eq!(
            steps_of(&join_steps(&steps).unwrap()),
            ["Mélanger.", "Cuire 10 min."]
        );
    }

    // -- parse_duration ------------------------------------------------------

    #[test]
    fn blank_duration_is_not_said() {
        assert_eq!(parse_duration(""), Ok(None));
        assert_eq!(parse_duration("  "), Ok(None));
    }

    #[test]
    fn duration_in_whole_minutes_up_to_the_maximum_is_accepted() {
        assert_eq!(parse_duration("1"), Ok(Some(1)));
        assert_eq!(parse_duration(" 45 "), Ok(Some(45)));
        assert_eq!(parse_duration("1440"), Ok(Some(DURATION_MAX)));
    }

    #[test]
    fn duration_out_of_bounds_or_not_whole_is_refused() {
        for input in ["0", "-5", "1441", "1.5", "une heure", "45 min"] {
            assert_eq!(parse_duration(input), Err("invalid_duration"), "{input}");
        }
    }

    #[test]
    fn duration_label_reads_in_hours_and_minutes() {
        assert_eq!(duration_label(45), "45 min");
        assert_eq!(duration_label(60), "1 h");
        assert_eq!(duration_label(90), "1 h 30");
        assert_eq!(duration_label(125), "2 h 05");
    }

    // -- stock_summary -------------------------------------------------------

    #[test]
    fn stock_summary_covers_the_three_cases() {
        assert_eq!(stock_summary(0, 0), "Aucun ingrédient requis");
        assert_eq!(stock_summary(3, 3), "Tous les ingrédients en stock");
        assert_eq!(stock_summary(2, 4), "2/4 ingrédients en stock");
    }
}
