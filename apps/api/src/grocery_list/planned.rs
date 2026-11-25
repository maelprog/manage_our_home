//! The grocery lines the planned recipes call for (#470): the pure part of
//! `items::generate_grocery_items`.
//!
//! Only the recipes with a `planned` selection (`recipe_selections`, #469)
//! count. Each quantity is scaled to the selection's number of people
//! against the recipe's base (`recipes.servings`, 0034), a recipe without
//! one counting as written for 1 (`scale_to_selection`, #1014, arbitrage of
//! 2026-11-25, replacing the decision of 2026-10-24 that took it as
//! written); a selection without one takes the quantity as written. An
//! ingredient named the same in several planned recipes makes one line,
//! attributed to the first recipe: the quantities add up when their units
//! are the same, and otherwise the first one stands (decisions of
//! 2026-10-24). Those lines are then counted
//! against the recipe lines already on the list (`count_against_list`,
//! #842).
//!
//! Two names are one ingredient (#902) when they read the same once
//! trimmed, whatever the case, or when the taxonomy maps both to one tag
//! (`Taxonomy::canonical_ingredient`: « parmesan » and « Parmigiano
//! Reggiano »). Names without a tag are never matched by a word they share.
//! Two different names of one tag are still two lines when both carry a
//! quantity and their units differ: there is no conversion, so neither
//! line's quantity would stand for the other's.

use manage_our_home_shared::validation::grocery_list::normalize_unit;
// Shared with the recipe edit form (#845), which scales the quantities of a
// recipe whose number of people changes the same way.
pub(crate) use manage_our_home_shared::validation::recipes::{
    scale_to_recipe, scale_to_selection, without_binary_noise,
};
use uuid::Uuid;

use crate::recipes::taxonomy::{taxonomy, TagId};

/// One required ingredient of a planned recipe, with the two numbers of
/// people its quantity is scaled between.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlannedIngredient {
    pub recipe_id: Uuid,
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    /// The number of people the recipe is written for (`recipes.servings`);
    /// none counts as 1 when the selection says one (#1014).
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

