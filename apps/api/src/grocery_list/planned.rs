//! The grocery lines the planned recipes call for (#470): the pure part of
//! `items::generate_grocery_items`.
//!
//! Only the recipes with a `planned` selection (`recipe_selections`, #469)
//! count. Each quantity is scaled to the selection's number of people
//! against the recipe's base (`recipes.servings`, 0034); without either
//! one it is taken as written. An ingredient named the same in several
//! planned recipes makes one line, attributed to the first recipe: the
//! quantities add up when their units are the same, and otherwise the first
//! one stands (decisions of 2026-10-24). Those lines are then counted
//! against the recipe lines already on the list (`count_against_list`,
//! #842).

use std::collections::HashSet;

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

/// An unchecked line already on the grocery list, as the generation
/// weighs it (#842).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ListedLine {
    pub id: Uuid,
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    /// `manual`, `recipe` or `low_stock` (`grocery_items.source`).
    pub source: String,
}

/// What a need does to the grocery list (#842).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ListChange {
    /// No unchecked line bears its name: a new line.
    Add(PlannedLine),
    /// A recipe line already counts part of it: raised to `quantity`.
    Raise { id: Uuid, quantity: f64 },
}

/// What the needs of the planned recipes (`plan_lines`, one per name) do
/// to the unchecked lines already on the list, names matched trimmed and
/// case-insensitively:
/// - no line of its name: the need is added;
/// - only recipe lines of its name, each with a quantity in the need's unit
///   (`normalize_unit`, no conversion): when they count less than the need
///   together, the first is raised by what they lack. A line is never
///   lowered, so counting again after a raise changes nothing;
/// - otherwise (a manual or low-stock line, a line or a need without
///   quantity, another unit): nothing, as before #842.
pub(crate) fn count_against_list(
    needs: impl IntoIterator<Item = PlannedLine>,
    listed: &[ListedLine],
) -> Vec<ListChange> {
    needs
        .into_iter()
        .filter_map(|need| {
            let key = need.name.trim().to_lowercase();
            let lines: Vec<&ListedLine> = listed
                .iter()
                .filter(|l| l.name.trim().to_lowercase() == key)
                .collect();
            let Some(first) = lines.first() else {
                return Some(ListChange::Add(need));
            };
            let wanted = need.quantity?;
            let mut counted = 0.0;
            for line in &lines {
                let unit = normalize_unit(line.quantity, line.unit.as_deref());
                match line.quantity {
                    Some(q)
                        if line.source == "recipe"
                            && same_unit(unit.as_deref(), need.unit.as_deref()) =>
                    {
                        counted = add_quantities(counted, q);
                    }
                    _ => return None,
                }
            }
            let lacking = without_binary_noise(wanted - counted);
            (lacking > 0.0).then(|| ListChange::Raise {
                id: first.id,
                quantity: add_quantities(first.quantity.unwrap_or(0.0), lacking),
            })
        })
        .collect()
}

