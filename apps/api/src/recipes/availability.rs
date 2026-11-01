//! What the stock holds of each planned recipe (#559): the ingredients it
//! lacks (`missing`) and whether it should be cooked first to use up what
//! is about to expire (`use_by`). Derived on every read, nothing stored,
//! like `low_stock` and `expiry_status`.
//!
//! Each planned recipe is weighed **alone** against the current stock: two
//! planned recipes calling for the same eggs both count on all of them
//! (no reservation between planned recipes, decision of 2026-10-21). A line
//! past its date **still counts** as available (same decision).
//!
//! The recipe's required ingredients are first brought to the selection's
//! number of people and merged by name as the grocery list does
//! (`grocery_list::planned::plan_lines`). An ingredient is then matched to
//! the stock lines of its name (`article_key`) holding more than 0, and,
//! when it has a quantity, of its unit too: there is no conversion, as for
//! `outcome::propose` (#473).

use chrono::NaiveDate;
use manage_our_home_shared::validation::stocks::{article_key, expiry_status, ExpiryStatus};
use serde::Serialize;
use uuid::Uuid;

use crate::error::AppResult;
use crate::grocery_list::planned::{
    plan_lines, without_binary_noise, PlannedIngredient, PlannedLine,
};
use crate::recipes::outcome::StockLine;

/// One ingredient of a planned recipe, as written.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RecipeIngredient {
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    pub is_optional: bool,
}

/// What a planned recipe needs: its required ingredients, scaled from the
/// recipe's number of people (`written_for`) to the selection's
/// (`cooked_for`) and merged by name (`plan_lines`). Optional ingredients
/// are left out.
pub(crate) fn needs(
    recipe_id: Uuid,
    ingredients: &[RecipeIngredient],
    written_for: Option<i32>,
    cooked_for: Option<i32>,
) -> Vec<PlannedLine> {
    plan_lines(
        ingredients
            .iter()
            .filter(|i| !i.is_optional)
            .map(|i| PlannedIngredient {
                recipe_id,
                name: i.name.clone(),
                quantity: i.quantity,
                unit: i.unit.clone(),
                recipe_servings: written_for,
                selection_servings: cooked_for,
            }),
    )
}

/// The stock lines a need is matched to: those of its name holding more
/// than 0 and, when it has a quantity, of its unit.
fn lines_for<'a, 's>(
    need: &PlannedLine,
    stock: &'a [StockLine<'s>],
) -> impl Iterator<Item = &'a StockLine<'s>> {
    let name = article_key(&need.name);
    let unit = need
        .quantity
        .map(|_| article_key(need.unit.as_deref().unwrap_or("")));
    stock.iter().filter(move |s| {
        s.quantity > 0.0
            && article_key(s.name) == name
            && unit.as_ref().is_none_or(|u| article_key(s.unit) == *u)
    })
}

/// What the lines hold together, without its binary noise.
fn total<'a, 's: 'a>(lines: impl Iterator<Item = &'a StockLine<'s>>) -> f64 {
    lines.fold(0.0, |sum, s| without_binary_noise(sum + s.quantity))
}

