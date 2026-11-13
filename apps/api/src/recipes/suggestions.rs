use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::AppResult;
use crate::groups::require_role;
use crate::recipes::taxonomy::{taxonomy, TagId, Taxonomy};
use crate::stocks::items::today;
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
/// from its name; an ingredient from its name. Equal names (case-insensitive,
/// trimmed) match too, whether or not either side has a category. It stays a
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

#[derive(Serialize, utoipa::ToSchema)]
pub struct MissingIngredient {
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
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

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SuggestionsQuery {
    pub limit: Option<i64>,
    /// `true` keeps only the recipes the stock covers entirely
    /// ([`is_possible`], #539), before `limit` applies.
    #[serde(default)]
    pub possible: bool,
}

#[utoipa::path(
    get,
    path = "/groups/{id}/recipes/suggestions",
    tag = "recipes",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        SuggestionsQuery,
    ),
    responses(
        (status = 200, description = "Recipes ranked by what the stock covers", body = crate::api_docs::schemas::SuggestionList),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn suggest_recipes(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Query(query): Query<SuggestionsQuery>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    let mut suggestions = score_recipes(&mut tx, group_id).await?;
    tx.commit().await?;

    if query.possible {
        suggestions.retain(|s| is_possible(s.matched_ingredients, s.total_required_ingredients));
    }
    suggestions.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());

    let limit = query.limit.unwrap_or(20).max(0) as usize;
    suggestions.truncate(limit);

    Ok(Json(json!({ "suggestions": suggestions })))
}

/// Every recipe of the group, scored, in the order the rows came in.
/// Shared by the suggestions and the replacements of a planned selection
/// (#815).
pub(crate) async fn score_recipes(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
) -> AppResult<Vec<RecipeSuggestion>> {
    let recipe_rows = sqlx::query_as!(
        RecipeRow,
        "SELECT id, name FROM recipes WHERE group_id = $1",
        group_id,
    )
    .fetch_all(&mut **tx)
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
    .fetch_all(&mut **tx)
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
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| stock_entry(taxonomy, &r.name, &r.categories_tags.unwrap_or_default()))
    .collect();

    let recent_meals: Vec<(Uuid, NaiveDate)> = sqlx::query!(
        r#"
        SELECT recipe_id, MAX(eaten_on) as "last_eaten_on!"
        FROM meal_history
        WHERE group_id = $1 AND eaten_on >= $3::date - $2::bigint * INTERVAL '1 day'
        GROUP BY recipe_id
        "#,
        group_id,
        VARIETY_WINDOW_DAYS,
        today(),
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.recipe_id, r.last_eaten_on))
    .collect();
    let recent_meals: HashMap<Uuid, NaiveDate> = recent_meals.into_iter().collect();

    let mut ingredients_by_recipe: HashMap<Uuid, Vec<&IngredientRow>> = HashMap::new();
    for ing in &ingredient_rows {
        ingredients_by_recipe
            .entry(ing.recipe_id)
            .or_default()
            .push(ing);
    }

    // The month of today in Paris, like the variety window: on the first
    // day of a month, the UTC month is still the previous one until 1–2 a.m.
    let current_month = today().month() as i32;

    let suggestions: Vec<RecipeSuggestion> = recipe_rows
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
    Ok(suggestions)
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

/// Whether some article in stock covers a recipe's ingredient: by name
/// (trimmed, case-insensitive) always, and also by category when both sides
/// have one ([`Taxonomy::satisfies`]).
fn in_stock(ingredient_name: &str, ingredient_tag: Option<TagId>, stock: &[StockEntry]) -> bool {
    let name = ingredient_name.trim().to_lowercase();
    stock
        .iter()
        .any(|item| item.name == name || ingredient_tag.is_some_and(|tag| item.tags.contains(&tag)))
}

/// Whether a recipe can be cooked from the stock (#539): it names at least
/// one required ingredient, and the stock covers every one of them.
/// Optional ingredients never count. A recipe with no required ingredient
/// says nothing of what it takes from the stock, so it is not « possible ».
fn is_possible(matched: usize, total_required: usize) -> bool {
    total_required > 0 && matched == total_required
}

/// How many recipes a planned selection is offered in its place (#815).
pub(crate) const REPLACEMENTS_MAX: usize = 3;

