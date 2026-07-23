//! Request/response shapes for the Recipes endpoints
//! (`apps/api/src/recipes/`), consumed by `apps/web`'s SSR client. Kept
//! field-for-field identical to `apps/api`'s wire structs so there is one
//! documented shape; only the fields `apps/web` needs are declared (serde
//! ignores extras on deserialize). The backend is *not* modified by this
//! epic — these mirror it, they don't replace it.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One ingredient on a create/update request. Mirrors
/// `apps/api/src/recipes/crud.rs::IngredientInput`. `quantity`/`unit`/
/// `seasonal_months` are omitted from the wire when `None`; `is_optional`
/// is always sent (the backend defaults a missing key to `false`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngredientInput {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default)]
    pub is_optional: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seasonal_months: Option<Vec<i32>>,
}

/// `POST /groups/:id/recipes` request body. Mirrors `CreateRecipeRequest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRecipeRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default)]
    pub ingredients: Vec<IngredientInput>,
}

/// Serde's blanket `Option<T>` impl collapses an explicit `null` and a
/// missing key to the same `None`, so a naive `Option<Option<T>>` can never
/// observe `Some(None)`. This `deserialize_with` only runs when the key is
/// present, making `Some(None)` reachable — matching the backend's identical
/// helper. See <https://github.com/serde-rs/serde/issues/984> and the twin
/// in `dto/stocks.rs`.
fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// `PATCH /groups/:id/recipes/:recipe_id` request body. Mirrors
/// `UpdateRecipeRequest`. For `instructions` the outer `Option`
/// distinguishes "leave untouched" (`None`, omitted from the wire) from
/// "clear" (`Some(None)`, sent as `null`) from "set" (`Some(Some(v))`). A
/// present `ingredients` replaces the recipe's full ingredient list; an
/// absent one leaves the ingredients untouched.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateRecipeRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_some",
        skip_serializing_if = "Option::is_none"
    )]
    pub instructions: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ingredients: Option<Vec<IngredientInput>>,
}

/// One ingredient on a recipe response. Mirrors `IngredientResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngredientResponse {
    pub id: Uuid,
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    pub is_optional: bool,
    pub seasonal_months: Option<Vec<i32>>,
}

/// `GET/POST/PATCH /groups/:id/recipes[/:recipe_id]` response body. Mirrors
/// `RecipeResponse`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeResponse {
    pub id: Uuid,
    pub group_id: Uuid,
    pub created_by: Uuid,
    pub name: String,
    pub instructions: Option<String>,
    pub ingredients: Vec<IngredientResponse>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `GET /groups/:id/recipes` response envelope (`{ "recipes": [...] }`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeList {
    pub recipes: Vec<RecipeResponse>,
}

/// A required ingredient a suggested recipe is missing from stock. Mirrors
/// `MissingIngredient` — the exact shape the grocery-list generate endpoint
/// (F6) consumes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissingIngredient {
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
}

/// One scored recipe from the suggestion endpoint. Mirrors
/// `RecipeSuggestion`. `score` is the raw internal heuristic — the UI uses
/// the *order* and the derived signals below, never the number itself (see
/// `docs/front-epic-5-recipes.md`).
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// `GET /groups/:id/recipes/suggestions` response envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestionList {
    pub suggestions: Vec<RecipeSuggestion>,
}

/// `POST /groups/:id/recipes/:recipe_id/meal-history` request body. Mirrors
/// `LogMealRequest`. An omitted `eaten_on` defaults to today server-side.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogMealRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eaten_on: Option<NaiveDate>,
}

/// One `meal_history` entry. Mirrors `MealHistoryEntryResponse`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MealHistoryEntryResponse {
    pub id: Uuid,
    pub recipe_id: Uuid,
    pub eaten_on: NaiveDate,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
}

/// `GET /groups/:id/recipes/meal-history` response envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MealHistoryList {
    pub entries: Vec<MealHistoryEntryResponse>,
}
