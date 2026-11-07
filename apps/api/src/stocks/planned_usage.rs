//! What the planned recipes would take from each stock line (#711), shown
//! on `GET /groups/:id/stock-items` and its detail as `planned_usage`.
//!
//! A recipe is planned while its selection is `planned`; a recipe done or
//! abandoned no longer counts. For each one, its ingredients are matched to
//! the stock exactly as marking it done would (`propose_for_selection`, the
//! function `preview_done` calls): the quantity a line shows for a recipe is
//! the one that preview would propose from that line, scaled to the
//! selection's number of people, in the line's unit. An ingredient spread
//! over several lots counts on each lot for its share; a lot the proposal
//! does not reach shows nothing for that recipe. Each recipe is proposed on
//! its own against the whole stock, as its preview is: two recipes taking
//! from the same lot each show what they would take, not what the other
//! leaves. Optional ingredients count, flagged. A recipe naming the line
//! twice shows once, the shares added up.
//!
//! All the group's planned selections and their ingredients come in one
//! query (`load`), whatever the number of recipes.

use std::collections::HashMap;

use manage_our_home_shared::dto::stocks::PlannedUsage;
use uuid::Uuid;

use crate::error::AppResult;
use crate::grocery_list::planned::without_binary_noise;
use crate::recipes::outcome::{propose_for_selection, Ingredient, StockLine};
use crate::stocks::items::StockItemRow;

/// One ingredient of a planned recipe, as written for the recipe's number
/// of people.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PlannedIngredient<'a> {
    pub name: &'a str,
    pub quantity: Option<f64>,
    pub unit: Option<&'a str>,
    pub is_optional: bool,
}

/// A recipe whose selection is `planned`, its ingredients in the order
/// `preview_done` reads them (by name, then id).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlannedRecipe<'a> {
    pub recipe_id: Uuid,
    pub recipe_name: &'a str,
    /// The recipe's number of people.
    pub written_for: Option<i32>,
    /// The selection's.
    pub cooked_for: Option<i32>,
    pub ingredients: Vec<PlannedIngredient<'a>>,
}

