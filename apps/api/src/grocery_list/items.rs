use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Utc};
use manage_our_home_shared::validation::grocery_list::normalize_unit;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::grocery_list::can_modify;
use crate::grocery_list::planned::{
    count_against_list, count_missing_against_list, plan_lines, ListChange, ListedLine,
    PlannedIngredient,
};
use crate::groups::require_role;
use crate::notifications::center;
use crate::recipes::availability::planned_availability;
use crate::stocks::items::today;
use crate::AppState;

#[derive(Deserialize, utoipa::ToSchema)]
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

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpdateGroceryItemRequest {
    pub name: Option<String>,
    /// `Some(None)` clears the quantity; `None` leaves it untouched.
    #[serde(default, deserialize_with = "deserialize_some")]
    pub quantity: Option<Option<f64>>,
    /// `Some(None)` clears the unit, unless a quantity remains: then it is
    /// « unité » (#458). `None` leaves it untouched.
    #[serde(default, deserialize_with = "deserialize_some")]
    pub unit: Option<Option<String>>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CheckGroceryItemRequest {
    pub checked: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
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

#[utoipa::path(
    post,
    path = "/groups/{id}/grocery-items",
    tag = "grocery-list",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
    ),
    request_body = CreateGroceryItemRequest,
    responses(
        (status = 201, description = "Item added", body = GroceryItemResponse),
        (status = 400, description = "`name_required` or `quantity_must_be_non_negative`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
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
    // A quantity typed without a unit gets the default one (#458).
    let unit = normalize_unit(body.quantity, body.unit.as_deref());

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
        unit,
    )
    .fetch_one(&mut *tx)
    .await?;
    // The other members' notification centre (#534). An item typed by
    // hand only: the lines `generate_grocery_items` writes from the
    // planned recipes are a recomputation, not news.
    center::insert_for_members(
        &mut tx,
        &center::NewNotification {
            group_id,
            event: center::Event::GroceryItem {
                author: &auth.display_name,
                item: &item.name,
            },
            object_id: Some(item.id),
            actor_id: Some(auth.user_id),
            dedup_key: None,
        },
    )
    .await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(GroceryItemResponse::from(item))))
}

