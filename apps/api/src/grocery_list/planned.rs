//! The grocery lines the planned recipes call for (#470): the pure part of
//! `items::generate_grocery_items`.
//!
//! Only the recipes with a `planned` selection (`recipe_selections`, #469)
//! count. Each quantity is scaled to the selection's number of people
//! against the recipe's base (`recipes.servings`, 0034); without either
//! one it is taken as written. An ingredient named the same in several
//! planned recipes makes one line, attributed to the first recipe: the
//! quantities add up when their units are the same, and otherwise the first
//! one stands (decisions of 2026-10-24).

use manage_our_home_shared::validation::grocery_list::normalize_unit;
use uuid::Uuid;

/// One required ingredient of a planned recipe, with the two numbers of
/// people its quantity is scaled between.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlannedIngredient {
    pub recipe_id: Uuid,
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    /// The number of people the recipe is written for (`recipes.servings`).
    pub recipe_servings: Option<i32>,
    /// The number of people it is cooked for this time
    /// (`recipe_selections.servings`).
    pub selection_servings: Option<i32>,
}

/// One grocery line to create, from the first planned recipe that calls
/// for it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlannedLine {
    pub recipe_id: Uuid,
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
}

/// Drops the binary noise of a computed quantity, without rounding it
/// (decision of 2026-10-25): `0.1 + 0.2` reads `0.3`, not
/// `0.30000000000000004`, while `0.125 + 0.0625` keeps its `0.1875` and
/// `200 × 5 / 3` reads `333.333333333`. It keeps twelve significant digits:
/// nothing bounds the digits of a quantity (`DOUBLE PRECISION`), so one
/// typed with more loses its last ones (`1.23456789012345` reads
/// `1.23456789012`), but a non-zero quantity never becomes 0.
pub(crate) fn without_binary_noise(q: f64) -> f64 {
    format!("{q:.11e}").parse().unwrap_or(q)
}

/// A sum of quantities, without its binary noise.
fn add_quantities(a: f64, b: f64) -> f64 {
    without_binary_noise(a + b)
}

/// `quantity × target / base`, not rounded, only without its binary noise;
/// the quantity as written when it, the base or the target is missing.
pub(crate) fn scale_quantity(
    quantity: Option<f64>,
    base: Option<i32>,
    target: Option<i32>,
) -> Option<f64> {
    match (quantity, base, target) {
        (Some(q), Some(b), Some(t)) if b > 0 => {
            Some(without_binary_noise(q * f64::from(t) / f64::from(b)))
        }
        _ => quantity,
    }
}

/// Two units name the same one when they match once trimmed, whatever the
/// case: `g` and ` G `.
fn same_unit(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.trim().to_lowercase() == b.trim().to_lowercase(),
        (None, None) => true,
        _ => false,
    }
}

