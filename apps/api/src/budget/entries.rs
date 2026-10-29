use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Datelike, Months, NaiveDate, Utc};
use manage_our_home_shared::dto::budget::{BudgetCategoryUsage, BudgetMonthSummary};
use manage_our_home_shared::validation::budget::{cap_usage, parse_month};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::budget::categories::{cents_to_euros, family_cap_cents};
use crate::budget::{can_modify, require_budget_access};
use crate::error::{AppError, AppResult};
use crate::stocks::items::day_or_today;
use crate::AppState;

#[derive(Deserialize)]
pub struct CreateBudgetEntryRequest {
    pub name: String,
    pub amount: f64,
    pub spent_at: Option<NaiveDate>,
    pub grocery_item_id: Option<Uuid>,
    /// A category of the family (#532); none files it under « Sans
    /// catégorie ».
    pub category_id: Option<Uuid>,
}

#[derive(Deserialize)]
pub struct UpdateBudgetEntryRequest {
    pub name: Option<String>,
    pub amount: Option<f64>,
    pub spent_at: Option<NaiveDate>,
    /// `Some(None)` (`null`) moves the entry to « Sans catégorie » (#532);
    /// `None` (left out) keeps its category.
    #[serde(default, deserialize_with = "deserialize_some")]
    pub category_id: Option<Option<Uuid>>,
}

/// Serde's blanket `Option<T>` impl treats an explicit JSON `null` the same
/// as a missing key, so a naive `Option<Option<T>>` field can never observe
/// `Some(None)` (see grocery_list/items.rs for the same pattern).
fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
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
    pub category_id: Option<Uuid>,
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
    category_id: Option<Uuid>,
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
            category_id: r.category_id,
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
    body: Result<Json<CreateBudgetEntryRequest>, JsonRejection>,
) -> AppResult<impl IntoResponse> {
    // The bar first, the body after: a standard member gets the same 404
    // whatever they send, never a 400 or 422 telling them the route exists.
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_budget_access(&mut tx, group_id, auth.user_id).await?;
    let Json(body) = body?;

    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("name_required".into()));
    }
    let amount_cents = to_cents(body.amount)?;
    if let Some(category_id) = body.category_id {
        require_category(&mut tx, group_id, category_id).await?;
    }

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
        INSERT INTO budget_entries (group_id, created_by, grocery_item_id, name, amount_cents, spent_at, category_id)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, group_id, created_by, grocery_item_id, category_id, name, amount_cents, spent_at, created_at, updated_at
        "#,
        group_id,
        auth.user_id,
        body.grocery_item_id,
        name,
        amount_cents,
        day_or_today(body.spent_at, Utc::now()),
        body.category_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(BudgetEntryResponse::from(entry))))
}

