use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::recipes::can_modify;
use crate::AppState;

#[derive(Deserialize)]
pub struct IngredientInput {
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    #[serde(default)]
    pub is_optional: bool,
    pub seasonal_months: Option<Vec<i32>>,
}

#[derive(Deserialize)]
pub struct CreateRecipeRequest {
    pub name: String,
    pub instructions: Option<String>,
    #[serde(default)]
    pub ingredients: Vec<IngredientInput>,
}

/// Serde's blanket `Option<T>` impl treats an explicit JSON `null` the same
/// as a missing key, so a naive `Option<Option<T>>` field can never observe
/// `Some(None)` (see stocks/items.rs for the same pattern/link).
fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
pub struct UpdateRecipeRequest {
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub instructions: Option<Option<String>>,
    /// When present, replaces the recipe's full ingredient list.
    pub ingredients: Option<Vec<IngredientInput>>,
}

#[derive(Serialize)]
pub struct IngredientResponse {
    pub id: Uuid,
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    pub is_optional: bool,
    pub seasonal_months: Option<Vec<i32>>,
}

#[derive(Serialize)]
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

struct RecipeRow {
    id: Uuid,
    group_id: Uuid,
    created_by: Uuid,
    name: String,
    instructions: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

struct IngredientRow {
    id: Uuid,
    recipe_id: Uuid,
    name: String,
    quantity: Option<f64>,
    unit: Option<String>,
    is_optional: bool,
    seasonal_months: Option<Vec<i32>>,
}

impl From<IngredientRow> for IngredientResponse {
    fn from(r: IngredientRow) -> Self {
        IngredientResponse {
            id: r.id,
            name: r.name,
            quantity: r.quantity,
            unit: r.unit,
            is_optional: r.is_optional,
            seasonal_months: r.seasonal_months,
        }
    }
}

fn validate_ingredients(ingredients: &[IngredientInput]) -> AppResult<()> {
    for ing in ingredients {
        if ing.name.trim().is_empty() {
            return Err(AppError::BadRequest("ingredient_name_required".into()));
        }
        if let Some(q) = ing.quantity {
            if q < 0.0 {
                return Err(AppError::BadRequest(
                    "ingredient_quantity_must_be_non_negative".into(),
                ));
            }
        }
        if let Some(months) = &ing.seasonal_months {
            if months.iter().any(|m| !(1..=12).contains(m)) {
                return Err(AppError::BadRequest("invalid_seasonal_month".into()));
            }
        }
    }
    Ok(())
}

async fn insert_ingredients(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    recipe_id: Uuid,
    group_id: Uuid,
    ingredients: &[IngredientInput],
) -> AppResult<()> {
    for ing in ingredients {
        sqlx::query!(
            r#"
            INSERT INTO recipe_ingredients (recipe_id, group_id, name, quantity, unit, is_optional, seasonal_months)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
            recipe_id,
            group_id,
            ing.name.trim(),
            ing.quantity,
            ing.unit.as_deref(),
            ing.is_optional,
            ing.seasonal_months.as_deref(),
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn fetch_ingredients(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    recipe_id: Uuid,
) -> AppResult<Vec<IngredientResponse>> {
    let rows = sqlx::query_as!(
        IngredientRow,
        r#"
        SELECT id, recipe_id, name, quantity, unit, is_optional, seasonal_months
        FROM recipe_ingredients
        WHERE recipe_id = $1
        ORDER BY name
        "#,
        recipe_id,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.into_iter().map(IngredientResponse::from).collect())
}

pub async fn create_recipe(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<CreateRecipeRequest>,
) -> AppResult<impl IntoResponse> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("name_required".into()));
    }
    validate_ingredients(&body.ingredients)?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let recipe = sqlx::query_as!(
        RecipeRow,
        r#"
        INSERT INTO recipes (group_id, created_by, name, instructions)
        VALUES ($1, $2, $3, $4)
        RETURNING id, group_id, created_by, name, instructions, created_at, updated_at
        "#,
        group_id,
        auth.user_id,
        name,
        body.instructions,
    )
    .fetch_one(&mut *tx)
    .await?;

    insert_ingredients(&mut tx, recipe.id, group_id, &body.ingredients).await?;
    let ingredients = fetch_ingredients(&mut tx, recipe.id).await?;
    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(RecipeResponse {
            id: recipe.id,
            group_id: recipe.group_id,
            created_by: recipe.created_by,
            name: recipe.name,
            instructions: recipe.instructions,
            ingredients,
            created_at: recipe.created_at,
            updated_at: recipe.updated_at,
        }),
    ))
}