#[utoipa::path(
    get,
    path = "/groups/{id}/grocery-items/{item_id}",
    tag = "grocery-list",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("item_id" = uuid::Uuid, Path, description = "Item id"),
    ),
    responses(
        (status = 200, description = "The item", body = GroceryItemResponse),
        (status = 404, description = "`not_found`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
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

#[utoipa::path(
    get,
    path = "/groups/{id}/grocery-items",
    tag = "grocery-list",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
    ),
    responses(
        (status = 200, description = "The grocery list", body = crate::api_docs::schemas::GroceryItemList),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
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
#[utoipa::path(
    post,
    path = "/groups/{id}/grocery-items/{item_id}/check",
    tag = "grocery-list",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("item_id" = uuid::Uuid, Path, description = "Item id"),
    ),
    request_body = CheckGroceryItemRequest,
    responses(
        (status = 200, description = "Item checked or unchecked", body = GroceryItemResponse),
        (status = 404, description = "`not_found`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
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

#[utoipa::path(
    patch,
    path = "/groups/{id}/grocery-items/{item_id}",
    tag = "grocery-list",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("item_id" = uuid::Uuid, Path, description = "Item id"),
    ),
    request_body = UpdateGroceryItemRequest,
    responses(
        (status = 200, description = "Item updated", body = GroceryItemResponse),
        (status = 400, description = "`name_required` or `quantity_must_be_non_negative`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 404, description = "`not_found`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
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
    // Normalized on the values the row ends up with (#458): a quantity left
    // without a unit, whichever field the PATCH touched, gets the default one.
    let unit = match body.unit {
        Some(u) => u,
        None => existing.unit,
    };
    let unit = normalize_unit(quantity, unit.as_deref());

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

#[utoipa::path(
    delete,
    path = "/groups/{id}/grocery-items/{item_id}",
    tag = "grocery-list",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("item_id" = uuid::Uuid, Path, description = "Item id"),
    ),
    responses(
        (status = 204, description = "Item deleted"),
        (status = 404, description = "`not_found`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
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

struct LowStockRow {
    name: String,
}

/// Generates new grocery items from the two automated sources named in
/// docs/v1-scope.md epic #5: the required, not-in-stock ingredients of the
/// recipes planned for the next shopping (#470), and stock items at/below
/// their reorder threshold. A recipe counts only while it has a `planned`
/// selection (`recipe_selections`, #469); its quantities are scaled to the
/// selection's number of people, and an ingredient several planned recipes
/// share makes one line (`planned::plan_lines`). Each recipe line names the
/// first recipe that calls for it in `source_recipe_id`. Matching against
/// existing rows is name-based (case-insensitive, trimmed), same heuristic
/// as recipes::suggestions and stocks' low_stock — an item already present
/// and unchecked is not duplicated; a recipe line of the same unit that
/// counts less than the planned recipes now call for is raised to it, never
/// lowered (`planned::count_against_list`, #842), so re-running this is
/// idempotent. Only the items created are returned, not the raised ones.
/// Any group member may trigger it (same bar as manual add).
#[utoipa::path(
    post,
    path = "/groups/{id}/grocery-items/generate",
    tag = "grocery-list",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
    ),
    responses(
        (status = 201, description = "Every planned recipe's missing ingredients, added to the list", body = crate::api_docs::schemas::GroceryItemList),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
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

    // Oldest selection first, so the "first recipe" a shared ingredient is
    // attributed to is the one planned first.
    let planned_ingredients = sqlx::query_as!(
        PlannedIngredient,
        r#"
        SELECT s.recipe_id, ri.name, ri.quantity, ri.unit,
               r.servings AS recipe_servings, s.servings AS selection_servings
        FROM recipe_selections s
        JOIN recipes r ON r.id = s.recipe_id
        JOIN recipe_ingredients ri ON ri.recipe_id = s.recipe_id
        WHERE s.group_id = $1 AND s.status = 'planned' AND ri.is_optional = false
        ORDER BY s.created_at, s.id, ri.name, ri.id
        "#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .filter(|i| !stock_names.contains(&i.name.trim().to_lowercase()));
    let missing_lines = plan_lines(planned_ingredients);

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

    let listed = listed_lines(&mut tx, group_id).await?;
    // A name already on the list, or that a planned recipe calls for, gets
    // no low-stock line.
    let mut seen: HashSet<String> = listed
        .iter()
        .map(|l| l.name.trim().to_lowercase())
        .chain(missing_lines.iter().map(|l| l.name.trim().to_lowercase()))
        .collect();
    let mut created = apply_list_changes(
        &mut tx,
        group_id,
        auth.user_id,
        count_against_list(missing_lines, &listed),
    )
    .await?;

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

/// `POST /groups/:id/recipes/:recipe_id/selection/missing` (#559): adds to
/// the grocery list what the stock lacks of the recipe's `planned`
/// selection (`recipes::availability::missing`), one item per element,
/// with what this recipe lacks of it and the recipe in `source_recipe_id`.
/// A name already on the list unchecked is not added again, as for
/// `generate_grocery_items`, but a recipe line of it is raised as there to
/// what the planned recipes lack of it together (`add_missing_lines`), so
/// replaying changes nothing. 404 without a
/// `planned` selection of that recipe in the group. Any member may, as for
/// the generation. 201 with the items created.
#[utoipa::path(
    post,
    path = "/groups/{id}/recipes/{recipe_id}/selection/missing",
    tag = "grocery-list",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("recipe_id" = uuid::Uuid, Path, description = "Recipe id"),
    ),
    responses(
        (status = 201, description = "The planned recipe's missing ingredients, added to the list", body = crate::api_docs::schemas::GroceryItemList),
        (status = 404, description = "`not_found`: no planned selection", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn add_missing_grocery_items(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    sqlx::query_scalar!(
        "SELECT id FROM recipe_selections WHERE recipe_id = $1 AND group_id = $2 AND status = 'planned'",
        recipe_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    let created = add_missing_lines(&mut tx, group_id, auth.user_id, recipe_id).await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(json!({ "items": created }))))
}

/// Adds to the grocery list what the stock lacks of the recipe's `planned`
/// selection, a name already on the list unchecked excepted; the items
/// created. A recipe line of such a name is counted, as by the generation
/// (`planned::count_against_list`, #842), against what every planned
/// recipe lacks of it, each weighed alone against the stock
/// (`planned::with_other_planned`): the line another recipe put there is
/// raised by this recipe's share. A line created carries only what this
/// recipe lacks (`planned::count_missing_against_list`). Shared by `add_missing_grocery_items` and
/// the replacement of a selection (#815).
pub(crate) async fn add_missing_lines(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    user_id: Uuid,
    recipe_id: Uuid,
) -> AppResult<Vec<GroceryItemResponse>> {
    let missing = planned_availability(tx, group_id, None, today())
        .await?
        .into_iter()
        .flat_map(|a| a.missing);
    let listed = listed_lines(tx, group_id).await?;
    let changes = count_missing_against_list(recipe_id, missing, &listed);
    apply_list_changes(tx, group_id, user_id, changes).await
}

/// The unchecked lines of the group's grocery list, oldest first.
async fn listed_lines(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
) -> AppResult<Vec<ListedLine>> {
    Ok(sqlx::query_as!(
        ListedLine,
        r#"
        SELECT id, name, quantity, unit, source FROM grocery_items
        WHERE group_id = $1 AND checked = false
        ORDER BY created_at, id
        "#,
        group_id,
    )
    .fetch_all(&mut **tx)
    .await?)
}

/// Writes what `count_against_list` decided: each new line is created as a
/// recipe line, each raised one takes its new quantity. The items created.
async fn apply_list_changes(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    user_id: Uuid,
    changes: Vec<ListChange>,
) -> AppResult<Vec<GroceryItemResponse>> {
    let mut created: Vec<GroceryItemResponse> = Vec::new();
    for change in changes {
        let line = match change {
            ListChange::Add(line) => line,
            ListChange::Raise { id, quantity } => {
                sqlx::query!(
                    "UPDATE grocery_items SET quantity = $3, updated_at = now() WHERE id = $1 AND group_id = $2",
                    id,
                    group_id,
                    quantity,
                )
                .execute(&mut **tx)
                .await?;
                continue;
            }
        };
        let item = sqlx::query_as!(
            GroceryItemRow,
            r#"
            INSERT INTO grocery_items (group_id, created_by, name, quantity, unit, source, source_recipe_id)
            VALUES ($1, $2, $3, $4, $5, 'recipe', $6)
            RETURNING id, group_id, created_by, name, quantity, unit, checked, source, source_recipe_id, created_at, updated_at
            "#,
            group_id,
            user_id,
            line.name,
            line.quantity,
            line.unit,
            line.recipe_id,
        )
        .fetch_one(&mut **tx)
        .await?;
        created.push(GroceryItemResponse::from(item));
    }
    Ok(created)
}
