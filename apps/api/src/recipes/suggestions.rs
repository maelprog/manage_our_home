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
/// Ingredient/stock matching is name-based (case-insensitive, trimmed) —
/// v1 has no shared ingredient taxonomy, so this is a heuristic, not an
/// exact reservation check (unit conversion is out of scope, same as the
/// low_stock heuristic in stocks::items).
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

    let stock_names: HashSet<String> = sqlx::query_scalar!(
        "SELECT name FROM stock_items WHERE group_id = $1 AND quantity > 0",
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|n| n.trim().to_lowercase())
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
            let matched = required
                .iter()
                .filter(|i| stock_names.contains(&i.name.trim().to_lowercase()))
                .count();
            let missing_ingredients: Vec<MissingIngredient> = required
                .iter()
                .filter(|i| !stock_names.contains(&i.name.trim().to_lowercase()))
                .map(|i| MissingIngredient {
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