/// The lines the planned ingredients make, in the order of their first
/// appearance. Names are matched trimmed and case-insensitively, as
/// everywhere else in the generation.
pub(crate) fn plan_lines(
    ingredients: impl IntoIterator<Item = PlannedIngredient>,
) -> Vec<PlannedLine> {
    let mut lines: Vec<(String, PlannedLine)> = Vec::new();
    for ing in ingredients {
        let key = ing.name.trim().to_lowercase();
        let quantity = scale_quantity(ing.quantity, ing.recipe_servings, ing.selection_servings);
        // A quantity without a unit gets the default one (#458), before the
        // units are compared.
        let unit = normalize_unit(quantity, ing.unit.as_deref());
        match lines.iter_mut().find(|(k, _)| *k == key) {
            Some((_, line)) => {
                if let (Some(sum), Some(q)) = (line.quantity, quantity) {
                    if same_unit(line.unit.as_deref(), unit.as_deref()) {
                        line.quantity = Some(add_quantities(sum, q));
                    }
                }
            }
            None => lines.push((
                key,
                PlannedLine {
                    recipe_id: ing.recipe_id,
                    name: ing.name.trim().to_string(),
                    quantity,
                    unit,
                },
            )),
        }
    }
    lines.into_iter().map(|(_, line)| line).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ing(
        recipe: u128,
        name: &str,
        quantity: Option<f64>,
        unit: Option<&str>,
        recipe_servings: Option<i32>,
        selection_servings: Option<i32>,
    ) -> PlannedIngredient {
        PlannedIngredient {
            recipe_id: Uuid::from_u128(recipe),
            name: name.into(),
            quantity,
            unit: unit.map(Into::into),
            recipe_servings,
            selection_servings,
        }
    }

    fn line(recipe: u128, name: &str, quantity: Option<f64>, unit: Option<&str>) -> PlannedLine {
        PlannedLine {
            recipe_id: Uuid::from_u128(recipe),
            name: name.into(),
            quantity,
            unit: unit.map(Into::into),
        }
    }

    // -- scale_quantity ------------------------------------------------------

    #[test]
    fn scales_to_the_selection_against_the_recipe_base() {
        assert_eq!(scale_quantity(Some(200.0), Some(4), Some(6)), Some(300.0));
        assert_eq!(scale_quantity(Some(3.0), Some(4), Some(2)), Some(1.5));
        assert_eq!(scale_quantity(Some(500.0), Some(4), Some(4)), Some(500.0));
    }

    #[test]
    fn a_scaled_quantity_is_not_rounded() {
        // Decision of 2026-10-25: no rounding, 200 g × 5 / 3 keeps its digits.
        assert_eq!(
            scale_quantity(Some(200.0), Some(3), Some(5)),
            Some(333.333333333)
        );
        assert_eq!(
            scale_quantity(Some(1.0), Some(3), Some(5)),
            Some(1.66666666667)
        );
    }

    #[test]
    fn a_small_scaled_quantity_keeps_every_digit() {
        // Neither the hundredth nor two significant digits.
        assert_eq!(scale_quantity(Some(0.0075), Some(2), Some(2)), Some(0.0075));
        assert_eq!(scale_quantity(Some(0.005), Some(4), Some(4)), Some(0.005));
        assert_eq!(
            scale_quantity(Some(0.01), Some(3), Some(1)),
            Some(0.00333333333333)
        );
        // 0.004 kg for 4, cooked for 1: 0.001 kg, not « 0 kg ».
        assert_eq!(scale_quantity(Some(0.004), Some(4), Some(1)), Some(0.001));
        // A written 0 stays 0.
        assert_eq!(scale_quantity(Some(0.0), Some(4), Some(1)), Some(0.0));
    }

    #[test]
    fn a_scaled_quantity_carries_no_binary_noise() {
        // 0.1 × 3 is 0.30000000000000004 in f64.
        assert_eq!(scale_quantity(Some(0.1), Some(1), Some(3)), Some(0.3));
    }

    #[test]
    fn without_a_base_or_a_target_the_quantity_is_taken_as_written() {
        assert_eq!(scale_quantity(Some(200.0), None, Some(6)), Some(200.0));
        assert_eq!(scale_quantity(Some(200.0), Some(4), None), Some(200.0));
        assert_eq!(scale_quantity(Some(0.125), None, None), Some(0.125));
        assert_eq!(scale_quantity(None, Some(4), Some(6)), None);
    }

    // -- plan_lines ----------------------------------------------------------

    #[test]
    fn each_ingredient_makes_a_line_attributed_to_its_recipe() {
        let lines = plan_lines([
            ing(1, "Farine", Some(200.0), Some("g"), Some(4), Some(8)),
            ing(1, "Oeufs", Some(3.0), Some("pièce"), Some(4), Some(8)),
            ing(2, "Lait", None, None, None, Some(2)),
        ]);
        assert_eq!(
            lines,
            vec![
                line(1, "Farine", Some(400.0), Some("g")),
                line(1, "Oeufs", Some(6.0), Some("pièce")),
                line(2, "Lait", None, None),
            ]
        );
    }

    #[test]
    fn the_same_ingredient_in_two_recipes_adds_up_on_one_line() {
        // « Farine 700 g (somme) »: 200 g scaled ×2, plus 300 g.
        let lines = plan_lines([
            ing(1, "Farine", Some(200.0), Some("g"), Some(2), Some(4)),
            ing(2, " farine ", Some(300.0), Some(" G "), None, None),
        ]);
        assert_eq!(lines, vec![line(1, "Farine", Some(700.0), Some("g"))]);
    }

    #[test]
    fn different_units_keep_the_first_quantity_on_one_line() {
        let lines = plan_lines([
            ing(1, "Lait", Some(1.0), Some("L"), None, None),
            ing(2, "Lait", Some(200.0), Some("ml"), None, None),
            ing(3, "Lait", Some(0.5), Some("l"), None, None),
        ]);
        // The 200 ml are not added; the 0.5 l, in the first line's unit, are.
        assert_eq!(lines, vec![line(1, "Lait", Some(1.5), Some("L"))]);
    }

    #[test]
    fn a_line_without_quantity_is_not_summed_into() {
        let lines = plan_lines([
            ing(1, "Sel", None, None, None, None),
            ing(2, "Sel", Some(5.0), Some("g"), None, None),
        ]);
        assert_eq!(lines, vec![line(1, "Sel", None, None)]);
        let lines = plan_lines([
            ing(1, "Sel", Some(5.0), Some("g"), None, None),
            ing(2, "Sel", None, None, None, None),
        ]);
        assert_eq!(lines, vec![line(1, "Sel", Some(5.0), Some("g"))]);
    }

    #[test]
    fn a_quantity_without_unit_gets_the_default_unit_and_sums_with_it() {
        let lines = plan_lines([
            ing(1, "Citrons", Some(2.0), None, None, None),
            ing(2, "Citrons", Some(1.0), Some("unité"), None, None),
        ]);
        assert_eq!(lines, vec![line(1, "Citrons", Some(3.0), Some("unité"))]);
    }

    #[test]
    fn a_sum_carries_no_binary_noise() {
        // 0.1 + 0.2 is 0.30000000000000004 in f64.
        let lines = plan_lines([
            ing(1, "Huile", Some(0.1), Some("L"), None, None),
            ing(2, "Huile", Some(0.2), Some("L"), None, None),
        ]);
        assert_eq!(lines, vec![line(1, "Huile", Some(0.3), Some("L"))]);
    }

    #[test]
    fn a_sum_of_written_quantities_is_not_rounded() {
        let lines = plan_lines([
            ing(1, "Beurre", Some(0.125), Some("kg"), None, None),
            ing(2, "Beurre", Some(0.0625), Some("kg"), None, None),
        ]);
        assert_eq!(lines, vec![line(1, "Beurre", Some(0.1875), Some("kg"))]);
        // A scaled quantity (1 × 5 / 3) plus a written one keeps the digits
        // of both.
        let lines = plan_lines([
            ing(1, "Sucre", Some(1.0), Some("kg"), Some(3), Some(5)),
            ing(2, "Sucre", Some(0.125), Some("kg"), None, None),
        ]);
        assert_eq!(
            lines,
            vec![line(1, "Sucre", Some(1.79166666667), Some("kg"))]
        );
    }

    #[test]
    fn a_sum_of_tiny_quantities_is_not_zero() {
        let lines = plan_lines([
            ing(1, "Safran", Some(0.001), Some("g"), None, None),
            ing(2, "Safran", Some(0.002), Some("g"), None, None),
        ]);
        assert_eq!(lines, vec![line(1, "Safran", Some(0.003), Some("g"))]);
    }

    #[test]
    fn nothing_planned_makes_no_line() {
        assert_eq!(plan_lines([]), vec![]);
    }
}
