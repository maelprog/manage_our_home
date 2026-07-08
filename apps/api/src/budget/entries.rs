use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::budget::can_modify;
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::AppState;

#[derive(Deserialize)]
pub struct CreateBudgetEntryRequest {
    pub name: String,
    pub amount: f64,
    pub spent_at: Option<NaiveDate>,
    pub grocery_item_id: Option<Uuid>,
}

#[derive(Deserialize)]
pub struct UpdateBudgetEntryRequest {
    pub name: Option<String>,
    pub amount: Option<f64>,
    pub spent_at: Option<NaiveDate>,
}

/// Attaches a price to an existing grocery item, e.g. once it's been
/// checked off/bought — the item's name is denormalized onto the entry so
/// it survives the grocery item being deleted later.
#[derive(Deserialize)]
pub struct SetGroceryItemPriceRequest {
    pub amount: f64,
    pub spent_at: Option<NaiveDate>,
}

#[derive(Serialize)]
pub struct BudgetEntryResponse {
    pub id: Uuid,
    pub group_id: Uuid,
    pub created_by: Uuid,
    pub grocery_item_id: Option<Uuid>,
    pub name: String,
    pub amount: f64,
    pub spent_at: NaiveDate,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

struct BudgetEntryRow {
    id: Uuid,
    group_id: Uuid,
    created_by: Uuid,
    grocery_item_id: Option<Uuid>,
    name: String,
    amount_cents: i64,
    spent_at: NaiveDate,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<BudgetEntryRow> for BudgetEntryResponse {
    fn from(r: BudgetEntryRow) -> Self {
        BudgetEntryResponse {
            id: r.id,
            group_id: r.group_id,
            created_by: r.created_by,
            grocery_item_id: r.grocery_item_id,
            name: r.name,
            amount: r.amount_cents as f64 / 100.0,
            spent_at: r.spent_at,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// Converts a euro amount from the API boundary into an integer cent count
/// for storage — `budget_entries.amount_cents` is BIGINT rather than
/// DOUBLE PRECISION specifically so summing amounts (`budget_summary`)
/// doesn't accumulate binary floating-point rounding error.
fn to_cents(amount: f64) -> AppResult<i64> {
    if !amount.is_finite() || amount < 0.0 {
        return Err(AppError::BadRequest("amount_must_be_non_negative".into()));
    }
    Ok((amount * 100.0).round() as i64)
}

pub async fn create_budget_entry(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<CreateBudgetEntryRequest>,
) -> AppResult<impl IntoResponse> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("name_required".into()));
    }
    let amount_cents = to_cents(body.amount)?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    if let Some(item_id) = body.grocery_item_id {
        // Same "referenced resource doesn't exist" condition as
        // set_grocery_item_price below — use the same NotFound status so
        // clients don't have to branch on two different codes for it.
        sqlx::query_scalar!(
            "SELECT id FROM grocery_items WHERE id = $1 AND group_id = $2",
            item_id,
            group_id,
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?;

        let already_priced = sqlx::query_scalar!(
            "SELECT id FROM budget_entries WHERE grocery_item_id = $1 AND group_id = $2",
            item_id,
            group_id,
        )
        .fetch_optional(&mut *tx)
        .await?;
        if already_priced.is_some() {
            return Err(AppError::Conflict("grocery_item_already_priced".into()));
        }
    }

    let entry = sqlx::query_as!(
        BudgetEntryRow,
        r#"
        INSERT INTO budget_entries (group_id, created_by, grocery_item_id, name, amount_cents, spent_at)
        VALUES ($1, $2, $3, $4, $5, COALESCE($6, current_date))
        RETURNING id, group_id, created_by, grocery_item_id, name, amount_cents, spent_at, created_at, updated_at
        "#,
        group_id,
        auth.user_id,
        body.grocery_item_id,
        name,
        amount_cents,
        body.spent_at,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(BudgetEntryResponse::from(entry))))
}

/// Associates a price with a grocery item, e.g. once it's been checked
/// off/bought (docs/v1-scope.md epic #6). Any member may set a price, same
/// bar as creating a manual entry.
pub async fn set_grocery_item_price(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, item_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<SetGroceryItemPriceRequest>,
) -> AppResult<impl IntoResponse> {
    let amount_cents = to_cents(body.amount)?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let item = sqlx::query!(
        "SELECT name FROM grocery_items WHERE id = $1 AND group_id = $2",
        item_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    // "Set" is idempotent: retries/double-taps upsert the price on the
    // unique (non-null) grocery_item_id index instead of creating a second,
    // double-counted budget entry.
    let entry = sqlx::query_as!(
        BudgetEntryRow,
        r#"
        INSERT INTO budget_entries (group_id, created_by, grocery_item_id, name, amount_cents, spent_at)
        VALUES ($1, $2, $3, $4, $5, COALESCE($6, current_date))
        ON CONFLICT (grocery_item_id) WHERE grocery_item_id IS NOT NULL
        DO UPDATE SET
            name = EXCLUDED.name,
            amount_cents = EXCLUDED.amount_cents,
            spent_at = EXCLUDED.spent_at,
            updated_at = now()
        RETURNING id, group_id, created_by, grocery_item_id, name, amount_cents, spent_at, created_at, updated_at
        "#,
        group_id,
        auth.user_id,
        item_id,
        item.name,
        amount_cents,
        body.spent_at,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(BudgetEntryResponse::from(entry))))
}

pub async fn get_budget_entry(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, entry_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let entry = sqlx::query_as!(
        BudgetEntryRow,
        r#"SELECT id, group_id, created_by, grocery_item_id, name, amount_cents, spent_at, created_at, updated_at
           FROM budget_entries WHERE id = $1 AND group_id = $2"#,
        entry_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    tx.commit().await?;

    Ok(Json(BudgetEntryResponse::from(entry)))
}

pub async fn list_budget_entries(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let rows = sqlx::query_as!(
        BudgetEntryRow,
        r#"
        SELECT id, group_id, created_by, grocery_item_id, name, amount_cents, spent_at, created_at, updated_at
        FROM budget_entries
        WHERE group_id = $1
        ORDER BY spent_at DESC, created_at DESC
        "#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let entries: Vec<BudgetEntryResponse> =
        rows.into_iter().map(BudgetEntryResponse::from).collect();

    Ok(Json(json!({ "entries": entries })))
}

pub async fn update_budget_entry(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, entry_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateBudgetEntryRequest>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;

    // FOR UPDATE locks the row for the rest of this transaction, mirroring
    // grocery_list/items.rs's and stocks/items.rs's concurrent-write protection.
    let existing = sqlx::query!(
        "SELECT created_by, name, amount_cents, spent_at FROM budget_entries WHERE id = $1 AND group_id = $2 FOR UPDATE",
        entry_id,
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
        Some(n) => n.to_string(),
        None => existing.name,
    };
    let amount_cents = match body.amount {
        Some(amount) => to_cents(amount)?,
        None => existing.amount_cents,
    };
    let spent_at = body.spent_at.unwrap_or(existing.spent_at);

    let entry = sqlx::query_as!(
        BudgetEntryRow,
        r#"
        UPDATE budget_entries SET
            name = $3,
            amount_cents = $4,
            spent_at = $5,
            updated_at = now()
        WHERE id = $1 AND group_id = $2
        RETURNING id, group_id, created_by, grocery_item_id, name, amount_cents, spent_at, created_at, updated_at
        "#,
        entry_id,
        group_id,
        name,
        amount_cents,
        spent_at,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(BudgetEntryResponse::from(entry)))
}

pub async fn delete_budget_entry(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, entry_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;

    let existing = sqlx::query!(
        "SELECT created_by FROM budget_entries WHERE id = $1 AND group_id = $2",
        entry_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    if !can_modify(&actor_role, existing.created_by == auth.user_id) {
        return Err(AppError::Forbidden);
    }

    sqlx::query!(
        "DELETE FROM budget_entries WHERE id = $1 AND group_id = $2",
        entry_id,
        group_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct BudgetPeriodTotal {
    /// First day of the period (month), e.g. "2026-07-01".
    pub period: NaiveDate,
    pub total: f64,
}

/// Cumulated spend per month for the family (docs/v1-scope.md epic #6:
/// "cumul de dépenses par période... au niveau de la famille"). Computed on
/// read via `date_trunc`, not stored, same as stocks' derived low-stock.
pub async fn budget_summary(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let rows = sqlx::query!(
        r#"
        SELECT date_trunc('month', spent_at)::date as "period!", SUM(amount_cents)::bigint as "total_cents!"
        FROM budget_entries
        WHERE group_id = $1
        GROUP BY 1
        ORDER BY 1 DESC
        "#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let periods: Vec<BudgetPeriodTotal> = rows
        .into_iter()
        .map(|r| BudgetPeriodTotal {
            period: r.period,
            total: r.total_cents as f64 / 100.0,
        })
        .collect();

    Ok(Json(json!({ "periods": periods })))
}