/// What the stock lacks of each need, in the need's unit:
/// - a need with a quantity lacks that quantity less what the lines of its
///   name and unit hold together; it is left out when they cover it;
/// - a need without a quantity lacks entirely when no line of its name holds
///   anything, whatever the unit, and is left out otherwise.
pub(crate) fn missing(needs: &[PlannedLine], stock: &[StockLine<'_>]) -> Vec<PlannedLine> {
    needs
        .iter()
        .filter_map(|need| match need.quantity {
            None => lines_for(need, stock)
                .next()
                .is_none()
                .then(|| need.clone()),
            Some(wanted) => {
                let short = without_binary_noise(wanted - total(lines_for(need, stock)));
                (short > 0.0).then(|| PlannedLine {
                    quantity: Some(short),
                    ..need.clone()
                })
            }
        })
        .collect()
}

/// Whether a line's date is past or within `EXPIRY_SOON_DAYS` of `today`.
fn expiring(line: &StockLine<'_>, today: NaiveDate) -> bool {
    matches!(
        expiry_status(line.expires_on, today),
        ExpiryStatus::Soon | ExpiryStatus::Expired
    )
}

/// The date by which the recipe should be cooked, or `None`. A need is
/// pressing when the lines it is matched to, leaving out those whose date
/// is past or within `EXPIRY_SOON_DAYS` of `today` (`expiry_status`), no
/// longer cover it — and at least one such line exists. The date is the
/// nearest among the expiring lines of the pressing needs.
pub(crate) fn use_by(
    needs: &[PlannedLine],
    stock: &[StockLine<'_>],
    today: NaiveDate,
) -> Option<NaiveDate> {
    needs
        .iter()
        .filter_map(|need| {
            let nearest = lines_for(need, stock)
                .filter(|s| expiring(s, today))
                .filter_map(|s| s.expires_on)
                .min()?;
            let mut lasting = lines_for(need, stock).filter(|s| !expiring(s, today));
            let covered = match need.quantity {
                None => lasting.next().is_some(),
                Some(wanted) => total(lasting) >= wanted,
            };
            (!covered).then_some(nearest)
        })
        .min()
}

/// The planned recipes in the order the list gives them: those with a
/// `use_by` first, the nearest first, then the others, each group keeping
/// the order it came in.
pub(crate) fn by_priority<T>(
    mut items: Vec<(T, Option<NaiveDate>)>,
) -> Vec<(T, Option<NaiveDate>)> {
    // A stable sort: equal keys keep their order. `false` sorts first.
    items.sort_by_key(|(_, date)| (date.is_none(), *date));
    items
}

// -- Reading ------------------------------------------------------------------

/// One ingredient of a planned recipe, with its selection.
struct PlannedIngredientRow {
    selection_id: Uuid,
    recipe_id: Uuid,
    name: String,
    quantity: Option<f64>,
    unit: Option<String>,
    is_optional: bool,
    recipe_servings: Option<i32>,
    selection_servings: Option<i32>,
}

/// A stock line as the availability reads it.
struct StockRow {
    id: Uuid,
    name: String,
    unit: String,
    quantity: f64,
    expires_on: Option<NaiveDate>,
}

/// What the stock holds of one planned selection.
pub(crate) struct Availability {
    pub selection_id: Uuid,
    pub missing: Vec<PlannedLine>,
    pub use_by: Option<NaiveDate>,
}

/// One element of `missing` on the wire.
#[derive(Serialize)]
pub struct MissingResponse {
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
}

impl From<PlannedLine> for MissingResponse {
    fn from(l: PlannedLine) -> Self {
        MissingResponse {
            name: l.name,
            quantity: l.quantity,
            unit: l.unit,
        }
    }
}

/// What the stock holds of the group's `planned` selections — only the one
/// of `recipe_id` when given — against the stock as it is now. A selection
/// whose recipe has no ingredient is not in the result: it lacks nothing
/// and has no date.
pub(crate) async fn planned_availability(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    recipe_id: Option<Uuid>,
    today: NaiveDate,
) -> AppResult<Vec<Availability>> {
    let rows = sqlx::query_as!(
        PlannedIngredientRow,
        r#"
        SELECT s.id AS selection_id, s.recipe_id, ri.name, ri.quantity, ri.unit, ri.is_optional,
               r.servings AS recipe_servings, s.servings AS selection_servings
        FROM recipe_selections s
        JOIN recipes r ON r.id = s.recipe_id
        JOIN recipe_ingredients ri ON ri.recipe_id = s.recipe_id
        WHERE s.group_id = $1 AND s.status = 'planned'
          AND ($2::uuid IS NULL OR s.recipe_id = $2)
        ORDER BY s.created_at, s.id, ri.name, ri.id
        "#,
        group_id,
        recipe_id,
    )
    .fetch_all(&mut **tx)
    .await?;
    let stock = sqlx::query_as!(
        StockRow,
        "SELECT id, name, unit, quantity, expires_on FROM stock_items WHERE group_id = $1",
        group_id,
    )
    .fetch_all(&mut **tx)
    .await?;
    let lines: Vec<StockLine<'_>> = stock
        .iter()
        .map(|s| StockLine {
            id: s.id,
            name: &s.name,
            unit: &s.unit,
            quantity: s.quantity,
            expires_on: s.expires_on,
        })
        .collect();

    let mut result = Vec::new();
    // The rows come grouped by selection.
    for chunk in rows.chunk_by(|a, b| a.selection_id == b.selection_id) {
        let first = &chunk[0];
        let ingredients: Vec<RecipeIngredient> = chunk
            .iter()
            .map(|r| RecipeIngredient {
                name: r.name.clone(),
                quantity: r.quantity,
                unit: r.unit.clone(),
                is_optional: r.is_optional,
            })
            .collect();
        let needs = needs(
            first.recipe_id,
            &ingredients,
            first.recipe_servings,
            first.selection_servings,
        );
        result.push(Availability {
            selection_id: first.selection_id,
            missing: missing(&needs, &lines),
            use_by: use_by(&needs, &lines, today),
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECIPE: Uuid = Uuid::from_u128(1);

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn today() -> NaiveDate {
        d(2026, 10, 21)
    }

    fn in_days(n: i64) -> NaiveDate {
        today() + chrono::Duration::days(n)
    }

    fn need(name: &str, quantity: Option<f64>, unit: Option<&str>) -> PlannedLine {
        PlannedLine {
            recipe_id: RECIPE,
            name: name.into(),
            quantity,
            unit: unit.map(Into::into),
        }
    }

    fn line<'a>(
        id: u128,
        name: &'a str,
        quantity: f64,
        unit: &'a str,
        expires_on: Option<NaiveDate>,
    ) -> StockLine<'a> {
        StockLine {
            id: Uuid::from_u128(id),
            name,
            unit,
            quantity,
            expires_on,
        }
    }

    fn ingredient(
        name: &str,
        quantity: Option<f64>,
        unit: Option<&str>,
        is_optional: bool,
    ) -> RecipeIngredient {
        RecipeIngredient {
            name: name.into(),
            quantity,
            unit: unit.map(Into::into),
            is_optional,
        }
    }

    // -- needs ---------------------------------------------------------------

    #[test]
    fn needs_leave_the_optional_ingredients_out() {
        let needs = needs(
            RECIPE,
            &[
                ingredient("Oeufs", Some(3.0), Some("unité"), false),
                ingredient("Ciboulette", Some(1.0), Some("botte"), true),
            ],
            None,
            None,
        );
        assert_eq!(needs, vec![need("Oeufs", Some(3.0), Some("unité"))]);
    }

    #[test]
    fn needs_are_scaled_to_the_selection_and_merged_by_name() {
        let needs = needs(
            RECIPE,
            &[
                ingredient("Farine", Some(200.0), Some("g"), false),
                ingredient(" farine ", Some(100.0), Some("g"), false),
                ingredient("Sel", None, None, false),
            ],
            Some(2),
            Some(4),
        );
        assert_eq!(
            needs,
            vec![
                need("Farine", Some(600.0), Some("g")),
                need("Sel", None, None),
            ]
        );
    }

    #[test]
    fn a_quantity_without_unit_needs_the_default_unit() {
        let needs = needs(
            RECIPE,
            &[ingredient("Oeufs", Some(3.0), None, false)],
            None,
            None,
        );
        assert_eq!(needs, vec![need("Oeufs", Some(3.0), Some("unité"))]);
    }

    // -- missing -------------------------------------------------------------

    #[test]
    fn three_eggs_against_one_lack_two() {
        let stock = [line(1, "Oeufs", 1.0, "unité", None)];
        assert_eq!(
            missing(&[need("Oeufs", Some(3.0), Some("unité"))], &stock),
            vec![need("Oeufs", Some(2.0), Some("unité"))]
        );
    }

    #[test]
    fn three_eggs_against_five_lack_nothing() {
        let stock = [line(1, "Oeufs", 5.0, "unité", None)];
        assert_eq!(
            missing(&[need("Oeufs", Some(3.0), Some("unité"))], &stock),
            vec![]
        );
        // Exactly enough is enough.
        let stock = [line(1, "Oeufs", 3.0, "unité", None)];
        assert_eq!(
            missing(&[need("Oeufs", Some(3.0), Some("unité"))], &stock),
            vec![]
        );
    }

    #[test]
    fn an_ingredient_without_any_line_lacks_entirely() {
        let stock = [line(1, "Lait", 1.0, "L", None)];
        assert_eq!(
            missing(&[need("Oeufs", Some(3.0), Some("unité"))], &stock),
            vec![need("Oeufs", Some(3.0), Some("unité"))]
        );
    }

    #[test]
    fn the_lines_of_one_name_and_unit_add_up() {
        let stock = [
            line(1, "Farine", 100.0, "g", None),
            line(2, " FARINE ", 150.0, " G ", Some(in_days(10))),
        ];
        assert_eq!(
            missing(&[need("Farine", Some(300.0), Some("g"))], &stock),
            vec![need("Farine", Some(50.0), Some("g"))]
        );
        assert_eq!(
            missing(&[need("Farine", Some(250.0), Some("g"))], &stock),
            vec![]
        );
    }

    #[test]
    fn a_line_in_another_unit_is_not_counted() {
        // No conversion: 1 kg does not cover 200 g.
        let stock = [line(1, "Farine", 1.0, "kg", None)];
        assert_eq!(
            missing(&[need("Farine", Some(200.0), Some("g"))], &stock),
            vec![need("Farine", Some(200.0), Some("g"))]
        );
    }

    #[test]
    fn an_expired_line_still_counts() {
        let stock = [line(1, "Oeufs", 6.0, "unité", Some(in_days(-2)))];
        assert_eq!(
            missing(&[need("Oeufs", Some(3.0), Some("unité"))], &stock),
            vec![]
        );
    }

    #[test]
    fn an_empty_line_holds_nothing() {
        let stock = [line(1, "Sel", 0.0, "g", None)];
        assert_eq!(
            missing(&[need("Sel", None, None)], &stock),
            vec![need("Sel", None, None)]
        );
        assert_eq!(
            missing(&[need("Sel", Some(5.0), Some("g"))], &stock),
            vec![need("Sel", Some(5.0), Some("g"))]
        );
    }

    #[test]
    fn without_quantity_any_line_of_its_name_covers_it() {
        let stock = [line(1, "Sel", 500.0, "g", None)];
        assert_eq!(missing(&[need("Sel", None, None)], &stock), vec![]);
        assert_eq!(
            missing(&[need("Poivre", None, None)], &stock),
            vec![need("Poivre", None, None)]
        );
    }

    #[test]
    fn a_shortfall_carries_no_binary_noise() {
        // 0.3 - 0.1 is 0.19999999999999998 in f64.
        let stock = [line(1, "Huile", 0.1, "L", None)];
        assert_eq!(
            missing(&[need("Huile", Some(0.3), Some("L"))], &stock),
            vec![need("Huile", Some(0.2), Some("L"))]
        );
    }

    #[test]
    fn missing_keeps_the_order_of_the_needs() {
        let needs = [
            need("Oeufs", Some(3.0), Some("unité")),
            need("Farine", Some(200.0), Some("g")),
            need("Lait", Some(1.0), Some("L")),
        ];
        let stock = [line(1, "Farine", 500.0, "g", None)];
        assert_eq!(
            missing(&needs, &stock),
            vec![
                need("Oeufs", Some(3.0), Some("unité")),
                need("Lait", Some(1.0), Some("L")),
            ]
        );
    }

    // -- use_by --------------------------------------------------------------

    #[test]
    fn an_expired_line_that_suffices_dates_the_recipe() {
        let stock = [line(1, "Oeufs", 6.0, "unité", Some(in_days(-2)))];
        assert_eq!(
            use_by(&[need("Oeufs", Some(3.0), Some("unité"))], &stock, today()),
            Some(in_days(-2))
        );
    }

    #[test]
    fn a_later_line_that_suffices_alone_leaves_no_date() {
        let stock = [
            line(1, "Lait", 1.0, "L", Some(in_days(2))),
            line(2, "Lait", 2.0, "L", Some(in_days(30))),
        ];
        assert_eq!(
            use_by(&[need("Lait", Some(2.0), Some("L"))], &stock, today()),
            None
        );
    }

    #[test]
    fn a_later_line_that_does_not_suffice_dates_the_recipe() {
        let stock = [
            line(1, "Lait", 1.0, "L", Some(in_days(2))),
            line(2, "Lait", 1.0, "L", Some(in_days(30))),
        ];
        assert_eq!(
            use_by(&[need("Lait", Some(2.0), Some("L"))], &stock, today()),
            Some(in_days(2))
        );
    }

    #[test]
    fn the_window_is_expiry_soon_days() {
        let n = |days| {
            let stock = [line(1, "Crème", 1.0, "pot", Some(in_days(days)))];
            use_by(&[need("Crème", Some(1.0), Some("pot"))], &stock, today())
        };
        assert_eq!(n(0), Some(today()));
        assert_eq!(n(3), Some(in_days(3)));
        assert_eq!(n(4), None);
    }

    #[test]
    fn an_undated_line_is_not_expiring() {
        let stock = [
            line(1, "Lait", 1.0, "L", Some(in_days(1))),
            line(2, "Lait", 1.0, "L", None),
        ];
        assert_eq!(
            use_by(&[need("Lait", Some(1.0), Some("L"))], &stock, today()),
            None
        );
    }

    #[test]
    fn nothing_expiring_leaves_no_date_even_when_short() {
        let stock = [line(1, "Lait", 1.0, "L", Some(in_days(30)))];
        assert_eq!(
            use_by(&[need("Lait", Some(5.0), Some("L"))], &stock, today()),
            None
        );
        assert_eq!(
            use_by(&[need("Oeufs", Some(5.0), Some("unité"))], &stock, today()),
            None
        );
    }

    #[test]
    fn an_expiring_line_in_another_unit_is_not_counted() {
        let stock = [line(1, "Farine", 1.0, "kg", Some(in_days(1)))];
        assert_eq!(
            use_by(&[need("Farine", Some(200.0), Some("g"))], &stock, today()),
            None
        );
    }

    #[test]
    fn an_empty_expiring_line_is_not_counted() {
        let stock = [line(1, "Lait", 0.0, "L", Some(in_days(1)))];
        assert_eq!(
            use_by(&[need("Lait", Some(1.0), Some("L"))], &stock, today()),
            None
        );
    }

    #[test]
    fn without_quantity_an_expiring_line_alone_dates_the_recipe() {
        let expiring = [line(1, "Persil", 1.0, "botte", Some(in_days(1)))];
        assert_eq!(
            use_by(&[need("Persil", None, None)], &expiring, today()),
            Some(in_days(1))
        );
        let with_a_fresh_one = [
            line(1, "Persil", 1.0, "botte", Some(in_days(1))),
            line(2, "Persil", 1.0, "g", Some(in_days(10))),
        ];
        assert_eq!(
            use_by(&[need("Persil", None, None)], &with_a_fresh_one, today()),
            None
        );
    }

    #[test]
    fn the_nearest_date_of_the_pressing_needs_wins() {
        let needs = [
            need("Lait", Some(1.0), Some("L")),
            need("Crème", Some(1.0), Some("pot")),
            need("Beurre", Some(1.0), Some("plaquette")),
        ];
        let stock = [
            line(1, "Lait", 1.0, "L", Some(in_days(3))),
            line(2, "Crème", 1.0, "pot", Some(in_days(1))),
            // Covered by a fresh line: its expiring one does not date the
            // recipe.
            line(3, "Beurre", 1.0, "plaquette", Some(in_days(-5))),
            line(4, "Beurre", 1.0, "plaquette", Some(in_days(40))),
        ];
        assert_eq!(use_by(&needs, &stock, today()), Some(in_days(1)));
    }

    #[test]
    fn no_need_no_date() {
        let stock = [line(1, "Lait", 1.0, "L", Some(in_days(1)))];
        assert_eq!(use_by(&[], &stock, today()), None);
    }

    // -- by_priority ---------------------------------------------------------

    #[test]
    fn the_dated_recipes_come_first_nearest_first_then_the_others_in_order() {
        let ordered = by_priority(vec![
            ("a", None),
            ("b", Some(in_days(3))),
            ("c", None),
            ("d", Some(in_days(-1))),
            ("e", Some(in_days(3))),
        ]);
        let names: Vec<_> = ordered.into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["d", "b", "e", "a", "c"]);
    }
}