/// What `recipe_id` lacks (`missing` of the planned recipes, each weighed
/// alone against the stock, `recipes::availability`), plus what the other
/// planned recipes lack of the same ingredients, merged by name as
/// `plan_lines` does with the recipe's own lines first: the need the list
/// is counted against when only that recipe's missing ingredients are
/// added (#842). An ingredient the recipe does not lack stays out.
pub(crate) fn with_other_planned(
    recipe_id: Uuid,
    missing: impl IntoIterator<Item = PlannedLine>,
) -> Vec<PlannedLine> {
    let (own, others): (Vec<_>, Vec<_>) =
        missing.into_iter().partition(|l| l.recipe_id == recipe_id);
    let names: HashSet<String> = own.iter().map(|l| l.name.trim().to_lowercase()).collect();
    let others = others
        .into_iter()
        .filter(|l| names.contains(&l.name.trim().to_lowercase()));
    plan_lines(own.into_iter().chain(others).map(|l| PlannedIngredient {
        recipe_id: l.recipe_id,
        name: l.name,
        quantity: l.quantity,
        unit: l.unit,
        recipe_servings: None,
        selection_servings: None,
    }))
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

    // -- count_against_list (#842) -------------------------------------------

    fn listed(
        id: u128,
        name: &str,
        quantity: Option<f64>,
        unit: Option<&str>,
        source: &str,
    ) -> ListedLine {
        ListedLine {
            id: Uuid::from_u128(id),
            name: name.into(),
            quantity,
            unit: unit.map(Into::into),
            source: source.into(),
        }
    }

    fn raise(id: u128, quantity: f64) -> ListChange {
        ListChange::Raise {
            id: Uuid::from_u128(id),
            quantity,
        }
    }

    #[test]
    fn a_need_not_on_the_list_is_added() {
        let need = line(1, "Farine", Some(200.0), Some("g"));
        let changes = count_against_list(
            [need.clone()],
            &[listed(9, "Sucre", Some(1.0), Some("kg"), "recipe")],
        );
        assert_eq!(changes, vec![ListChange::Add(need)]);
    }

    #[test]
    fn a_recipe_line_short_of_the_need_is_raised_to_it() {
        // Recipe A put « farine 200 g »; A and B now call for 500 g.
        let changes = count_against_list(
            [line(1, "Farine", Some(500.0), Some("g"))],
            &[listed(9, " farine ", Some(200.0), Some(" G "), "recipe")],
        );
        assert_eq!(changes, vec![raise(9, 500.0)]);
    }

    #[test]
    fn a_recipe_line_is_never_lowered() {
        for counted in [500.0, 800.0] {
            let changes = count_against_list(
                [line(1, "Farine", Some(500.0), Some("g"))],
                &[listed(9, "Farine", Some(counted), Some("g"), "recipe")],
            );
            assert_eq!(changes, vec![], "{counted} g already counted");
        }
    }

    #[test]
    fn units_that_differ_are_not_compared() {
        // No conversion: 1 kg and 300 g are two units.
        let changes = count_against_list(
            [line(1, "Farine", Some(300.0), Some("g"))],
            &[listed(9, "Farine", Some(1.0), Some("kg"), "recipe")],
        );
        assert_eq!(changes, vec![]);
    }

    #[test]
    fn a_quantity_without_unit_compares_in_the_default_unit() {
        // A line « 2 » reads « 2 unité » (#458).
        let changes = count_against_list(
            [line(1, "Citrons", Some(3.0), Some("unité"))],
            &[listed(9, "Citrons", Some(2.0), None, "recipe")],
        );
        assert_eq!(changes, vec![raise(9, 3.0)]);
    }

    #[test]
    fn without_a_quantity_on_either_side_nothing_changes() {
        let changes = count_against_list(
            [line(1, "Sel", Some(5.0), Some("g"))],
            &[listed(9, "Sel", None, None, "recipe")],
        );
        assert_eq!(changes, vec![]);
        let changes = count_against_list(
            [line(1, "Sel", None, None)],
            &[listed(9, "Sel", Some(5.0), Some("g"), "recipe")],
        );
        assert_eq!(changes, vec![]);
    }

    #[test]
    fn a_manual_or_low_stock_line_is_left_as_it_is() {
        for source in ["manual", "low_stock"] {
            let changes = count_against_list(
                [line(1, "Farine", Some(500.0), Some("g"))],
                &[listed(9, "Farine", Some(200.0), Some("g"), source)],
            );
            assert_eq!(changes, vec![], "{source}");
        }
        // Beside a recipe line of the same name too.
        let changes = count_against_list(
            [line(1, "Farine", Some(500.0), Some("g"))],
            &[
                listed(8, "Farine", Some(200.0), Some("g"), "recipe"),
                listed(9, "Farine", None, None, "manual"),
            ],
        );
        assert_eq!(changes, vec![]);
    }

    #[test]
    fn several_recipe_lines_of_the_name_count_together() {
        // 200 g + 100 g already counted, 500 g wanted: the first takes the
        // 200 g lacking.
        let changes = count_against_list(
            [line(1, "Farine", Some(500.0), Some("g"))],
            &[
                listed(8, "Farine", Some(200.0), Some("g"), "recipe"),
                listed(9, "Farine", Some(100.0), Some("g"), "recipe"),
            ],
        );
        assert_eq!(changes, vec![raise(8, 400.0)]);
        // One of them in another unit: nothing compares.
        let changes = count_against_list(
            [line(1, "Farine", Some(500.0), Some("g"))],
            &[
                listed(8, "Farine", Some(200.0), Some("g"), "recipe"),
                listed(9, "Farine", Some(1.0), Some("kg"), "recipe"),
            ],
        );
        assert_eq!(changes, vec![]);
    }

    #[test]
    fn a_raised_quantity_carries_no_binary_noise() {
        let changes = count_against_list(
            [line(1, "Huile", Some(0.3), Some("L"))],
            &[
                listed(8, "Huile", Some(0.1), Some("L"), "recipe"),
                listed(9, "Huile", Some(0.1), Some("L"), "recipe"),
            ],
        );
        assert_eq!(changes, vec![raise(8, 0.2)]);
    }

    #[test]
    fn counting_what_a_raise_left_changes_nothing() {
        let need = line(1, "Farine", Some(500.0), Some("g"));
        let mut list = [listed(9, "Farine", Some(200.0), Some("g"), "recipe")];
        let changes = count_against_list([need.clone()], &list);
        let [ListChange::Raise { quantity, .. }] = changes[..] else {
            panic!("{changes:?}");
        };
        list[0].quantity = Some(quantity);
        assert_eq!(count_against_list([need], &list), vec![]);
    }

    // -- with_other_planned (#842) -------------------------------------------

    #[test]
    fn the_recipe_needs_add_what_other_planned_recipes_lack_of_them() {
        let lines = with_other_planned(
            Uuid::from_u128(2),
            [
                line(1, "Farine", Some(200.0), Some("g")),
                line(1, "Sucre", Some(100.0), Some("g")),
                line(2, "farine", Some(300.0), Some("g")),
                line(2, "Oeufs", Some(2.0), Some("pièce")),
                line(3, "Farine", Some(1.0), Some("kg")),
            ],
        );
        // The recipe's own lines first, in its units; the sugar it does not
        // call for stays out, the kilogram does not add to grams.
        assert_eq!(
            lines,
            vec![
                line(2, "farine", Some(500.0), Some("g")),
                line(2, "Oeufs", Some(2.0), Some("pièce")),
            ]
        );
    }

    #[test]
    fn a_recipe_lacking_nothing_has_no_need() {
        let lines = with_other_planned(
            Uuid::from_u128(2),
            [line(1, "Farine", Some(200.0), Some("g"))],
        );
        assert_eq!(lines, vec![]);
    }

    #[test]
    fn nothing_planned_makes_no_line() {
        assert_eq!(plan_lines([]), vec![]);
    }
}
