//! Spending categories and the family's monthly cap (#532). Same bar as the
//! entries (#531): an owner or an admin, a standard member gets 404 — and the
//! bar comes before the body is looked at, so a standard member's malformed
//! category is a 404 too — be it refused by the validation or unreadable to the
//! extractor —, never a 400 or 422 that would tell them the route exists.
//!
//! The wire shapes are `manage_our_home_shared::dto::budget`'s own structs;
//! the guards are `validation::budget`'s.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Utc};
use manage_our_home_shared::dto::budget::{
    BudgetCategory, BudgetCategoryList, BudgetSettings, CreateBudgetCategoryRequest,
    UpdateBudgetCategoryRequest,
};
use manage_our_home_shared::validation::budget::{
    cap_to_cents, is_category_color, validate_category_name, CategoryError, CATEGORIES_MAX,
};
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::budget::require_budget_access;
use crate::error::{AppError, AppResult};
use crate::AppState;

/// `0035_budget_categories.sql`'s case-insensitive name index.
const NAME_KEY: &str = "budget_categories_group_id_name_uidx";

impl From<CategoryError> for AppError {
    fn from(e: CategoryError) -> Self {
        AppError::BadRequest(e.code().into())
    }
}

pub(crate) fn cents_to_euros(cents: i64) -> f64 {
    cents as f64 / 100.0
}

fn optional_cap(cap: Option<f64>) -> Result<Option<i64>, CategoryError> {
    cap.map(cap_to_cents).transpose()
}

fn valid_color(color: &str) -> Result<&str, CategoryError> {
    if is_category_color(color) {
        Ok(color)
    } else {
        Err(CategoryError::InvalidColor)
    }
}

/// A second category of the same name, whatever its case, is a 409.
fn name_taken(e: sqlx::Error) -> AppError {
    match &e {
        sqlx::Error::Database(db) if db.constraint() == Some(NAME_KEY) => {
            AppError::Conflict("category_name_taken".into())
        }
        _ => e.into(),
    }
}

struct CategoryRow {
    id: Uuid,
    group_id: Uuid,
    name: String,
    color: String,
    monthly_cap_cents: Option<i64>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<CategoryRow> for BudgetCategory {
    fn from(r: CategoryRow) -> Self {
        BudgetCategory {
            id: r.id,
            group_id: r.group_id,
            name: r.name,
            color: r.color,
            monthly_cap: r.monthly_cap_cents.map(cents_to_euros),
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

pub async fn list_budget_categories(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_budget_access(&mut tx, group_id, auth.user_id).await?;

    let rows = sqlx::query_as!(
        CategoryRow,
        r#"SELECT id, group_id, name, color, monthly_cap_cents, created_at, updated_at
           FROM budget_categories WHERE group_id = $1 ORDER BY lower(name)"#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(BudgetCategoryList {
        categories: rows.into_iter().map(BudgetCategory::from).collect(),
    }))
}

pub async fn create_budget_category(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    body: Result<Json<CreateBudgetCategoryRequest>, JsonRejection>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_budget_access(&mut tx, group_id, auth.user_id).await?;
    let Json(body) = body?;

    let name = validate_category_name(&body.name)?;
    let color = valid_color(&body.color)?;
    let cap_cents = optional_cap(body.monthly_cap)?;

    // The settings row is the family's lock: two creations at once take
    // turns here, so the count below cannot let both through to a 31st.
    sqlx::query!(
        "INSERT INTO budget_settings (group_id) VALUES ($1) ON CONFLICT (group_id) DO NOTHING",
        group_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "SELECT group_id FROM budget_settings WHERE group_id = $1 FOR UPDATE",
        group_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    let count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM budget_categories WHERE group_id = $1"#,
        group_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    if count >= CATEGORIES_MAX {
        return Err(AppError::Conflict("category_limit_reached".into()));
    }

    let row = sqlx::query_as!(
        CategoryRow,
        r#"INSERT INTO budget_categories (group_id, name, color, monthly_cap_cents)
           VALUES ($1, $2, $3, $4)
           RETURNING id, group_id, name, color, monthly_cap_cents, created_at, updated_at"#,
        group_id,
        name,
        color,
        cap_cents,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(name_taken)?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(BudgetCategory::from(row))))
}

pub async fn update_budget_category(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, category_id)): Path<(Uuid, Uuid)>,
    body: Result<Json<UpdateBudgetCategoryRequest>, JsonRejection>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_budget_access(&mut tx, group_id, auth.user_id).await?;
    let Json(body) = body?;

    let name = body
        .name
        .as_deref()
        .map(validate_category_name)
        .transpose()?;
    let color = body.color.as_deref().map(valid_color).transpose()?;
    let cap = body.monthly_cap.map(optional_cap).transpose()?;

    let existing = sqlx::query!(
        "SELECT name, color, monthly_cap_cents FROM budget_categories
         WHERE id = $1 AND group_id = $2 FOR UPDATE",
        category_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    let row = sqlx::query_as!(
        CategoryRow,
        r#"UPDATE budget_categories SET name = $3, color = $4, monthly_cap_cents = $5, updated_at = now()
           WHERE id = $1 AND group_id = $2
           RETURNING id, group_id, name, color, monthly_cap_cents, created_at, updated_at"#,
        category_id,
        group_id,
        name.unwrap_or(&existing.name),
        color.unwrap_or(&existing.color),
        cap.unwrap_or(existing.monthly_cap_cents),
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(name_taken)?;
    tx.commit().await?;

    Ok(Json(BudgetCategory::from(row)))
}

/// Its entries stay, under « Sans catégorie »: the key's
/// `ON DELETE SET NULL (category_id)` clears theirs.
pub async fn delete_budget_category(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, category_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_budget_access(&mut tx, group_id, auth.user_id).await?;

    sqlx::query_scalar!(
        "DELETE FROM budget_categories WHERE id = $1 AND group_id = $2 RETURNING id",
        category_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

/// The family's monthly cap, `null` when none was set.
pub(crate) async fn family_cap_cents(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
) -> AppResult<Option<i64>> {
    Ok(sqlx::query_scalar!(
        "SELECT monthly_cap_cents FROM budget_settings WHERE group_id = $1",
        group_id,
    )
    .fetch_optional(&mut **tx)
    .await?
    .flatten())
}

pub async fn get_budget_settings(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_budget_access(&mut tx, group_id, auth.user_id).await?;
    let cap = family_cap_cents(&mut tx, group_id).await?;
    tx.commit().await?;

    Ok(Json(BudgetSettings {
        monthly_cap: cap.map(cents_to_euros),
    }))
}

/// Replaces the family's monthly cap: a number sets it, `null` or no
/// `monthly_cap` at all removes it.
pub async fn put_budget_settings(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    body: Result<Json<BudgetSettings>, JsonRejection>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_budget_access(&mut tx, group_id, auth.user_id).await?;
    let Json(body) = body?;

    let cap_cents = optional_cap(body.monthly_cap)?;
    let cap = sqlx::query_scalar!(
        r#"INSERT INTO budget_settings (group_id, monthly_cap_cents) VALUES ($1, $2)
           ON CONFLICT (group_id) DO UPDATE
           SET monthly_cap_cents = EXCLUDED.monthly_cap_cents, updated_at = now()
           RETURNING monthly_cap_cents"#,
        group_id,
        cap_cents,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(BudgetSettings {
        monthly_cap: cap.map(cents_to_euros),
    }))
}