/// A category of this family, or the 404 any other referenced resource gets
/// (the grocery item above). The composite key of `budget_entries` refuses
/// another family's category anyway; this turns that into a clean answer.
async fn require_category(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    category_id: Uuid,
) -> AppResult<()> {
    sqlx::query_scalar!(
        "SELECT id FROM budget_categories WHERE id = $1 AND group_id = $2",
        category_id,
        group_id,
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(())
}

/// Associates a price with a grocery item, e.g. once it's been checked
/// off/bought (docs/v1-scope.md epic #6). Same bar as creating a manual
/// entry: an owner or an admin (#531); a standard member gets 404, since a
/// price is a budget entry.
pub async fn set_grocery_item_price(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, item_id)): Path<(Uuid, Uuid)>,
    body: Result<Json<SetGroceryItemPriceRequest>, JsonRejection>,
) -> AppResult<impl IntoResponse> {
    // The bar first, as `create_budget_entry`.
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_budget_access(&mut tx, group_id, auth.user_id).await?;
    let Json(body) = body?;

    let amount_cents = to_cents(body.amount)?;

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
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (grocery_item_id) WHERE grocery_item_id IS NOT NULL
        DO UPDATE SET
            name = EXCLUDED.name,
            amount_cents = EXCLUDED.amount_cents,
            spent_at = EXCLUDED.spent_at,
            updated_at = now()
        RETURNING id, group_id, created_by, grocery_item_id, category_id, name, amount_cents, spent_at, created_at, updated_at
        "#,
        group_id,
        auth.user_id,
        item_id,
        item.name,
        amount_cents,
        day_or_today(body.spent_at, Utc::now()),
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
    require_budget_access(&mut tx, group_id, auth.user_id).await?;

    let entry = sqlx::query_as!(
        BudgetEntryRow,
        r#"SELECT id, group_id, created_by, grocery_item_id, category_id, name, amount_cents, spent_at, created_at, updated_at
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
    require_budget_access(&mut tx, group_id, auth.user_id).await?;

    let rows = sqlx::query_as!(
        BudgetEntryRow,
        r#"
        SELECT id, group_id, created_by, grocery_item_id, category_id, name, amount_cents, spent_at, created_at, updated_at
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
    body: Result<Json<UpdateBudgetEntryRequest>, JsonRejection>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_budget_access(&mut tx, group_id, auth.user_id).await?;
    let Json(body) = body?;

    // FOR UPDATE locks the row for the rest of this transaction, mirroring
    // grocery_list/items.rs's and stocks/items.rs's concurrent-write protection.
    let existing = sqlx::query!(
        "SELECT created_by, name, amount_cents, spent_at, category_id FROM budget_entries WHERE id = $1 AND group_id = $2 FOR UPDATE",
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
    let category_id = match body.category_id {
        Some(Some(category_id)) => {
            require_category(&mut tx, group_id, category_id).await?;
            Some(category_id)
        }
        Some(None) => None,
        None => existing.category_id,
    };

    let entry = sqlx::query_as!(
        BudgetEntryRow,
        r#"
        UPDATE budget_entries SET
            name = $3,
            amount_cents = $4,
            spent_at = $5,
            category_id = $6,
            updated_at = now()
        WHERE id = $1 AND group_id = $2
        RETURNING id, group_id, created_by, grocery_item_id, category_id, name, amount_cents, spent_at, created_at, updated_at
        "#,
        entry_id,
        group_id,
        name,
        amount_cents,
        spent_at,
        category_id,
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
    let actor_role = require_budget_access(&mut tx, group_id, auth.user_id).await?;

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

#[derive(Deserialize)]
pub struct SummaryQuery {
    /// `YYYY-MM`: the month the `month` member reports on (#532). Left out,
    /// the current month in Paris.
    pub month: Option<String>,
}

/// Cumulated spend per month for the family (docs/v1-scope.md epic #6:
/// "cumul de dépenses par période... au niveau de la famille"). Computed on
/// read via `date_trunc`, not stored, same as stocks' derived low-stock.
///
/// Since #532 it also reports one month against the caps, under `month`
/// (`BudgetMonthSummary`): the month's total and the family's cap, and per
/// category its total, its cap and the share of it spent
/// (`validation::budget::cap_usage`).
pub async fn budget_summary(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Query(query): Query<SummaryQuery>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_budget_access(&mut tx, group_id, auth.user_id).await?;

    let month = match query.month.as_deref() {
        Some(month) => {
            parse_month(month).ok_or_else(|| AppError::BadRequest("invalid_month".into()))?
        }
        None => day_or_today(None, Utc::now())
            .with_day(1)
            .expect("day 1 exists"),
    };
    let next_month = month
        .checked_add_months(Months::new(1))
        .ok_or_else(|| AppError::BadRequest("invalid_month".into()))?;

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

    let family_cap = family_cap_cents(&mut tx, group_id).await?;
    let categories = sqlx::query!(
        r#"
        SELECT c.id, c.name, c.color, c.monthly_cap_cents,
               COALESCE(SUM(e.amount_cents), 0)::bigint AS "total_cents!"
        FROM budget_categories c
        LEFT JOIN budget_entries e
               ON e.category_id = c.id AND e.group_id = c.group_id
              AND e.spent_at >= $2 AND e.spent_at < $3
        WHERE c.group_id = $1
        GROUP BY c.id
        ORDER BY lower(c.name)
        "#,
        group_id,
        month,
        next_month,
    )
    .fetch_all(&mut *tx)
    .await?;
    let uncategorized_cents = sqlx::query_scalar!(
        r#"
        SELECT COALESCE(SUM(amount_cents), 0)::bigint AS "total_cents!"
        FROM budget_entries
        WHERE group_id = $1 AND category_id IS NULL AND spent_at >= $2 AND spent_at < $3
        "#,
        group_id,
        month,
        next_month,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    let periods: Vec<BudgetPeriodTotal> = rows
        .into_iter()
        .map(|r| BudgetPeriodTotal {
            period: r.period,
            total: r.total_cents as f64 / 100.0,
        })
        .collect();

    let total_cents = categories.iter().fold(uncategorized_cents, |sum, c| {
        sum.saturating_add(c.total_cents)
    });
    let usage = cap_usage(total_cents, family_cap);
    let month = BudgetMonthSummary {
        period: month,
        total: cents_to_euros(total_cents),
        monthly_cap: family_cap.map(cents_to_euros),
        percent: usage.percent,
        over_cap: usage.over_cap,
        categories: categories
            .into_iter()
            .map(|c| {
                let usage = cap_usage(c.total_cents, c.monthly_cap_cents);
                BudgetCategoryUsage {
                    id: c.id,
                    name: c.name,
                    color: c.color,
                    total: cents_to_euros(c.total_cents),
                    monthly_cap: c.monthly_cap_cents.map(cents_to_euros),
                    percent: usage.percent,
                    over_cap: usage.over_cap,
                }
            })
            .collect(),
        uncategorized: cents_to_euros(uncategorized_cents),
    };

    Ok(Json(json!({ "periods": periods, "month": month })))
}
