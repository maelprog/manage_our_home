use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::stocks::can_modify;
use crate::AppState;

#[derive(Deserialize)]
pub struct CreateStockItemRequest {
    pub name: String,
    pub category: Option<String>,
    #[serde(default)]
    pub quantity: f64,
    #[serde(default = "default_unit")]
    pub unit: String,
    pub reorder_threshold: Option<f64>,
}

fn default_unit() -> String {
    "unit".to_string()
}

/// Serde's blanket `Option<T>` impl treats an explicit JSON `null` the same
/// as a missing key (both call `visit_none`), so a naive `Option<Option<T>>`
/// field can never observe `Some(None)` — `{"field": null}` deserializes to
/// `None` just like an absent field. Forcing the value through this
/// `deserialize_with` skips that blanket impl: it only runs when the key is
/// present, deserializes the inner `Option<T>` (which *does* turn `null`
/// into `None`), and wraps the result in `Some`, so `Some(None)` becomes
/// reachable again. See https://github.com/serde-rs/serde/issues/984.
fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
pub struct UpdateStockItemRequest {
    pub name: Option<String>,
    /// `Some(None)` clears the category; `None` leaves it untouched.
    #[serde(default, deserialize_with = "deserialize_some")]
    pub category: Option<Option<String>>,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    /// `Some(None)` clears the threshold; `None` leaves it untouched.
    #[serde(default, deserialize_with = "deserialize_some")]
    pub reorder_threshold: Option<Option<f64>>,
}

#[derive(Serialize)]
pub struct StockItemResponse {
    pub id: Uuid,
    pub group_id: Uuid,
    pub created_by: Uuid,
    pub name: String,
    pub category: Option<String>,
    pub quantity: f64,
    pub unit: String,
    pub reorder_threshold: Option<f64>,
    pub low_stock: bool,
}

struct StockItemRow {
    id: Uuid,
    group_id: Uuid,
    created_by: Uuid,
    name: String,
    category: Option<String>,
    quantity: f64,
    unit: String,
    reorder_threshold: Option<f64>,
}

impl From<StockItemRow> for StockItemResponse {
    fn from(r: StockItemRow) -> Self {
        let low_stock = r
            .reorder_threshold
            .map(|t| r.quantity <= t)
            .unwrap_or(false);
        StockItemResponse {
            id: r.id,
            group_id: r.group_id,
            created_by: r.created_by,
            name: r.name,
            category: r.category,
            quantity: r.quantity,
            unit: r.unit,
            reorder_threshold: r.reorder_threshold,
            low_stock,
        }
    }
}

fn validate_request(quantity: f64, reorder_threshold: Option<f64>) -> AppResult<()> {
    if quantity < 0.0 {
        return Err(AppError::BadRequest("quantity_must_be_non_negative".into()));
    }
    if let Some(t) = reorder_threshold {
        if t < 0.0 {
            return Err(AppError::BadRequest(
                "reorder_threshold_must_be_non_negative".into(),
            ));
        }
    }
    Ok(())
}

pub async fn create_stock_item(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<CreateStockItemRequest>,
) -> AppResult<impl IntoResponse> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("name_required".into()));
    }
    if body.unit.trim().is_empty() {
        return Err(AppError::BadRequest("unit_required".into()));
    }
    validate_request(body.quantity, body.reorder_threshold)?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let item = sqlx::query_as!(
        StockItemRow,
        r#"
        INSERT INTO stock_items (group_id, created_by, name, category, quantity, unit, reorder_threshold)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, group_id, created_by, name, category, quantity, unit, reorder_threshold
        "#,
        group_id,
        auth.user_id,
        name,
        body.category,
        body.quantity,
        body.unit,
        body.reorder_threshold,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(StockItemResponse::from(item))))
}

pub async fn get_stock_item(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, item_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let item = sqlx::query_as!(
        StockItemRow,
        r#"SELECT id, group_id, created_by, name, category, quantity, unit, reorder_threshold
           FROM stock_items WHERE id = $1 AND group_id = $2"#,
        item_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    tx.commit().await?;

    Ok(Json(StockItemResponse::from(item)))
}

#[derive(Deserialize)]
pub struct ListStockItemsQuery {
    /// When true, only items at or below their reorder threshold are
    /// returned (used by the future Recipes/Grocery-list epics to compute
    /// "what's missing").
    #[serde(default)]
    pub low_stock: bool,
}

pub async fn list_stock_items(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Query(query): Query<ListStockItemsQuery>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let rows = sqlx::query_as!(
        StockItemRow,
        r#"
        SELECT id, group_id, created_by, name, category, quantity, unit, reorder_threshold
        FROM stock_items
        WHERE group_id = $1
        ORDER BY name
        "#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let items: Vec<StockItemResponse> = rows
        .into_iter()
        .map(StockItemResponse::from)
        .filter(|item| !query.low_stock || item.low_stock)
        .collect();

    Ok(Json(json!({ "items": items })))
}

pub async fn update_stock_item(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, item_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateStockItemRequest>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;

    // FOR UPDATE locks the row for the rest of this transaction, so a
    // concurrent PATCH on the same item blocks on this SELECT until we
    // commit instead of both transactions reading the same stale quantity
    // and one overwriting the other's write (lost update).
    let existing = sqlx::query!(
        "SELECT created_by, name, category, quantity, unit, reorder_threshold FROM stock_items WHERE id = $1 AND group_id = $2 FOR UPDATE",
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
    let unit = match body.unit.as_deref().map(str::trim) {
        Some("") => return Err(AppError::BadRequest("unit_required".into())),
        Some(u) => Some(u.to_string()),
        None => None,
    };
    let category = match body.category {
        Some(c) => c,
        None => existing.category,
    };

    let quantity = body.quantity.unwrap_or(existing.quantity);
    let reorder_threshold = match body.reorder_threshold {
        Some(t) => t,
        None => existing.reorder_threshold,
    };
    validate_request(quantity, reorder_threshold)?;

    let item = sqlx::query_as!(
        StockItemRow,
        r#"
        UPDATE stock_items SET
            name = COALESCE($3, name),
            category = $4,
            quantity = $5,
            unit = COALESCE($6, unit),
            reorder_threshold = $7,
            updated_at = now()
        WHERE id = $1 AND group_id = $2
        RETURNING id, group_id, created_by, name, category, quantity, unit, reorder_threshold
        "#,
        item_id,
        group_id,
        name,
        category,
        quantity,
        unit,
        reorder_threshold,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(StockItemResponse::from(item)))
}

pub async fn delete_stock_item(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, item_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;

    let existing = sqlx::query!(
        "SELECT created_by FROM stock_items WHERE id = $1 AND group_id = $2",
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
        "DELETE FROM stock_items WHERE id = $1 AND group_id = $2",
        item_id,
        group_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}