pub async fn get_recipe(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let recipe = sqlx::query_as!(
        RecipeRow,
        r#"SELECT id, group_id, created_by, name, instructions, created_at, updated_at
           FROM recipes WHERE id = $1 AND group_id = $2"#,
        recipe_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    let ingredients = fetch_ingredients(&mut tx, recipe.id).await?;
    tx.commit().await?;

    Ok(Json(RecipeResponse {
        id: recipe.id,
        group_id: recipe.group_id,
        created_by: recipe.created_by,
        name: recipe.name,
        instructions: recipe.instructions,
        ingredients,
        created_at: recipe.created_at,
        updated_at: recipe.updated_at,
    }))
}

pub async fn list_recipes(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let recipe_rows = sqlx::query_as!(
        RecipeRow,
        r#"
        SELECT id, group_id, created_by, name, instructions, created_at, updated_at
        FROM recipes
        WHERE group_id = $1
        ORDER BY name
        "#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;

    let ingredient_rows = sqlx::query_as!(
        IngredientRow,
        r#"
        SELECT id, recipe_id, name, quantity, unit, is_optional, seasonal_months
        FROM recipe_ingredients
        WHERE group_id = $1
        ORDER BY name
        "#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut recipes: Vec<RecipeResponse> = recipe_rows
        .into_iter()
        .map(|r| RecipeResponse {
            id: r.id,
            group_id: r.group_id,
            created_by: r.created_by,
            name: r.name,
            instructions: r.instructions,
            ingredients: Vec::new(),
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
        .collect();

    for ing in ingredient_rows {
        if let Some(recipe) = recipes.iter_mut().find(|r| r.id == ing.recipe_id) {
            recipe.ingredients.push(IngredientResponse::from(ing));
        }
    }

    Ok(Json(json!({ "recipes": recipes })))
}

pub async fn update_recipe(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateRecipeRequest>,
) -> AppResult<impl IntoResponse> {
    if let Some(ingredients) = &body.ingredients {
        validate_ingredients(ingredients)?;
    }

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;

    // FOR UPDATE locks the row for the rest of this transaction, mirroring
    // stocks/items.rs's concurrent-write protection.
    let existing = sqlx::query!(
        "SELECT created_by, name, instructions FROM recipes WHERE id = $1 AND group_id = $2 FOR UPDATE",
        recipe_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    if !can_modify(&actor_role, existing.created_by == auth.user_id) {
        return Err(AppError::Forbidden);
    }

    let name = match body.name.as_deref().map(str::trim) {
        Some("") => return Err(AppError::BadRequest("name_required".into())),
        Some(n) => Some(n.to_string()),
        None => None,
    };
    let instructions = match body.instructions {
        Some(i) => i,
        None => existing.instructions,
    };

    let recipe = sqlx::query_as!(
        RecipeRow,
        r#"
        UPDATE recipes SET
            name = COALESCE($3, name),
            instructions = $4,
            updated_at = now()
        WHERE id = $1 AND group_id = $2
        RETURNING id, group_id, created_by, name, instructions, created_at, updated_at
        "#,
        recipe_id,
        group_id,
        name,
        instructions,
    )
    .fetch_one(&mut *tx)
    .await?;

    if let Some(ingredients) = &body.ingredients {
        sqlx::query!(
            "DELETE FROM recipe_ingredients WHERE recipe_id = $1",
            recipe_id
        )
        .execute(&mut *tx)
        .await?;
        insert_ingredients(&mut tx, recipe_id, group_id, ingredients).await?;
    }

    let ingredients = fetch_ingredients(&mut tx, recipe_id).await?;
    tx.commit().await?;

    Ok(Json(RecipeResponse {
        id: recipe.id,
        group_id: recipe.group_id,
        created_by: recipe.created_by,
        name: recipe.name,
        instructions: recipe.instructions,
        ingredients,
        created_at: recipe.created_at,
        updated_at: recipe.updated_at,
    }))
}

pub async fn delete_recipe(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;

    let existing = sqlx::query!(
        "SELECT created_by FROM recipes WHERE id = $1 AND group_id = $2",
        recipe_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    if !can_modify(&actor_role, existing.created_by == auth.user_id) {
        return Err(AppError::Forbidden);
    }

    sqlx::query!(
        "DELETE FROM recipes WHERE id = $1 AND group_id = $2",
        recipe_id,
        group_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}