/// The planned recipes' use of each stock line, by line id, each line's
/// uses by recipe name then id. A line no recipe takes from is absent.
pub(crate) fn planned_usage(
    recipes: &[PlannedRecipe<'_>],
    stock: &[StockLine<'_>],
) -> HashMap<Uuid, Vec<PlannedUsage>> {
    let mut usage: HashMap<Uuid, Vec<PlannedUsage>> = HashMap::new();
    for recipe in recipes {
        let written: Vec<Ingredient<'_>> = recipe
            .ingredients
            .iter()
            .map(|i| Ingredient {
                name: i.name,
                quantity: i.quantity,
                unit: i.unit,
            })
            .collect();
        let proposals =
            propose_for_selection(&written, recipe.written_for, recipe.cooked_for, stock);
        for (ingredient, (_, proposal)) in recipe.ingredients.iter().zip(proposals) {
            for take in proposal.lots {
                let Some(line) = stock.iter().find(|s| s.id == take.stock_item_id) else {
                    continue;
                };
                let uses = usage.entry(line.id).or_default();
                match uses.iter_mut().find(|u| u.recipe_id == recipe.recipe_id) {
                    Some(u) => {
                        u.quantity = without_binary_noise(u.quantity + take.quantity);
                        u.is_optional &= ingredient.is_optional;
                    }
                    None => uses.push(PlannedUsage {
                        recipe_id: recipe.recipe_id,
                        recipe_name: recipe.recipe_name.to_string(),
                        quantity: take.quantity,
                        unit: line.unit.to_string(),
                        is_optional: ingredient.is_optional,
                    }),
                }
            }
        }
    }
    for uses in usage.values_mut() {
        uses.sort_by(|a, b| {
            (a.recipe_name.as_str(), a.recipe_id).cmp(&(b.recipe_name.as_str(), b.recipe_id))
        });
    }
    usage
}

/// The stock lines as `propose` reads them.
pub(crate) fn stock_lines(rows: &[StockItemRow]) -> Vec<StockLine<'_>> {
    rows.iter()
        .map(|s| StockLine {
            id: s.id,
            name: &s.name,
            unit: &s.unit,
            quantity: s.quantity,
            expires_on: s.expires_on,
        })
        .collect()
}

/// The whole stock of the group, which each proposal reads.
pub(crate) async fn group_stock(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
) -> AppResult<Vec<StockItemRow>> {
    Ok(sqlx::query_as!(
        StockItemRow,
        r#"SELECT id, group_id, created_by, name, category, quantity, unit, reorder_threshold, expires_on, barcode
           FROM stock_items WHERE group_id = $1"#,
        group_id,
    )
    .fetch_all(&mut **tx)
    .await?)
}

/// The planned recipes' use of each line of `stock`, the group's whole
/// stock: one query for all the group's planned selections.
pub(crate) async fn for_stock(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    stock: &[StockItemRow],
) -> AppResult<HashMap<Uuid, Vec<PlannedUsage>>> {
    let rows = load(tx, group_id).await?;
    Ok(planned_usage(&recipes(&rows), &stock_lines(stock)))
}

/// One ingredient of one planned selection, as `load` reads it.
struct PlannedRow {
    selection_id: Uuid,
    recipe_id: Uuid,
    recipe_name: String,
    recipe_servings: Option<i32>,
    servings: Option<i32>,
    name: String,
    quantity: Option<f64>,
    unit: Option<String>,
    is_optional: bool,
}

/// The ingredients of every planned selection of the group, in one query,
/// grouped by selection.
async fn load(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
) -> AppResult<Vec<PlannedRow>> {
    Ok(sqlx::query_as!(
        PlannedRow,
        r#"
        SELECT s.id AS selection_id, s.recipe_id, r.name AS recipe_name,
               r.servings AS recipe_servings, s.servings,
               i.name, i.quantity, i.unit, i.is_optional
        FROM recipe_selections s
        JOIN recipes r ON r.id = s.recipe_id AND r.group_id = s.group_id
        JOIN recipe_ingredients i ON i.recipe_id = s.recipe_id AND i.group_id = s.group_id
        WHERE s.group_id = $1 AND s.status = 'planned'
        ORDER BY s.id, i.name, i.id
        "#,
        group_id,
    )
    .fetch_all(&mut **tx)
    .await?)
}

/// `load`'s rows as recipes: one per selection, its rows being consecutive.
fn recipes(rows: &[PlannedRow]) -> Vec<PlannedRecipe<'_>> {
    let mut out: Vec<(Uuid, PlannedRecipe<'_>)> = Vec::new();
    for r in rows {
        let ingredient = PlannedIngredient {
            name: &r.name,
            quantity: r.quantity,
            unit: r.unit.as_deref(),
            is_optional: r.is_optional,
        };
        match out.last_mut() {
            Some((id, recipe)) if *id == r.selection_id => recipe.ingredients.push(ingredient),
            _ => out.push((
                r.selection_id,
                PlannedRecipe {
                    recipe_id: r.recipe_id,
                    recipe_name: &r.recipe_name,
                    written_for: r.recipe_servings,
                    cooked_for: r.servings,
                    ingredients: vec![ingredient],
                },
            )),
        }
    }
    out.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn line<'a>(
        n: u128,
        name: &'a str,
        unit: &'a str,
        quantity: f64,
        expires_on: Option<NaiveDate>,
    ) -> StockLine<'a> {
        StockLine {
            id: id(n),
            name,
            unit,
            quantity,
            expires_on,
        }
    }

    fn date(d: u32) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(2026, 11, d)
    }

    fn ing<'a>(name: &'a str, quantity: f64, unit: &'a str) -> PlannedIngredient<'a> {
        PlannedIngredient {
            name,
            quantity: Some(quantity),
            unit: Some(unit),
            is_optional: false,
        }
    }

    fn recipe<'a>(
        n: u128,
        name: &'a str,
        servings: (Option<i32>, Option<i32>),
        ingredients: Vec<PlannedIngredient<'a>>,
    ) -> PlannedRecipe<'a> {
        PlannedRecipe {
            recipe_id: id(n),
            recipe_name: name,
            written_for: servings.0,
            cooked_for: servings.1,
            ingredients,
        }
    }

    fn usage(n: u128, name: &str, quantity: f64, unit: &str, is_optional: bool) -> PlannedUsage {
        PlannedUsage {
            recipe_id: id(n),
            recipe_name: name.into(),
            quantity,
            unit: unit.into(),
            is_optional,
        }
    }

    fn row(selection: u128, recipe: u128, name: &str) -> PlannedRow {
        PlannedRow {
            selection_id: id(selection),
            recipe_id: id(recipe),
            recipe_name: format!("Recette {recipe}"),
            recipe_servings: Some(2),
            servings: Some(4),
            name: name.into(),
            quantity: Some(1.0),
            unit: Some("g".into()),
            is_optional: false,
        }
    }

    /// `load`'s rows, ordered by selection, become one recipe per
    /// selection, its ingredients in the rows' order.
    #[test]
    fn the_rows_become_one_recipe_per_selection() {
        let rows = [
            row(1, 10, "Beurre"),
            row(1, 10, "Farine"),
            row(2, 11, "Farine"),
        ];
        let got = recipes(&rows);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].recipe_id, id(10));
        assert_eq!(got[0].recipe_name, "Recette 10");
        assert_eq!((got[0].written_for, got[0].cooked_for), (Some(2), Some(4)));
        let names: Vec<&str> = got[0].ingredients.iter().map(|i| i.name).collect();
        assert_eq!(names, ["Beurre", "Farine"]);
        assert_eq!(got[1].recipe_id, id(11));
        assert_eq!(got[1].ingredients.len(), 1);
        assert!(recipes(&[]).is_empty());
    }

    #[test]
    fn without_a_planned_recipe_no_line_is_used() {
        let stock = [line(1, "Farine", "g", 500.0, None)];
        assert!(planned_usage(&[], &stock).is_empty());
    }

    #[test]
    fn a_line_shows_the_quantity_scaled_to_the_selection() {
        let stock = [
            line(1, "Farine", "g", 500.0, None),
            line(2, "Pain", "unité", 1.0, None),
        ];
        let crepes = recipe(
            10,
            "Crêpes",
            (Some(2), Some(4)),
            vec![ing("farine", 200.0, "g")],
        );
        let got = planned_usage(&[crepes], &stock);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[&id(1)], vec![usage(10, "Crêpes", 400.0, "g", false)]);
    }

    #[test]
    fn an_ingredient_over_several_lots_counts_on_each_for_its_share() {
        let stock = [
            line(1, "Farine", "g", 500.0, date(20)),
            line(2, "Farine", "g", 300.0, None),
            line(3, "Farine", "g", 100.0, date(3)),
        ];
        let pain = recipe(10, "Pain", (None, None), vec![ing("Farine", 400.0, "g")]);
        let got = planned_usage(&[pain], &stock);
        assert_eq!(got[&id(3)], vec![usage(10, "Pain", 100.0, "g", false)]);
        assert_eq!(got[&id(1)], vec![usage(10, "Pain", 300.0, "g", false)]);
        assert!(!got.contains_key(&id(2)), "the undated lot is not reached");
    }

    #[test]
    fn an_optional_ingredient_counts_flagged() {
        let stock = [line(1, "Persil", "botte", 1.0, None)];
        let soupe = recipe(
            10,
            "Soupe",
            (None, None),
            vec![PlannedIngredient {
                is_optional: true,
                ..ing("Persil", 1.0, "botte")
            }],
        );
        assert_eq!(
            planned_usage(&[soupe], &stock)[&id(1)],
            vec![usage(10, "Soupe", 1.0, "botte", true)]
        );
    }

    /// Each recipe is proposed on its own, as its own preview is: both
    /// show 400 g of the 500 g lot, by recipe name.
    #[test]
    fn two_recipes_on_one_line_each_show_their_own_proposal() {
        let stock = [line(1, "Farine", "g", 500.0, None)];
        let pain = recipe(10, "Pain", (None, None), vec![ing("Farine", 400.0, "g")]);
        let crepes = recipe(
            11,
            "Crêpes",
            (Some(2), Some(4)),
            vec![ing("Farine", 200.0, "g")],
        );
        assert_eq!(
            planned_usage(&[pain, crepes], &stock)[&id(1)],
            vec![
                usage(11, "Crêpes", 400.0, "g", false),
                usage(10, "Pain", 400.0, "g", false),
            ]
        );
    }

    /// The preview proposes 0 from the matched line when the units differ
    /// or the recipe gives no quantity: the line still shows the recipe, at
    /// 0. Without a line of that name, nothing.
    #[test]
    fn a_line_the_preview_proposes_nothing_from_shows_zero() {
        let stock = [
            line(1, "Sucre", "kg", 1.0, None),
            line(2, "Sel", "g", 100.0, None),
        ];
        let gateau = recipe(
            10,
            "Gâteau",
            (None, None),
            vec![
                ing("Sucre", 30.0, "g"),
                PlannedIngredient {
                    quantity: None,
                    ..ing("Sel", 0.0, "g")
                },
                ing("Oeufs", 3.0, "unité"),
            ],
        );
        let got = planned_usage(&[gateau], &stock);
        assert_eq!(got[&id(1)], vec![usage(10, "Gâteau", 0.0, "kg", false)]);
        assert_eq!(got[&id(2)], vec![usage(10, "Gâteau", 0.0, "g", false)]);
        assert_eq!(got.len(), 2);
    }

    /// The second ingredient of the name takes what the first left, as in
    /// the preview; the line shows the recipe once, the shares added up,
    /// optional only when every share is.
    #[test]
    fn a_recipe_naming_a_line_twice_shows_once_with_the_shares_added() {
        let stock = [line(1, "Beurre", "g", 250.0, None)];
        let optional_butter = PlannedIngredient {
            is_optional: true,
            ..ing("Beurre", 100.0, "g")
        };
        let tarte = recipe(
            10,
            "Tarte",
            (None, None),
            vec![ing("Beurre", 200.0, "g"), optional_butter],
        );
        assert_eq!(
            planned_usage(&[tarte], &stock)[&id(1)],
            vec![usage(10, "Tarte", 250.0, "g", false)]
        );
        let all_optional = recipe(
            11,
            "Sablés",
            (None, None),
            vec![optional_butter, optional_butter],
        );
        assert_eq!(
            planned_usage(&[all_optional], &stock)[&id(1)],
            vec![usage(11, "Sablés", 200.0, "g", true)]
        );
    }

    /// The quantity a line shows is the take `propose_for_selection` gives
    /// for it, the function `preview_done` calls.
    #[test]
    fn the_quantity_is_the_take_the_done_preview_proposes() {
        let stock = [
            line(1, "Farine", "g", 150.0, date(2)),
            line(2, "Farine", "g", 1000.0, date(9)),
            line(3, "Lait", "l", 0.5, None),
        ];
        let crepes = recipe(
            10,
            "Crêpes",
            (Some(4), Some(6)),
            vec![ing("Farine", 250.0, "g"), ing("Lait", 0.5, "l")],
        );
        let written: Vec<Ingredient<'_>> = crepes
            .ingredients
            .iter()
            .map(|i| Ingredient {
                name: i.name,
                quantity: i.quantity,
                unit: i.unit,
            })
            .collect();
        let preview = propose_for_selection(&written, Some(4), Some(6), &stock);
        let got = planned_usage(std::slice::from_ref(&crepes), &stock);
        let mut seen = 0;
        for (_, proposal) in &preview {
            for t in &proposal.lots {
                assert_eq!(got[&t.stock_item_id][0].quantity, t.quantity);
                seen += 1;
            }
        }
        assert_eq!(seen, 3, "two lots of flour and the milk");
        assert_eq!(got.len(), 3);
    }
}
