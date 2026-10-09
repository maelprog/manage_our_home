use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use chrono::{Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::AppResult;
use crate::groups::require_role;
use crate::recipes::taxonomy::{taxonomy, TagId, Taxonomy};
use crate::AppState;

/// Rule-based suggestion algorithm (architecture.md: "a concrete v1 rule,
/// not ML"). Scores each family recipe on three inputs named in idea.md:
///
/// 1. **Stock match** — how many of the recipe's required (non-optional)
///    ingredients are currently in stock. Dominant term (0-100 points):
///    lets the family cook what they already have.
/// 2. **Variety** — a flat penalty if the recipe was logged in
///    `meal_history` within the last 14 days, so the same meal doesn't get
///    resuggested right after being eaten.
/// 3. **Season** — a small bonus per ingredient whose `seasonal_months`
///    includes the current month, nudging suggestions toward what's in
///    season without overriding the stock-match term.
///
/// Ingredient/stock matching goes through Open Food Facts' category
/// taxonomy (#406, `recipes::taxonomy`): an article covers an ingredient
/// when its category is the ingredient's or descends from it. An article
/// takes its categories from its scanned product (`off_products`), else
/// from its name; an ingredient from its name. When either side has none,
/// the match falls back to the name (case-insensitive, trimmed). It stays a
/// heuristic, not an exact reservation check (unit conversion is out of
/// scope, same as the low_stock heuristic in stocks::items).
const VARIETY_WINDOW_DAYS: i64 = 14;
const RECENCY_PENALTY: f64 = 30.0;
const SEASONAL_BONUS: f64 = 5.0;

struct RecipeRow {
    id: Uuid,
    name: String,
}

struct IngredientRow {
    recipe_id: Uuid,
    name: String,
    quantity: Option<f64>,
    unit: Option<String>,
    is_optional: bool,
    seasonal_months: Option<Vec<i32>>,
}

#[derive(Serialize)]
pub struct MissingIngredient {
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
}

#[derive(Serialize)]
pub struct RecipeSuggestion {
    pub recipe_id: Uuid,
    pub name: String,
    pub score: f64,
    pub matched_ingredients: usize,
    pub total_required_ingredients: usize,
    pub missing_ingredients: Vec<MissingIngredient>,
    pub recently_eaten: bool,
    pub last_eaten_on: Option<NaiveDate>,
}

#[derive(Deserialize)]
pub struct SuggestionsQuery {
    pub limit: Option<i64>,
}

pub async fn suggest_recipes(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Query(query): Query<SuggestionsQuery>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let recipe_rows = sqlx::query_as!(
        RecipeRow,
        "SELECT id, name FROM recipes WHERE group_id = $1",
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;

    let ingredient_rows = sqlx::query_as!(
        IngredientRow,
        r#"
        SELECT recipe_id, name, quantity, unit, is_optional, seasonal_months
        FROM recipe_ingredients
        WHERE group_id = $1
        "#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;

    // `off_products` is the public Open Food Facts cache (no RLS, see
    // migration 0026); the family's rows come from `stock_items`. A cached
    // record past its TTL still names the product's categories.
    let taxonomy = taxonomy();
    let stock: Vec<StockEntry> = sqlx::query!(
        r#"
        SELECT s.name, o.categories_tags AS "categories_tags?"
        FROM stock_items s
        LEFT JOIN off_products o ON o.code = s.barcode
        WHERE s.group_id = $1 AND s.quantity > 0
        "#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|r| stock_entry(taxonomy, &r.name, &r.categories_tags.unwrap_or_default()))
    .collect();

    let recent_meals: Vec<(Uuid, NaiveDate)> = sqlx::query!(
        r#"
        SELECT recipe_id, MAX(eaten_on) as "last_eaten_on!"
        FROM meal_history
        WHERE group_id = $1 AND eaten_on >= CURRENT_DATE - $2::bigint * INTERVAL '1 day'
        GROUP BY recipe_id
        "#,
        group_id,
        VARIETY_WINDOW_DAYS,
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|r| (r.recipe_id, r.last_eaten_on))
    .collect();
    let recent_meals: HashMap<Uuid, NaiveDate> = recent_meals.into_iter().collect();

    tx.commit().await?;

    let mut ingredients_by_recipe: HashMap<Uuid, Vec<&IngredientRow>> = HashMap::new();
    for ing in &ingredient_rows {
        ingredients_by_recipe
            .entry(ing.recipe_id)
            .or_default()
            .push(ing);
    }

    let current_month = Utc::now().month() as i32;

    let mut suggestions: Vec<RecipeSuggestion> = recipe_rows
        .into_iter()
        .map(|recipe| {
            let ingredients = ingredients_by_recipe
                .get(&recipe.id)
                .cloned()
                .unwrap_or_default();

            let required: Vec<&&IngredientRow> =
                ingredients.iter().filter(|i| !i.is_optional).collect();
            let covered: Vec<bool> = required
                .iter()
                .map(|i| in_stock(&i.name, taxonomy.canonical_ingredient(&i.name), &stock))
                .collect();
            let matched = covered.iter().filter(|&&c| c).count();
            let missing_ingredients: Vec<MissingIngredient> = required
                .iter()
                .zip(&covered)
                .filter(|(_, &c)| !c)
                .map(|(i, _)| MissingIngredient {
                    name: i.name.clone(),
                    quantity: i.quantity,
                    unit: i.unit.clone(),
                })
                .collect();

            let match_ratio = if required.is_empty() {
                1.0
            } else {
                matched as f64 / required.len() as f64
            };

            let last_eaten_on = recent_meals.get(&recipe.id).copied();
            let recently_eaten = last_eaten_on.is_some();

            let seasonal_bonus = ingredients
                .iter()
                .filter(|i| {
                    i.seasonal_months
                        .as_ref()
                        .is_some_and(|months| months.contains(&current_month))
                })
                .count() as f64
                * SEASONAL_BONUS;

            let score = match_ratio * 100.0 - if recently_eaten { RECENCY_PENALTY } else { 0.0 }
                + seasonal_bonus;

            RecipeSuggestion {
                recipe_id: recipe.id,
                name: recipe.name,
                score,
                matched_ingredients: matched,
                total_required_ingredients: required.len(),
                missing_ingredients,
                recently_eaten,
                last_eaten_on,
            }
        })
        .collect();

    suggestions.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());

    let limit = query.limit.unwrap_or(20).max(0) as usize;
    suggestions.truncate(limit);

    Ok(Json(json!({ "suggestions": suggestions })))
}

/// An article in stock as the matching sees it: its name, trimmed and
/// lowercased, and the categories it belongs to, each with all its
/// ancestors (empty when it has none).
struct StockEntry {
    name: String,
    tags: HashSet<TagId>,
}

/// The categories come first from Open Food Facts' `categories_tags` of a
/// scanned article (#402), the ones the taxonomy knows; otherwise from the
/// article's name.
fn stock_entry(taxonomy: &Taxonomy, name: &str, off_categories: &[String]) -> StockEntry {
    let mut direct: Vec<TagId> = off_categories
        .iter()
        .filter_map(|id| taxonomy.tag(id))
        .collect();
    if direct.is_empty() {
        direct.extend(taxonomy.canonical_ingredient(name));
    }
    StockEntry {
        name: name.trim().to_lowercase(),
        tags: direct
            .into_iter()
            .flat_map(|tag| taxonomy.ancestors_or_self(tag))
            .collect(),
    }
}

/// Whether some article in stock covers a recipe's ingredient: by category
/// when both sides have one ([`Taxonomy::satisfies`]), by name otherwise.
fn in_stock(ingredient_name: &str, ingredient_tag: Option<TagId>, stock: &[StockEntry]) -> bool {
    let name = ingredient_name.trim().to_lowercase();
    stock.iter().any(|item| match ingredient_tag {
        Some(tag) if !item.tags.is_empty() => item.tags.contains(&tag),
        _ => item.name == name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, off: &[&str]) -> StockEntry {
        let off: Vec<String> = off.iter().map(|s| s.to_string()).collect();
        stock_entry(taxonomy(), name, &off)
    }

    fn covered(ingredient: &str, stock: &[StockEntry]) -> bool {
        in_stock(
            ingredient,
            taxonomy().canonical_ingredient(ingredient),
            stock,
        )
    }

    #[test]
    fn scanned_article_satisfies_its_ancestor_category() {
        // The issue's example: a scanned "Lait demi-écrémé Lactel" carries
        // Open Food Facts' categories down to en:semi-skimmed-milks.
        let stock = [entry(
            "Lait demi-écrémé Lactel",
            &["en:dairies", "en:milks", "en:semi-skimmed-milks"],
        )];
        assert!(covered("lait", &stock));
        assert!(covered("Lait demi-écrémé", &stock));
        assert!(!covered("lait de coco", &stock));
        assert!(!covered("beurre", &stock));
    }

    #[test]
    fn article_without_categories_is_tagged_from_its_name() {
        let stock = [entry("Lait demi-écrémé", &[])];
        assert!(covered("lait", &stock));
        // Never the reverse: plain milk does not cover semi-skimmed.
        let stock = [entry("Lait", &[])];
        assert!(!covered("lait demi-écrémé", &stock));
        assert!(covered("Laits", &stock));
    }

    #[test]
    fn unknown_categories_fall_back_to_the_name() {
        let stock = [entry("Oeufs", &["xx:not-a-category"])];
        assert!(covered("œufs", &stock));
    }

    #[test]
    fn categories_decide_when_both_sides_have_one() {
        // A drink sold as "Lait d'avoine" but scanned under oat drinks is
        // not milk, whatever its name says.
        let stock = [entry("Lait", &["en:oat-based-drinks"])];
        assert!(!covered("lait", &stock));
    }

    #[test]
    fn name_match_when_either_side_has_no_category() {
        // Neither side in the taxonomy: the previous name match.
        let stock = [entry("  Sauce Maison ", &[])];
        assert!(covered("sauce maison", &stock));
        assert!(!covered("sauce", &stock));
        // Only the article has a category: name match too.
        let stock = [entry("Lait", &[])];
        assert!(!covered("lait entier bio de la ferme", &stock));
        let stock = [entry("lait entier bio de la ferme", &[])];
        assert!(covered("Lait entier bio de la ferme", &stock));
    }

    #[test]
    fn nothing_in_stock_covers_nothing() {
        assert!(!covered("lait", &[]));
    }
}
