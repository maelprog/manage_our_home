use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::AppState;

#[derive(Deserialize)]
pub struct LogMealRequest {
    /// Defaults to today when omitted.
    pub eaten_on: Option<NaiveDate>,
}

#[derive(Serialize)]
pub struct MealHistoryEntryResponse {
    pub id: Uuid,
    pub recipe_id: Uuid,
    pub eaten_on: NaiveDate,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
}

pub async fn log_meal(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<LogMealRequest>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    // Confirms the recipe exists in this group before logging against it,
    // rather than relying solely on the FK error (which would surface as an
    // opaque 500 through `AppError::Sqlx`).
    let recipe_exists = sqlx::query!(
        "SELECT id FROM recipes WHERE id = $1 AND group_id = $2",
        recipe_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .is_some();
    if !recipe_exists {
        return Err(AppError::NotFound);
    }

    let entry = sqlx::query_as!(
        MealHistoryEntryResponse,
        r#"
        INSERT INTO meal_history (group_id, recipe_id, eaten_on, created_by)
        VALUES ($1, $2, COALESCE($3, CURRENT_DATE), $4)
        RETURNING id, recipe_id, eaten_on, created_by, created_at
        "#,
        group_id,
        recipe_id,
        body.eaten_on,
        auth.user_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(entry)))
}

#[derive(Deserialize)]
pub struct ListMealHistoryQuery {
    /// Only entries eaten in the last `days_back` days. Defaults to 14 to
    /// match the variety window used by the suggestion algorithm.
    pub days_back: Option<i64>,
}

pub async fn list_meal_history(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Query(query): Query<ListMealHistoryQuery>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let days_back = query.days_back.unwrap_or(14);
    let entries = sqlx::query_as!(
        MealHistoryEntryResponse,
        r#"
        SELECT id, recipe_id, eaten_on, created_by, created_at
        FROM meal_history
        WHERE group_id = $1 AND eaten_on >= CURRENT_DATE - $2::bigint * INTERVAL '1 day'
        ORDER BY eaten_on DESC
        "#,
        group_id,
        days_back,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(json!({ "entries": entries })))
}