/// A sum of quantities, without its binary noise.
fn add_quantities(a: f64, b: f64) -> f64 {
    without_binary_noise(a + b)
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

/// What an ingredient's name is matched by (#902): the name trimmed and
/// lowercased, and the tag the taxonomy maps it to, if any.
#[derive(Debug, Clone, PartialEq)]
struct IngredientKey {
    name: String,
    tag: Option<TagId>,
}

impl IngredientKey {
    fn of(name: &str) -> Self {
        Self {
            name: name.trim().to_lowercase(),
            tag: taxonomy().canonical_ingredient(name),
        }
    }

    /// Whether the two names are one ingredient: the same name, or the
    /// same tag.
    fn same_ingredient(&self, other: &Self) -> bool {
        self.name == other.name || self.tag.is_some_and(|t| other.tag == Some(t))
    }
}

/// Two quantities whose units differ cannot stand for each other; a side
/// without a quantity conflicts with nothing.
fn units_conflict(a: (Option<f64>, Option<&str>), b: (Option<f64>, Option<&str>)) -> bool {
    a.0.is_some() && b.0.is_some() && !same_unit(a.1, b.1)
}

/// Whether a line already made and an ingredient of the two keys make one
/// line (#902): the same name always does, as before, whatever the units;
/// two names of one tag do when their units do not conflict.
fn one_line(
    a: &IngredientKey,
    a_qty: (Option<f64>, Option<&str>),
    b: &IngredientKey,
    b_qty: (Option<f64>, Option<&str>),
) -> bool {
    a.name == b.name || (a.same_ingredient(b) && !units_conflict(a_qty, b_qty))
}

/// The lines the planned ingredients make, in the order of their first
/// appearance. Names are matched trimmed and case-insensitively, as
/// everywhere else in the generation, or by their tag (#902): an
/// ingredient goes to the first line it makes one line with
/// (`one_line`), preferring one whose units do not conflict with its own,
/// so that its quantity can add up.
pub(crate) fn plan_lines(
    ingredients: impl IntoIterator<Item = PlannedIngredient>,
) -> Vec<PlannedLine> {
    let scaled = ingredients.into_iter().map(|ing| {
        let quantity =
            scale_to_selection(ing.quantity, ing.recipe_servings, ing.selection_servings);
        PlannedLine {
            recipe_id: ing.recipe_id,
            name: ing.name.trim().to_string(),
            quantity,
            // A quantity without a unit gets the default one (#458),
            // before the units are compared.
            unit: normalize_unit(quantity, ing.unit.as_deref()),
        }
    });
    group_lines(scaled).into_iter().map(|g| g.line).collect()
}

/// One line `plan_lines` makes, and the lines merged into it, the first
/// included, in their order.
struct Group {
    line: PlannedLine,
    members: Vec<PlannedLine>,
}

/// `plan_lines`' merge, on lines already scaled and of a normalized unit.
fn group_lines(lines: impl IntoIterator<Item = PlannedLine>) -> Vec<Group> {
    let mut groups: Vec<(IngredientKey, Group)> = Vec::new();
    for l in lines {
        let key = IngredientKey::of(&l.name);
        let own = (l.quantity, l.unit.as_deref());
        let fits = |(k, g): &(IngredientKey, Group)| {
            one_line(k, (g.line.quantity, g.line.unit.as_deref()), &key, own)
        };
        let at = groups
            .iter()
            .position(|e| {
                fits(e) && !units_conflict((e.1.line.quantity, e.1.line.unit.as_deref()), own)
            })
            .or_else(|| groups.iter().position(fits));
        match at.map(|i| &mut groups[i].1) {
            Some(group) => {
                let line = &mut group.line;
                if let (Some(sum), Some(q)) = (line.quantity, l.quantity) {
                    if same_unit(line.unit.as_deref(), l.unit.as_deref()) {
                        line.quantity = Some(add_quantities(sum, q));
                    }
                }
                group.members.push(l);
            }
            None => groups.push((
                key,
                Group {
                    line: l.clone(),
                    members: vec![l],
                },
            )),
        }
    }
    groups.into_iter().map(|(_, g)| g).collect()
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

/// What the needs of the planned recipes (`plan_lines`, one per
/// ingredient) do to the unchecked lines already on the list. A line is the
/// need's when its name is the need's, trimmed and case-insensitively, or
/// when both names map to one tag (#902) — except a recipe line of another
/// name whose quantity's unit conflicts with the need's: `plan_lines` keeps
/// such a line apart, so it does not count for the need.
/// - no line of the need: the need is added;
/// - only recipe lines of the need, each with a quantity in the need's unit
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
            let key = IngredientKey::of(&need.name);
            let wanted_unit = (need.quantity, need.unit.as_deref());
            let lines: Vec<&ListedLine> = listed
                .iter()
                .filter(|l| {
                    let unit = normalize_unit(l.quantity, l.unit.as_deref());
                    let line_key = IngredientKey::of(&l.name);
                    if l.source == "recipe" {
                        one_line(&line_key, (l.quantity, unit.as_deref()), &key, wanted_unit)
                    } else {
                        line_key.same_ingredient(&key)
                    }
                })
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

/// The lines `plan_lines` makes of what the planned recipes lack
/// (`missing`, each recipe weighed alone against the stock,
/// `recipes::availability`, in the order the recipes were planned) that
/// `recipe_id` has a part in, each with the lines merged into it: the need
/// the list is counted against when only that recipe's missing ingredients
/// are added (#842). The lines are merged across every planned recipe, in
/// their order, as the generation merges them: whether two names of one
/// tag make one line depends on the lines met before them (#902), so
/// merging the recipe's own lines first would make other lines than the
/// generation's, and adding the missing ingredients after a generation
/// would change the list. An ingredient the recipe does not lack stays out.
fn with_other_planned(
    recipe_id: Uuid,
    missing: impl IntoIterator<Item = PlannedLine>,
) -> impl Iterator<Item = Group> {
    group_lines(missing)
        .into_iter()
        .filter(move |g| g.members.iter().any(|m| m.recipe_id == recipe_id))
}

/// What adding `recipe_id`'s missing ingredients does to the list (#842):
/// a recipe line already there is counted against what every planned
/// recipe lacks of the ingredient (`with_other_planned`), but the lines
/// created carry only what `recipe_id` lacks: its own lines merged into
/// that need.
pub(crate) fn count_missing_against_list(
    recipe_id: Uuid,
    missing: impl IntoIterator<Item = PlannedLine>,
    listed: &[ListedLine],
) -> Vec<ListChange> {
    with_other_planned(recipe_id, missing)
        .flat_map(|group| {
            let own: Vec<PlannedLine> = group
                .members
                .into_iter()
                .filter(|m| m.recipe_id == recipe_id)
                .collect();
            count_against_list([group.line], listed)
                .into_iter()
                .flat_map(move |change| match change {
                    ListChange::Add(_) => own.iter().cloned().map(ListChange::Add).collect(),
                    raise => vec![raise],
                })
                .collect::<Vec<_>>()
        })
        .collect()
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

    fn other_planned_lines(
        recipe: Uuid,
        missing: impl IntoIterator<Item = PlannedLine>,
    ) -> Vec<PlannedLine> {
        with_other_planned(recipe, missing)
            .map(|g| g.line)
            .collect()
    }

    #[test]
    fn the_recipe_needs_add_what_other_planned_recipes_lack_of_them() {
        let lines = other_planned_lines(
            Uuid::from_u128(2),
            [
                line(1, "Farine", Some(200.0), Some("g")),
                line(1, "Sucre", Some(100.0), Some("g")),
                line(2, "farine", Some(300.0), Some("g")),
                line(2, "Oeufs", Some(2.0), Some("pièce")),
                line(3, "Farine", Some(1.0), Some("kg")),
            ],
        );
        // The lines the generation makes, in the planned order (#902); the
        // sugar the recipe does not call for stays out, the kilogram does
        // not add to grams.
        assert_eq!(
            lines,
            vec![
                line(1, "Farine", Some(500.0), Some("g")),
                line(2, "Oeufs", Some(2.0), Some("pièce")),
            ]
        );
    }

    // -- count_missing_against_list (#842) -----------------------------------

    #[test]
    fn a_missing_line_is_created_with_the_recipe_own_quantity() {
        // A lacks 200 g, B 300 g; nothing on the list yet.
        let changes = count_missing_against_list(
            Uuid::from_u128(1),
            [
                line(1, "Farine", Some(200.0), Some("g")),
                line(2, "Farine", Some(300.0), Some("g")),
            ],
            &[],
        );
        assert_eq!(
            changes,
            vec![ListChange::Add(line(1, "Farine", Some(200.0), Some("g")))]
        );
    }

    #[test]
    fn a_listed_recipe_line_counts_against_every_planned_recipe() {
        // A's 200 g already listed; adding B's missing raises it to 500 g.
        let changes = count_missing_against_list(
            Uuid::from_u128(2),
            [
                line(1, "Farine", Some(200.0), Some("g")),
                line(2, "Farine", Some(300.0), Some("g")),
            ],
            &[listed(9, "Farine", Some(200.0), Some("g"), "recipe")],
        );
        assert_eq!(changes, vec![raise(9, 500.0)]);
    }

    #[test]
    fn a_recipe_lacking_nothing_has_no_need() {
        let lines = other_planned_lines(
            Uuid::from_u128(2),
            [line(1, "Farine", Some(200.0), Some("g"))],
        );
        assert_eq!(lines, vec![]);
    }

    #[test]
    fn nothing_planned_makes_no_line() {
        assert_eq!(plan_lines([]), vec![]);
    }

    // -- one key per canonical ingredient (#902) -----------------------------

    #[test]
    fn two_names_of_one_ingredient_in_one_unit_make_one_line() {
        // Both are the taxonomy's en:parmigiano-reggiano.
        let lines = plan_lines([
            ing(1, "parmesan", Some(100.0), Some("g"), None, None),
            ing(2, "Parmigiano Reggiano", Some(50.0), Some("g"), None, None),
        ]);
        assert_eq!(lines, vec![line(1, "parmesan", Some(150.0), Some("g"))]);
        // The spelling with or without its accent, too (#901's alias).
        let lines = plan_lines([
            ing(1, "crème fraîche", Some(20.0), Some("cl"), None, None),
            ing(2, "Crème fraiche", Some(10.0), Some("cl"), None, None),
        ]);
        assert_eq!(
            lines,
            vec![line(1, "crème fraîche", Some(30.0), Some("cl"))]
        );
    }

    #[test]
    fn two_names_of_one_ingredient_in_two_units_make_two_lines() {
        let lines = plan_lines([
            ing(1, "parmesan", Some(100.0), Some("g"), None, None),
            ing(2, "Parmigiano Reggiano", Some(1.0), Some("kg"), None, None),
            // A third, in the second line's unit, adds up with it.
            ing(3, "parmesan", Some(0.5), Some("kg"), None, None),
        ]);
        assert_eq!(
            lines,
            vec![
                line(1, "parmesan", Some(100.0), Some("g")),
                line(2, "Parmigiano Reggiano", Some(1.5), Some("kg")),
            ]
        );
        // Without a quantity, a unit conflicts with nothing.
        let lines = plan_lines([
            ing(1, "parmesan", None, None, None, None),
            ing(2, "Parmigiano Reggiano", Some(50.0), Some("g"), None, None),
        ]);
        assert_eq!(lines, vec![line(1, "parmesan", None, None)]);
    }

    #[test]
    fn names_without_a_tag_are_not_guessed_together() {
        let t = crate::recipes::taxonomy::taxonomy();
        for name in ["sauce maison", "sauce maison épicée"] {
            assert_eq!(t.canonical_ingredient(name), None, "{name}");
        }
        let lines = plan_lines([
            ing(1, "Sauce maison", Some(1.0), Some("pot"), None, None),
            ing(2, "sauce maison épicée", Some(1.0), Some("pot"), None, None),
        ]);
        assert_eq!(
            lines,
            vec![
                line(1, "Sauce maison", Some(1.0), Some("pot")),
                line(2, "sauce maison épicée", Some(1.0), Some("pot")),
            ]
        );
    }

    #[test]
    fn a_manual_line_of_another_name_of_the_ingredient_is_seen() {
        for unit in [None, Some("kg")] {
            let changes = count_against_list(
                [line(1, "parmesan", Some(100.0), Some("g"))],
                &[listed(
                    9,
                    "parmigiano reggiano",
                    unit.map(|_| 1.0),
                    unit,
                    "manual",
                )],
            );
            assert_eq!(changes, vec![], "{unit:?}");
        }
    }

    #[test]
    fn a_recipe_line_of_another_name_of_the_ingredient_is_counted() {
        let changes = count_against_list(
            [line(1, "parmesan", Some(150.0), Some("g"))],
            &[listed(
                9,
                "Parmigiano Reggiano",
                Some(100.0),
                Some("g"),
                "recipe",
            )],
        );
        assert_eq!(changes, vec![raise(9, 150.0)]);
        // In another unit, it is another line, as `plan_lines` keeps it.
        let need = line(1, "parmesan", Some(150.0), Some("g"));
        let changes = count_against_list(
            [need.clone()],
            &[listed(
                9,
                "Parmigiano Reggiano",
                Some(1.0),
                Some("kg"),
                "recipe",
            )],
        );
        assert_eq!(changes, vec![ListChange::Add(need)]);
    }

    #[test]
    fn the_lines_two_names_made_count_as_they_are() {
        // What `plan_lines` made of two names in two units, then counted
        // against the list they were written to: nothing changes.
        let needs = plan_lines([
            ing(1, "parmesan", Some(100.0), Some("g"), None, None),
            ing(2, "Parmigiano Reggiano", Some(1.0), Some("kg"), None, None),
        ]);
        let list = [
            listed(8, "parmesan", Some(100.0), Some("g"), "recipe"),
            listed(9, "Parmigiano Reggiano", Some(1.0), Some("kg"), "recipe"),
        ];
        assert_eq!(count_against_list(needs, &list), vec![]);
    }

    #[test]
    fn other_planned_recipes_count_by_ingredient_not_by_name() {
        let lines = other_planned_lines(
            Uuid::from_u128(2),
            [
                line(1, "Parmigiano Reggiano", Some(50.0), Some("g")),
                line(2, "parmesan", Some(100.0), Some("g")),
                line(3, "parmigiano reggiano", Some(1.0), Some("kg")),
            ],
        );
        assert_eq!(
            lines,
            vec![line(1, "Parmigiano Reggiano", Some(150.0), Some("g"))]
        );
        // The line created carries the recipe's own quantity, under its name.
        let changes = count_missing_against_list(
            Uuid::from_u128(2),
            [
                line(1, "Parmigiano Reggiano", Some(50.0), Some("g")),
                line(2, "parmesan", Some(100.0), Some("g")),
            ],
            &[],
        );
        assert_eq!(
            changes,
            vec![ListChange::Add(line(2, "parmesan", Some(100.0), Some("g")))]
        );
    }

    // -- replaying changes nothing, whatever the order (#902) ---------------

    /// The grocery list the changes are written to, as `apply_list_changes`
    /// writes them: a line added is a recipe line, a raise sets the
    /// quantity.
    fn apply(list: &mut Vec<ListedLine>, changes: Vec<ListChange>) {
        for change in changes {
            match change {
                ListChange::Add(l) => list.push(ListedLine {
                    id: Uuid::from_u128(1000 + list.len() as u128),
                    name: l.name,
                    quantity: l.quantity,
                    unit: l.unit,
                    source: "recipe".into(),
                }),
                ListChange::Raise { id, quantity } => {
                    let line = list.iter_mut().find(|l| l.id == id).unwrap();
                    line.quantity = Some(quantity);
                }
            }
        }
    }

    /// What `recipes::availability::missing` hands the grocery list with
    /// an empty stock: each recipe's needs, merged recipe by recipe.
    fn missing_of(recipes: &[Vec<PlannedIngredient>]) -> Vec<PlannedLine> {
        recipes
            .iter()
            .flat_map(|r| plan_lines(r.iter().cloned()))
            .collect()
    }

    fn generate(recipes: &[Vec<PlannedIngredient>], list: &[ListedLine]) -> Vec<ListChange> {
        count_against_list(plan_lines(recipes.iter().flatten().cloned()), list)
    }

    fn add_missing(
        recipes: &[Vec<PlannedIngredient>],
        recipe: usize,
        list: &[ListedLine],
    ) -> Vec<ListChange> {
        count_missing_against_list(Uuid::from_u128(recipe as u128), missing_of(recipes), list)
    }

    #[test]
    fn adding_the_missing_after_a_generation_changes_nothing() {
        // A « parmesan 3 kg », B « parmesan 4 » (unité), C « Parmesan râpé
        // 4 » (unité), all three the taxonomy's en:parmigiano-reggiano.
        let recipes = vec![
            vec![ing(0, "parmesan", Some(3.0), Some("kg"), None, None)],
            vec![ing(1, "parmesan", Some(4.0), None, None, None)],
            vec![ing(2, "Parmesan râpé", Some(4.0), None, None, None)],
        ];
        let mut list = Vec::new();
        apply(&mut list, generate(&recipes, &[]));
        let generated = list.clone();
        for r in [0, 1, 2, 2] {
            assert_eq!(add_missing(&recipes, r, &list), vec![], "recipe {r}");
        }
        assert_eq!(list, generated);
        assert_eq!(generate(&recipes, &list), vec![]);
    }

    /// A small deterministic generator (xorshift), enough to draw recipes.
    struct Draw(u64);

    impl Draw {
        fn next(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }
    }

    fn draw_recipes(d: &mut Draw) -> Vec<Vec<PlannedIngredient>> {
        const NAMES: [&str; 6] = [
            "parmesan",
            "Parmigiano Reggiano",
            "Parmesan râpé",
            "Farine",
            "farine",
            "sauce maison",
        ];
        const UNITS: [Option<&str>; 3] = [None, Some("g"), Some("kg")];
        (0..1 + d.next(4))
            .map(|r| {
                (0..1 + d.next(3))
                    .map(|_| {
                        let quantity = [None, Some(1.0), Some(2.0), Some(4.0)][d.next(4)];
                        let name = NAMES[d.next(NAMES.len())];
                        let unit = UNITS[d.next(UNITS.len())];
                        ing(r as u128, name, quantity, unit, None, None)
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn replaying_the_generation_or_the_missing_changes_nothing() {
        let mut d = Draw(0x9e37_79b9_7f4a_7c15);
        for case in 0..20_000 {
            let recipes = draw_recipes(&mut d);
            let n = recipes.len();
            // Generated, then each recipe's missing added: nothing to add.
            let mut list = Vec::new();
            apply(&mut list, generate(&recipes, &[]));
            assert_eq!(generate(&recipes, &list), vec![], "{case}: {recipes:?}");
            for r in 0..n {
                assert_eq!(
                    add_missing(&recipes, r, &list),
                    vec![],
                    "{case}: recipe {r} after the generation: {recipes:?}"
                );
            }
            // Each recipe's missing added, in a drawn order: added again,
            // nothing changes. (A generation then may still raise a line:
            // `recipes::availability` merges each recipe's lines before
            // they are merged across recipes, the generation merges them
            // all at once, as before #902.)
            let mut list = Vec::new();
            let order: Vec<usize> = (0..n).map(|_| d.next(n)).chain(0..n).collect();
            for &r in &order {
                let changes = add_missing(&recipes, r, &list);
                apply(&mut list, changes);
            }
            for r in 0..n {
                assert_eq!(
                    add_missing(&recipes, r, &list),
                    vec![],
                    "{case}: recipe {r} replayed after {order:?}: {recipes:?}"
                );
            }
        }
    }
}