/// The recipes offered in place of a planned one (#815): neither the
/// replaced recipe nor a recipe already planned, the best score first;
/// equal scores by name, then id, so that the offer does not depend on the
/// order the rows came in. At most [`REPLACEMENTS_MAX`], none when no
/// other recipe is left.
pub(crate) fn replacement_candidates(
    ranked: Vec<RecipeSuggestion>,
    replaced: Uuid,
    planned: &HashSet<Uuid>,
) -> Vec<RecipeSuggestion> {
    let mut candidates: Vec<RecipeSuggestion> = ranked
        .into_iter()
        .filter(|s| s.recipe_id != replaced && !planned.contains(&s.recipe_id))
        .collect();
    candidates.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.recipe_id.cmp(&b.recipe_id))
    });
    candidates.truncate(REPLACEMENTS_MAX);
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- replacement_candidates (#815) -------------------------------------

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn suggestion(n: u128, name: &str, score: f64) -> RecipeSuggestion {
        RecipeSuggestion {
            recipe_id: id(n),
            name: name.to_string(),
            score,
            matched_ingredients: 0,
            total_required_ingredients: 0,
            missing_ingredients: vec![],
            recently_eaten: false,
            last_eaten_on: None,
        }
    }

    fn offered(ranked: Vec<RecipeSuggestion>, replaced: u128, planned: &[u128]) -> Vec<u128> {
        let planned: HashSet<Uuid> = planned.iter().copied().map(id).collect();
        replacement_candidates(ranked, id(replaced), &planned)
            .into_iter()
            .map(|s| s.recipe_id.as_u128())
            .collect()
    }

    #[test]
    fn neither_the_replaced_recipe_nor_a_planned_one_is_offered() {
        let ranked = vec![
            suggestion(1, "Gratin", 100.0),
            suggestion(2, "Soupe", 90.0),
            suggestion(3, "Tarte", 80.0),
            suggestion(4, "Quiche", 70.0),
        ];
        // 1 is replaced, 1 and 3 are planned.
        assert_eq!(offered(ranked, 1, &[1, 3]), [2, 4]);
    }

    #[test]
    fn the_best_score_comes_first_whatever_the_order_given() {
        let ranked = vec![
            suggestion(1, "Gratin", 10.0),
            suggestion(2, "Soupe", 95.0),
            suggestion(3, "Tarte", -20.0),
            suggestion(4, "Quiche", 50.0),
        ];
        assert_eq!(offered(ranked, 9, &[9]), [2, 4, 1]);
    }

    #[test]
    fn equal_scores_go_by_name_then_id() {
        let ranked = vec![
            suggestion(3, "Tarte", 60.0),
            suggestion(5, "Gratin", 60.0),
            suggestion(4, "Gratin", 60.0),
        ];
        assert_eq!(offered(ranked, 9, &[9]), [4, 5, 3]);
    }

    #[test]
    fn at_most_three_are_offered() {
        let ranked: Vec<_> = (1..=6)
            .map(|n| suggestion(n, &format!("R{n}"), 100.0 - n as f64))
            .collect();
        assert_eq!(offered(ranked, 9, &[9]), [1, 2, 3]);
        assert_eq!(REPLACEMENTS_MAX, 3);
    }

    #[test]
    fn fewer_or_none_when_few_recipes_are_left() {
        let ranked = vec![suggestion(1, "Gratin", 50.0), suggestion(2, "Soupe", 40.0)];
        assert_eq!(offered(ranked, 1, &[1]), [2]);
        let ranked = vec![suggestion(1, "Gratin", 50.0), suggestion(2, "Soupe", 40.0)];
        assert!(offered(ranked, 1, &[1, 2]).is_empty());
        assert!(offered(vec![], 1, &[1]).is_empty());
    }

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
    fn category_or_name_when_both_sides_have_a_category() {
        // Categories that diverge, names equal: the name the family typed
        // wins (arbitrage of 2026-10-09: the name stays a fallback even
        // when both sides have a category). Guards against a return to a
        // category-only rule.
        let stock = [entry("Lait", &["en:oat-based-drinks"])];
        assert!(covered("lait", &stock));
        // Category satisfied, names different: the category covers it.
        let stock = [entry("Lait demi-écrémé Lactel", &["en:semi-skimmed-milks"])];
        assert!(covered("lait", &stock));
        // Categories that diverge, names different: no match.
        let stock = [entry("Boisson avoine", &["en:oat-based-drinks"])];
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
    fn possible_when_every_required_ingredient_is_in_stock() {
        assert!(is_possible(2, 2));
        assert!(is_possible(1, 1));
    }

    #[test]
    fn not_possible_when_a_required_ingredient_is_missing() {
        assert!(!is_possible(1, 2));
        assert!(!is_possible(0, 3));
    }

    #[test]
    fn not_possible_without_a_required_ingredient() {
        // Nothing to take from the stock: the recipe says nothing about
        // what it needs, so it is not offered as cookable from it.
        assert!(!is_possible(0, 0));
    }

    #[test]
    fn nothing_in_stock_covers_nothing() {
        assert!(!covered("lait", &[]));
    }
}
