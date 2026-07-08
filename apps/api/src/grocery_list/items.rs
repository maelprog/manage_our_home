use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::grocery_list::can_modify;
use crate::groups::require_role;
use crate::AppState;

#[derive(Deserialize)]
pub struct CreateGroceryItemRequest {
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
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
pub struct UpdateGroceryItemRequest {
    pub name: Option<String>,
    /// `Some(None)` clears the quantity; `None` leaves it untouched.
    #[serde(default, deserialize_with = "deserialize_some")]
    pub quantity: Option<Option<f64>>,
    /// `Some(None)` clears the unit; `None` leaves it untouched.
    #[serde(default, deserialize_with = "deserialize_some")]
    pub unit: Option<Option<String>>,
}

#[derive(Deserialize)]
pub struct CheckGroceryItemRequest {
    pub checked: bool,
}

#[derive(Serialize)]
pub struct GroceryItemResponse {
    pub id: Uuid,
    pub group_id: Uuid,
    pub created_by: Uuid,
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    pub checked: bool,
    pub source: String,
    pub source_recipe_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

struct GroceryItemRow {
    id: Uuid,
    group_id: Uuid,
    created_by: Uuid,
    name: String,
    quantity: Option<f64>,
    unit: Option<String>,
    checked: bool,
    source: String,
    source_recipe_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<GroceryItemRow> for GroceryItemResponse {
    fn from(r: GroceryItemRow) -> Self {
        GroceryItemResponse {
            id: r.id,
            group_id: r.group_id,
            created_by: r.created_by,
            name: r.name,
            quantity: r.quantity,
            unit: r.unit,
            checked: r.checked,
            source: r.source,
            source_recipe_id: r.source_recipe_id,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

fn validate_quantity(quantity: Option<f64>) -> AppResult<()> {
    if let Some(q) = quantity {
        if q < 0.0 {
            return Err(AppError::BadRequest("quantity_must_be_non_negative".into()));
        }
    }
    Ok(())
}

pub async fn create_grocery_item(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<CreateGroceryItemRequest>,
) -> AppResult<impl IntoResponse> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("name_required".into()));
    }
    validate_quantity(body.quantity)?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let item = sqlx::query_as!(
        GroceryItemRow,
        r#"
        INSERT INTO grocery_items (group_id, created_by, name, quantity, unit, source)
        VALUES ($1, $2, $3, $4, $5, 'manual')
        RETURNING id, group_id, created_by, name, quantity, unit, checked, source, source_recipe_id, created_at, updated_at
        "#,
        group_id,
        auth.user_id,
        name,
        body.quantity,
        body.unit,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(GroceryItemResponse::from(item))))
}

pub async fn get_grocery_item(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, item_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let item = sqlx::query_as!(
        GroceryItemRow,
        r#"SELECT id, group_id, created_by, name, quantity, unit, checked, source, source_recipe_id, created_at, updated_at
           FROM grocery_items WHERE id = $1 AND group_id = $2"#,
        item_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    tx.commit().await?;

    Ok(Json(GroceryItemResponse::from(item)))
}

pub async fn list_grocery_items(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let rows = sqlx::query_as!(
        GroceryItemRow,
        r#"
        SELECT id, group_id, created_by, name, quantity, unit, checked, source, source_recipe_id, created_at, updated_at
        FROM grocery_items
        WHERE group_id = $1
        ORDER BY checked, name
        "#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let items: Vec<GroceryItemResponse> = rows.into_iter().map(GroceryItemResponse::from).collect();

    Ok(Json(json!({ "items": items })))
}

/// Any member may check/uncheck an item off the shared list — this is a
/// lighter permission bar than `update_grocery_item`, which only the
/// creator or a group admin/owner may use to edit name/quantity/unit.
pub async fn check_grocery_item(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, item_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<CheckGroceryItemRequest>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let item = sqlx::query_as!(
        GroceryItemRow,
        r#"
        UPDATE grocery_items SET checked = $3, updated_at = now()
        WHERE id = $1 AND group_id = $2
        RETURNING id, group_id, created_by, name, quantity, unit, checked, source, source_recipe_id, created_at, updated_at
        "#,
        item_id,
        group_id,
        body.checked,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    tx.commit().await?;

    Ok(Json(GroceryItemResponse::from(item)))
}

pub async fn update_grocery_item(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, item_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateGroceryItemRequest>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;

    // FOR UPDATE locks the row for the rest of this transaction, mirroring
    // stocks/items.rs's and recipes/crud.rs's concurrent-write protection.
    let existing = sqlx::query!(
        "SELECT created_by, name, quantity, unit FROM grocery_items WHERE id = $1 AND group_id = $2 FOR UPDATE",
        item_id,
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
    let quantity = match body.quantity {
        Some(q) => q,
        None => existing.quantity,
    };
    validate_quantity(quantity)?;
    let unit = match body.unit {
        Some(u) => u,
        None => existing.unit,
    };

    let item = sqlx::query_as!(
        GroceryItemRow,
        r#"
        UPDATE grocery_items SET
            name = COALESCE($3, name),
            quantity = $4,
            unit = $5,
            updated_at = now()
        WHERE id = $1 AND group_id = $2
        RETURNING id, group_id, created_by, name, quantity, unit, checked, source, source_recipe_id, created_at, updated_at
        "#,
        item_id,
        group_id,
        name,
        quantity,
        unit,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(GroceryItemResponse::from(item)))
}

pub async fn delete_grocery_item(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, item_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;

    let existing = sqlx::query!(
        "SELECT created_by FROM grocery_items WHERE id = $1 AND group_id = $2",
        item_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    if !can_modify(&actor_role, existing.created_by == auth.user_id) {
        return Err(AppError::Forbidden);
    }

    sqlx::query!(
        "DELETE FROM grocery_items WHERE id = $1 AND group_id = $2",
        item_id,
        group_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

struct RequiredIngredientRow {
    name: String,
    quantity: Option<f64>,
    unit: Option<String>,
}

struct LowStockRow {
    name: String,
}

/// Generates new grocery items from the two automated sources named in
/// docs/v1-scope.md epic #5: recipes' missing (required, not-in-stock)
/// ingredients, and stock items at/below their reorder threshold. Matching
/// against existing rows is name-based (case-insensitive, trimmed), same
/// heuristic as recipes::suggestions and stocks' low_stock — an item
/// already present and unchecked is not duplicated, so re-running this is
/// idempotent. Any group member may trigger it (same bar as manual add).
pub async fn generate_grocery_items(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let stock_names: HashSet<String> = sqlx::query_scalar!(
        "SELECT name FROM stock_items WHERE group_id = $1 AND quantity > 0",
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|n| n.trim().to_lowercase())
    .collect();

    let missing_ingredients = sqlx::query_as!(
        RequiredIngredientRow,
        r#"
        SELECT ri.name, ri.quantity, ri.unit
        FROM recipe_ingredients ri
        WHERE ri.group_id = $1 AND ri.is_optional = false
        "#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .filter(|i| !stock_names.contains(&i.name.trim().to_lowercase()));

    let low_stock_items = sqlx::query_as!(
        LowStockRow,
        r#"
        SELECT name FROM stock_items
        WHERE group_id = $1 AND reorder_threshold IS NOT NULL AND quantity <= reorder_threshold
        "#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;

    let existing_unchecked: HashSet<String> = sqlx::query_scalar!(
        "SELECT name FROM grocery_items WHERE group_id = $1 AND checked = false",
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|n| n.trim().to_lowercase())
    .collect();

    let mut seen: HashSet<String> = existing_unchecked;
    let mut created: Vec<GroceryItemResponse> = Vec::new();

    for ing in missing_ingredients {
        let key = ing.name.trim().to_lowercase();
        if !seen.insert(key) {
            continue;
        }
        let item = sqlx::query_as!(
            GroceryItemRow,
            r#"
            INSERT INTO grocery_items (group_id, created_by, name, quantity, unit, source)
            VALUES ($1, $2, $3, $4, $5, 'recipe')
            RETURNING id, group_id, created_by, name, quantity, unit, checked, source, source_recipe_id, created_at, updated_at
            "#,
            group_id,
            auth.user_id,
            ing.name.trim(),
            ing.quantity,
            ing.unit,
        )
        .fetch_one(&mut *tx)
        .await?;
        created.push(GroceryItemResponse::from(item));
    }

    for stock in low_stock_items {
        let key = stock.name.trim().to_lowercase();
        if !seen.insert(key) {
            continue;
        }
        let item = sqlx::query_as!(
            GroceryItemRow,
            r#"
            INSERT INTO grocery_items (group_id, created_by, name, source)
            VALUES ($1, $2, $3, 'low_stock')
            RETURNING id, group_id, created_by, name, quantity, unit, checked, source, source_recipe_id, created_at, updated_at
            "#,
            group_id,
            auth.user_id,
            stock.name.trim(),
        )
        .fetch_one(&mut *tx)
        .await?;
        created.push(GroceryItemResponse::from(item));
    }

    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(json!({ "items": created }))))
}
