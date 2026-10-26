//! Recipes chosen for the next shopping (#469): `recipe_selections`
//! (`0033_recipe_selections.sql`).
//!
//! A recipe has at most one `planned` selection, so the endpoints name the
//! recipe rather than the selection. Any member may select a recipe,
//! change the number of people it is cooked for, or deselect it, whoever
//! selected it (controller's decision of 2026-10-24). Selecting a recipe
//! already planned is idempotent: 200 with the existing selection, its
//! `servings` updated when the request carries one. Moving a selection to
//! `done` or `abandoned` is `recipes::outcome` (#473); generating the
//! grocery list from the selections is #470.

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
use crate::AppState;

/// Upper bound on the number of people a selection is cooked for. The
/// `CHECK` of `recipe_selections.servings` (0033) holds the same bound.
pub(crate) const SERVINGS_MAX: i32 = 50;

/// A selection's number of people: from 1 to [`SERVINGS_MAX`].
pub(crate) fn validate_servings(servings: i32) -> AppResult<i32> {
    if (1..=SERVINGS_MAX).contains(&servings) {
        Ok(servings)
    } else {
        Err(AppError::BadRequest("invalid_servings".into()))
    }
}

#[derive(Deserialize)]
pub struct SelectRecipeRequest {
    /// Number of people; left as it is (or unset, on a new selection) when
    /// omitted.
    pub servings: Option<i32>,
}

#[derive(Deserialize)]
pub struct UpdateSelectionRequest {
    pub servings: i32,
}

#[derive(Serialize)]
pub struct SelectionResponse {
    pub id: Uuid,
    pub recipe_id: Uuid,
    pub recipe_name: String,
    pub status: String,
    pub servings: Option<i32>,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The recipe's name, or 404 when the recipe is not in this group.
async fn recipe_name(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    recipe_id: Uuid,
) -> AppResult<String> {
    sqlx::query_scalar!(
        "SELECT name FROM recipes WHERE id = $1 AND group_id = $2",
        recipe_id,
        group_id,
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(AppError::NotFound)
}

/// `POST /groups/:id/recipes/:recipe_id/selection`: 201 with a new
/// `planned` selection, or 200 with the one already planned.
pub async fn select_recipe(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<SelectRecipeRequest>,
) -> AppResult<impl IntoResponse> {
    let servings = body.servings.map(validate_servings).transpose()?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    let recipe_name = recipe_name(&mut tx, group_id, recipe_id).await?;

    // One statement, so two members selecting the same recipe at once end
    // up on the same row: the second waits on the unique index and takes
    // the `DO UPDATE` branch. `xmax = 0` tells a fresh row from an updated
    // one.
    let row = sqlx::query!(
        r#"
        INSERT INTO recipe_selections (group_id, recipe_id, servings, created_by)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (recipe_id) WHERE status = 'planned' DO UPDATE SET
            servings = COALESCE(EXCLUDED.servings, recipe_selections.servings),
            updated_at = CASE
                WHEN EXCLUDED.servings IS NOT NULL
                     AND EXCLUDED.servings IS DISTINCT FROM recipe_selections.servings
                THEN now()
                ELSE recipe_selections.updated_at
            END
        RETURNING id, recipe_id, status, servings, created_by, created_at, updated_at,
                  (xmax = 0) AS "inserted!"
        "#,
        group_id,
        recipe_id,
        servings,
        auth.user_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    let status = if row.inserted {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(SelectionResponse {
            id: row.id,
            recipe_id: row.recipe_id,
            recipe_name,
            status: row.status,
            servings: row.servings,
            created_by: row.created_by,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }),
    ))
}

/// `PATCH /groups/:id/recipes/:recipe_id/selection`: changes the number of
/// people of the recipe's `planned` selection; 404 when there is none.
pub async fn update_selection(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateSelectionRequest>,
) -> AppResult<impl IntoResponse> {
    let servings = validate_servings(body.servings)?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    let recipe_name = recipe_name(&mut tx, group_id, recipe_id).await?;

    let row = sqlx::query!(
        r#"
        UPDATE recipe_selections SET servings = $3, updated_at = now()
        WHERE recipe_id = $1 AND group_id = $2 AND status = 'planned'
        RETURNING id, recipe_id, status, servings, created_by, created_at, updated_at
        "#,
        recipe_id,
        group_id,
        servings,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    tx.commit().await?;

    Ok(Json(SelectionResponse {
        id: row.id,
        recipe_id: row.recipe_id,
        recipe_name,
        status: row.status,
        servings: row.servings,
        created_by: row.created_by,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }))
}

/// `DELETE /groups/:id/recipes/:recipe_id/selection`: deletes the recipe's
/// `planned` selection; 404 when there is none. A `done` or `abandoned`
/// selection (#473) is history and stays.
pub async fn deselect_recipe(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let deleted = sqlx::query!(
        "DELETE FROM recipe_selections WHERE recipe_id = $1 AND group_id = $2 AND status = 'planned'",
        recipe_id,
        group_id,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if deleted == 0 {
        return Err(AppError::NotFound);
    }
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /groups/:id/recipes/selections`: the group's `planned` selections,
/// oldest first.
pub async fn list_selections(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let selections = sqlx::query_as!(
        SelectionResponse,
        r#"
        SELECT s.id, s.recipe_id, r.name AS recipe_name, s.status, s.servings,
               s.created_by, s.created_at, s.updated_at
        FROM recipe_selections s
        JOIN recipes r ON r.id = s.recipe_id
        WHERE s.group_id = $1 AND s.status = 'planned'
        ORDER BY s.created_at, s.id
        "#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(json!({ "selections": selections })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rejected(servings: i32) -> bool {
        matches!(
            validate_servings(servings),
            Err(AppError::BadRequest(m)) if m == "invalid_servings"
        )
    }

    #[test]
    fn accepts_one_to_the_maximum() {
        assert_eq!(validate_servings(1).unwrap(), 1);
        assert_eq!(validate_servings(4).unwrap(), 4);
        assert_eq!(validate_servings(SERVINGS_MAX).unwrap(), SERVINGS_MAX);
    }

    #[test]
    fn rejects_zero_and_negatives() {
        assert!(rejected(0));
        assert!(rejected(-1));
        assert!(rejected(i32::MIN));
    }

    #[test]
    fn rejects_above_the_maximum() {
        assert!(rejected(SERVINGS_MAX + 1));
        assert!(rejected(i32::MAX));
    }
}
