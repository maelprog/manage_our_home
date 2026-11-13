//! Recipes chosen for the next shopping (#469): `recipe_selections`
//! (`0033_recipe_selections.sql`).
//!
//! A recipe has at most one `planned` selection, so the endpoints name the
//! recipe rather than the selection. Any member may select a recipe,
//! change the number of people it is cooked for, or deselect it, whoever
//! selected it (controller's decision of 2026-10-24). Selecting a recipe
//! already planned is idempotent: 200 with the existing selection, its
//! `servings` updated when the request carries one. A selection may say
//! when it is meant to be cooked, a day and a meal of that day (#811,
//! `0056_recipe_selection_plan.sql`): both optional, changed by any member
//! like `servings`. Moving a selection to
//! `done` or `abandoned` is `recipes::outcome` (#473); generating the
//! grocery list from the selections is #470. The list tells, for each
//! selection, what the stock lacks and by when it should be cooked
//! (`recipes::availability`, #559). A selection may be replaced by another
//! recipe, keeping its day, meal and number of people (#815).

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::grocery_list::items::add_missing_lines;
use crate::groups::require_role;
use crate::recipes::availability::{by_priority, planned_availability, MissingResponse};
use crate::recipes::suggestions::{replacement_candidates, score_recipes};
use crate::stocks::items::today;
use crate::AppState;

/// Upper bound on the number of people a selection is cooked for. The
/// `CHECK` of `recipe_selections.servings` (0033) holds the same bound.
pub(crate) const SERVINGS_MAX: i32 = 50;

/// How far from the day of the request a selection may be dated, before or
/// after (#811).
pub(crate) const PLAN_HORIZON_DAYS: i64 = 366;

/// The meal of the day a selection is planned for (#811). The `CHECK` of
/// `recipe_selections.meal_slot` (0056) holds the same two values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MealSlot {
    Lunch,
    Dinner,
}

impl MealSlot {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "lunch" => Some(Self::Lunch),
            "dinner" => Some(Self::Dinner),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Lunch => "lunch",
            Self::Dinner => "dinner",
        }
    }
}

/// When a selection is meant to be cooked: a day, and a meal of that day.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    pub planned_on: Option<NaiveDate>,
    pub meal_slot: Option<MealSlot>,
}

/// The plan `current` becomes once a request's fields are applied (#811).
/// Each field has three states: absent (`None`, left as it is), `null`
/// (`Some(None)`, removed) and a value (set). A date given must lie within
/// [`PLAN_HORIZON_DAYS`] of `today` (`invalid_planned_on`); a date kept
/// from `current` is not judged again, so an old plan does not block a
/// change of `servings`. A meal must be `lunch` or `dinner`, and the
/// result may not have a meal without a day (`invalid_meal_slot`).
pub(crate) fn merge_plan(
    current: Plan,
    planned_on: Option<Option<NaiveDate>>,
    meal_slot: Option<Option<&str>>,
    today: NaiveDate,
) -> AppResult<Plan> {
    let planned_on = match planned_on {
        None => current.planned_on,
        Some(None) => None,
        Some(Some(day)) => {
            if (day - today).num_days().abs() > PLAN_HORIZON_DAYS {
                return Err(AppError::BadRequest("invalid_planned_on".into()));
            }
            Some(day)
        }
    };
    let meal_slot = match meal_slot {
        None => current.meal_slot,
        Some(None) => None,
        Some(Some(value)) => Some(
            MealSlot::parse(value)
                .ok_or_else(|| AppError::BadRequest("invalid_meal_slot".into()))?,
        ),
    };
    if meal_slot.is_some() && planned_on.is_none() {
        return Err(AppError::BadRequest("invalid_meal_slot".into()));
    }
    Ok(Plan {
        planned_on,
        meal_slot,
    })
}

/// The order of the list by plan (#811): by day, then lunch, dinner and
/// no meal; the selections without a day last. Sorted stably on it, the
/// undated ones keep the order they came in.
pub(crate) fn plan_key(
    planned_on: Option<NaiveDate>,
    meal_slot: Option<&str>,
) -> (bool, Option<NaiveDate>, u8) {
    let meal = match meal_slot.and_then(MealSlot::parse) {
        Some(MealSlot::Lunch) => 0,
        Some(MealSlot::Dinner) => 1,
        None => 2,
    };
    // `false` sorts first.
    (planned_on.is_none(), planned_on, meal)
}

/// A selection's number of people: from 1 to [`SERVINGS_MAX`].
pub(crate) fn validate_servings(servings: i32) -> AppResult<i32> {
    if (1..=SERVINGS_MAX).contains(&servings) {
        Ok(servings)
    } else {
        Err(AppError::BadRequest("invalid_servings".into()))
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SelectRecipeRequest {
    /// Number of people; left as it is (or unset, on a new selection) when
    /// omitted.
    pub servings: Option<i32>,
    /// Day the recipe is meant to be cooked, at most 366 days from today.
    /// When given, it replaces the selection's day and meal (`meal_slot`,
    /// unset when omitted); when omitted, both are left as they are.
    pub planned_on: Option<NaiveDate>,
    /// `lunch` or `dinner`; only with `planned_on`.
    pub meal_slot: Option<String>,
}

/// Serde's blanket `Option<T>` impl treats an explicit JSON `null` the same
/// as a missing key, so a naive `Option<Option<T>>` field can never observe
/// `Some(None)` (same pattern as `recipes::crud`).
fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// Each field is left as it is when omitted.
#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpdateSelectionRequest {
    pub servings: Option<i32>,
    /// Day the recipe is meant to be cooked, at most 366 days from today;
    /// `null` removes it, and must remove the meal with it.
    #[serde(default, deserialize_with = "deserialize_some")]
    pub planned_on: Option<Option<NaiveDate>>,
    /// `lunch` or `dinner`, on a selection that has a day; `null` removes
    /// it.
    #[serde(default, deserialize_with = "deserialize_some")]
    pub meal_slot: Option<Option<String>>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SelectionResponse {
    pub id: Uuid,
    pub recipe_id: Uuid,
    pub recipe_name: String,
    pub status: String,
    pub servings: Option<i32>,
    /// Day the recipe is meant to be cooked (#811), if said.
    pub planned_on: Option<NaiveDate>,
    /// `lunch` or `dinner` (#811), if said; never without `planned_on`.
    pub meal_slot: Option<String>,
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
#[utoipa::path(
    post,
    path = "/groups/{id}/recipes/{recipe_id}/selection",
    tag = "recipes",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("recipe_id" = uuid::Uuid, Path, description = "Recipe id"),
    ),
    request_body = SelectRecipeRequest,
    responses(
        (status = 201, description = "Recipe planned", body = SelectionResponse),
        (status = 200, description = "Already planned: the selection as it is", body = SelectionResponse),
        (status = 400, description = "`invalid_servings`, `invalid_planned_on` (more than 366 days from today), `invalid_meal_slot` (not `lunch` nor `dinner`, or without `planned_on`)", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 404, description = "`not_found`: no such recipe", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn select_recipe(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<SelectRecipeRequest>,
) -> AppResult<impl IntoResponse> {
    let servings = body.servings.map(validate_servings).transpose()?;
    // Judged on its own: a day given replaces the plan, meal included, and
    // a meal comes only with a day.
    let plan = merge_plan(
        Plan::default(),
        body.planned_on.map(Some),
        body.meal_slot.as_deref().map(Some),
        today(),
    )?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    let recipe_name = recipe_name(&mut tx, group_id, recipe_id).await?;

    // One statement, so two members selecting the same recipe at once end
    // up on the same row: the second waits on the unique index and takes
    // the `DO UPDATE` branch. `xmax = 0` tells a fresh row from an updated
    // one.
    let row = sqlx::query!(
        r#"
        INSERT INTO recipe_selections
            (group_id, recipe_id, servings, planned_on, meal_slot, created_by)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (recipe_id) WHERE status = 'planned' DO UPDATE SET
            servings = COALESCE(EXCLUDED.servings, recipe_selections.servings),
            planned_on = COALESCE(EXCLUDED.planned_on, recipe_selections.planned_on),
            meal_slot = CASE
                WHEN EXCLUDED.planned_on IS NOT NULL THEN EXCLUDED.meal_slot
                ELSE recipe_selections.meal_slot
            END,
            updated_at = CASE
                WHEN (EXCLUDED.servings IS NOT NULL
                      AND EXCLUDED.servings IS DISTINCT FROM recipe_selections.servings)
                  OR (EXCLUDED.planned_on IS NOT NULL
                      AND (EXCLUDED.planned_on, EXCLUDED.meal_slot) IS DISTINCT FROM
                          (recipe_selections.planned_on, recipe_selections.meal_slot))
                THEN now()
                ELSE recipe_selections.updated_at
            END
        RETURNING id, recipe_id, status, servings, planned_on, meal_slot,
                  created_by, created_at, updated_at,
                  (xmax = 0) AS "inserted!"
        "#,
        group_id,
        recipe_id,
        servings,
        plan.planned_on,
        plan.meal_slot.map(MealSlot::as_str),
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
            planned_on: row.planned_on,
            meal_slot: row.meal_slot,
            created_by: row.created_by,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }),
    ))
}

/// `PATCH /groups/:id/recipes/:recipe_id/selection`: changes the number of
/// people, the day or the meal of the recipe's `planned` selection; 404
/// when there is none.
#[utoipa::path(
    patch,
    path = "/groups/{id}/recipes/{recipe_id}/selection",
    tag = "recipes",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("recipe_id" = uuid::Uuid, Path, description = "Recipe id"),
    ),
    request_body = UpdateSelectionRequest,
    responses(
        (status = 200, description = "Selection updated", body = SelectionResponse),
        (status = 400, description = "`invalid_servings`, `invalid_planned_on` (more than 366 days from today), `invalid_meal_slot` (not `lunch` nor `dinner`, or left without a day)", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 404, description = "`not_found`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn update_selection(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateSelectionRequest>,
) -> AppResult<impl IntoResponse> {
    let servings = body.servings.map(validate_servings).transpose()?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    let recipe_name = recipe_name(&mut tx, group_id, recipe_id).await?;

    // The plan is judged once merged with the stored one (a meal may not
    // outlive its day), so the row is read and locked first.
    let current = sqlx::query!(
        r#"
        SELECT planned_on, meal_slot FROM recipe_selections
        WHERE recipe_id = $1 AND group_id = $2 AND status = 'planned'
        FOR UPDATE
        "#,
        recipe_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    let plan = merge_plan(
        Plan {
            planned_on: current.planned_on,
            meal_slot: current.meal_slot.as_deref().and_then(MealSlot::parse),
        },
        body.planned_on,
        body.meal_slot.as_ref().map(Option::as_deref),
        today(),
    )?;

    let row = sqlx::query!(
        r#"
        UPDATE recipe_selections SET
            servings = COALESCE($3, servings),
            planned_on = $4,
            meal_slot = $5,
            updated_at = now()
        WHERE recipe_id = $1 AND group_id = $2 AND status = 'planned'
        RETURNING id, recipe_id, status, servings, planned_on, meal_slot,
                  created_by, created_at, updated_at
        "#,
        recipe_id,
        group_id,
        servings,
        plan.planned_on,
        plan.meal_slot.map(MealSlot::as_str),
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(SelectionResponse {
        id: row.id,
        recipe_id: row.recipe_id,
        recipe_name,
        status: row.status,
        servings: row.servings,
        planned_on: row.planned_on,
        meal_slot: row.meal_slot,
        created_by: row.created_by,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }))
}

/// `DELETE /groups/:id/recipes/:recipe_id/selection`: deletes the recipe's
/// `planned` selection; 404 when there is none. A `done` or `abandoned`
/// selection (#473) is history and stays.
#[utoipa::path(
    delete,
    path = "/groups/{id}/recipes/{recipe_id}/selection",
    tag = "recipes",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("recipe_id" = uuid::Uuid, Path, description = "Recipe id"),
    ),
    responses(
        (status = 204, description = "Selection removed"),
        (status = 404, description = "`not_found`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
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

/// The recipe's `planned` selection, locked when `lock`; 404 when there is
/// none.
async fn planned_row(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    recipe_id: Uuid,
    lock: bool,
) -> AppResult<PlannedRow> {
    let row = if lock {
        sqlx::query_as!(
            PlannedRow,
            r#"
            SELECT id, servings, planned_on, meal_slot FROM recipe_selections
            WHERE recipe_id = $1 AND group_id = $2 AND status = 'planned'
            FOR UPDATE
            "#,
            recipe_id,
            group_id,
        )
        .fetch_optional(&mut **tx)
        .await?
    } else {
        sqlx::query_as!(
            PlannedRow,
            r#"
            SELECT id, servings, planned_on, meal_slot FROM recipe_selections
            WHERE recipe_id = $1 AND group_id = $2 AND status = 'planned'
            "#,
            recipe_id,
            group_id,
        )
        .fetch_optional(&mut **tx)
        .await?
    };
    row.ok_or(AppError::NotFound)
}

struct PlannedRow {
    id: Uuid,
    servings: Option<i32>,
    planned_on: Option<NaiveDate>,
    meal_slot: Option<String>,
}

/// `GET /groups/:id/recipes/:recipe_id/selection/replacements` (#815): the
/// recipes offered in place of the recipe's `planned` selection, scored as
/// the suggestions are (`recipes::suggestions`), neither that recipe nor
/// one already planned, at most three, the best first
/// (`suggestions::replacement_candidates`). Empty when no other recipe is
/// left; 404 without a `planned` selection. Any member may.
#[utoipa::path(
    get,
    path = "/groups/{id}/recipes/{recipe_id}/selection/replacements",
    tag = "recipes",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("recipe_id" = uuid::Uuid, Path, description = "Recipe of the planned selection to replace"),
    ),
    responses(
        (status = 200, description = "Up to three recipes to plan instead, the best first", body = crate::api_docs::schemas::ReplacementList),
        (status = 404, description = "`not_found`: no planned selection", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_replacements(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    planned_row(&mut tx, group_id, recipe_id, false).await?;

    let planned: HashSet<Uuid> = sqlx::query_scalar!(
        "SELECT recipe_id FROM recipe_selections WHERE group_id = $1 AND status = 'planned'",
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    let ranked = score_recipes(&mut tx, group_id).await?;
    tx.commit().await?;

    Ok(Json(json!({
        "replacements": replacement_candidates(ranked, recipe_id, &planned),
    })))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ReplaceSelectionRequest {
    /// The recipe to plan instead; any recipe of the group not already
    /// planned, offered or not.
    pub recipe_id: Uuid,
}

/// `POST /groups/:id/recipes/:recipe_id/selection/replacement` (#815):
/// plans `recipe_id` in place of the recipe's `planned` selection, with
/// the same day, meal and number of people, and deletes the replaced
/// selection, as deselecting does. The grocery list follows: the unchecked
/// items generated for the replaced recipe are removed, except those a
/// recipe still planned needs (`grocery_lines_to_drop`), and what the
/// stock lacks of the new recipe is added, as
/// `…/selection/missing` adds it. When no unchecked item was generated for
/// the replaced recipe nor bears the name of one of its required
/// ingredients (`list_follows`), whichever recipe it is attributed to, the
/// list is left as it is. One transaction: on any
/// error, the replaced selection and the list stay as they were. Any member
/// may, whoever made the selection, and may remove items generated by
/// another member this way, as abandoning a recipe does.
#[utoipa::path(
    post,
    path = "/groups/{id}/recipes/{recipe_id}/selection/replacement",
    tag = "recipes",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("recipe_id" = uuid::Uuid, Path, description = "Recipe of the planned selection to replace"),
    ),
    request_body = ReplaceSelectionRequest,
    responses(
        (status = 200, description = "Replaced: the new selection, and what changed on the grocery list", body = crate::api_docs::schemas::SelectionReplaced),
        (status = 400, description = "`same_recipe`: the replacement is the replaced recipe", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 404, description = "`not_found`: no planned selection, or no such replacement recipe", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 409, description = "`already_planned`: the replacement recipe already has a planned selection", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn replace_selection(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ReplaceSelectionRequest>,
) -> AppResult<impl IntoResponse> {
    let new_recipe = body.recipe_id;
    if new_recipe == recipe_id {
        return Err(AppError::BadRequest("same_recipe".into()));
    }

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    let old = planned_row(&mut tx, group_id, recipe_id, true).await?;
    let recipe_name = recipe_name(&mut tx, group_id, new_recipe).await?;

    // A planned selection of the new recipe, made meanwhile or before,
    // holds the unique index: nothing is inserted, and the transaction is
    // dropped with the replaced selection untouched.
    let row = sqlx::query!(
        r#"
        INSERT INTO recipe_selections
            (group_id, recipe_id, servings, planned_on, meal_slot, created_by)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (recipe_id) WHERE status = 'planned' DO NOTHING
        RETURNING id, recipe_id, status, servings, planned_on, meal_slot,
                  created_by, created_at, updated_at
        "#,
        group_id,
        new_recipe,
        old.servings,
        old.planned_on,
        old.meal_slot,
        auth.user_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::Conflict("already_planned".into()))?;
    sqlx::query!(
        "DELETE FROM recipe_selections WHERE id = $1 AND group_id = $2",
        old.id,
        group_id,
    )
    .execute(&mut *tx)
    .await?;

    let old_lines = sqlx::query!(
        r#"
        SELECT id, name FROM grocery_items
        WHERE group_id = $1 AND source_recipe_id = $2 AND checked = false
        ORDER BY id FOR UPDATE
        "#,
        group_id,
        recipe_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    // An item generated for the replaced recipe, or bearing one of its
    // ingredients under another recipe planned first.
    let follows = !old_lines.is_empty() || {
        let unchecked: Vec<String> = sqlx::query_scalar!(
            "SELECT name FROM grocery_items WHERE group_id = $1 AND checked = false",
            group_id,
        )
        .fetch_all(&mut *tx)
        .await?;
        let required: Vec<String> = sqlx::query_scalar!(
            r#"
            SELECT name FROM recipe_ingredients
            WHERE recipe_id = $1 AND group_id = $2 AND is_optional = false
            "#,
            recipe_id,
            group_id,
        )
        .fetch_all(&mut *tx)
        .await?;
        let unchecked: Vec<&str> = unchecked.iter().map(String::as_str).collect();
        let required: Vec<&str> = required.iter().map(String::as_str).collect();
        list_follows(&unchecked, &required)
    };
    let (removed, added) = if !follows {
        (vec![], vec![])
    } else {
        // The recipes still planned, the new one aside: its lines are
        // added below with its own quantities.
        let still_needed: Vec<String> = sqlx::query_scalar!(
            r#"
            SELECT ri.name FROM recipe_selections s
            JOIN recipe_ingredients ri ON ri.recipe_id = s.recipe_id
            WHERE s.group_id = $1 AND s.status = 'planned'
              AND ri.is_optional = false AND s.recipe_id <> $2
            "#,
            group_id,
            new_recipe,
        )
        .fetch_all(&mut *tx)
        .await?;
        let lines: Vec<(Uuid, &str)> = old_lines.iter().map(|l| (l.id, l.name.as_str())).collect();
        let needed: Vec<&str> = still_needed.iter().map(String::as_str).collect();
        let removed = grocery_lines_to_drop(&lines, &needed);
        sqlx::query!(
            "DELETE FROM grocery_items WHERE group_id = $1 AND id = ANY($2)",
            group_id,
            &removed,
        )
        .execute(&mut *tx)
        .await?;
        let added = add_missing_lines(&mut tx, group_id, auth.user_id, new_recipe).await?;
        (removed, added)
    };
    tx.commit().await?;

    Ok(Json(json!({
        "selection": SelectionResponse {
            id: row.id,
            recipe_id: row.recipe_id,
            recipe_name,
            status: row.status,
            servings: row.servings,
            planned_on: row.planned_on,
            meal_slot: row.meal_slot,
            created_by: row.created_by,
            created_at: row.created_at,
            updated_at: row.updated_at,
        },
        "removed_grocery_items": removed,
        "added_grocery_items": added,
    })))
}

/// One `planned` selection of the list (#559): the selection, what the
/// stock lacks of its recipe's required ingredients (`missing`, empty when
/// the stock covers them), and the date by which it should be cooked to
/// use up what expires (`use_by`), with the days left until then in
/// Europe/Paris (`days_left`, negative once past). A non-empty `missing`
/// is a warning: the recipe can still be cooked.
#[derive(Serialize, utoipa::ToSchema)]
pub struct PlannedSelectionResponse {
    #[serde(flatten)]
    pub selection: SelectionResponse,
    pub missing: Vec<MissingResponse>,
    pub use_by: Option<chrono::NaiveDate>,
    pub days_left: Option<i64>,
}

/// `GET /groups/:id/recipes/selections`: the group's `planned` selections,
/// the dated ones first, by day then lunch, dinner and no meal (#811); the
/// undated ones last, those with a `use_by` first, the nearest first, then
/// the others oldest first. Dated selections of a same day and meal follow
/// that same `use_by` order.
#[utoipa::path(
    get,
    path = "/groups/{id}/recipes/selections",
    tag = "recipes",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
    ),
    responses(
        (status = 200, description = "The planned recipes, with what is missing for each", body = crate::api_docs::schemas::PlannedSelectionList),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
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
               s.planned_on, s.meal_slot, s.created_by, s.created_at, s.updated_at
        FROM recipe_selections s
        JOIN recipes r ON r.id = s.recipe_id
        WHERE s.group_id = $1 AND s.status = 'planned'
        ORDER BY s.created_at, s.id
        "#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    let today = today();
    let mut availability = planned_availability(&mut tx, group_id, None, today).await?;
    tx.commit().await?;

    let planned: Vec<_> = selections
        .into_iter()
        .map(|selection| {
            let (missing, use_by) = match availability
                .iter()
                .position(|a| a.selection_id == selection.id)
            {
                Some(i) => {
                    let a = availability.swap_remove(i);
                    (a.missing, a.use_by)
                }
                None => (vec![], None),
            };
            let response = PlannedSelectionResponse {
                selection,
                missing: missing.into_iter().map(MissingResponse::from).collect(),
                use_by,
                days_left: use_by.map(|d| (d - today).num_days()),
            };
            (response, use_by)
        })
        .collect();
    let mut selections: Vec<PlannedSelectionResponse> =
        by_priority(planned).into_iter().map(|(s, _)| s).collect();
    // Stable: the undated selections keep the order of `by_priority`.
    selections.sort_by_key(|s| plan_key(s.selection.planned_on, s.selection.meal_slot.as_deref()));

    Ok(Json(json!({ "selections": selections })))
}

/// The grocery lines a replacement removes (#815): of the unchecked lines
/// generated for the replaced recipe, those no other planned recipe needs.
/// A line is still needed when its name, trimmed and case-insensitive (as
/// the generation matches names), is a required ingredient of a recipe
/// that stays planned; the generation attributes a line several recipes
/// share to the first one only, so removing it would leave the others
/// without it.
pub(crate) fn grocery_lines_to_drop(lines: &[(Uuid, &str)], still_needed: &[&str]) -> Vec<Uuid> {
    let needed: HashSet<String> = still_needed
        .iter()
        .map(|name| name.trim().to_lowercase())
        .collect();
    lines
        .iter()
        .filter(|(_, name)| !needed.contains(&name.trim().to_lowercase()))
        .map(|(id, _)| *id)
        .collect()
}

/// Whether the grocery list follows a recipe (#815): one of its unchecked
/// items bears the name, trimmed and case-insensitive, of a required
/// ingredient of the recipe, whichever recipe the item is attributed to.
pub(crate) fn list_follows(unchecked: &[&str], required: &[&str]) -> bool {
    let required: HashSet<String> = required
        .iter()
        .map(|name| name.trim().to_lowercase())
        .collect();
    unchecked
        .iter()
        .any(|name| required.contains(&name.trim().to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- list_follows (#815) -------------------------------------------------

    #[test]
    fn the_list_follows_a_recipe_one_of_whose_ingredients_it_holds() {
        assert!(list_follows(&["Pain", " crème "], &["Crème", "Lardons"]));
        assert!(list_follows(&["LARDONS"], &["lardons"]));
    }

    #[test]
    fn the_list_does_not_follow_a_recipe_it_holds_nothing_of() {
        assert!(!list_follows(&["Pain"], &["Crème", "Lardons"]));
        assert!(!list_follows(&[], &["Crème"]));
        assert!(!list_follows(&["Crème"], &[]));
    }

    // -- grocery_lines_to_drop (#815) ---------------------------------------

    #[test]
    fn drops_every_line_no_other_recipe_needs() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        assert_eq!(
            grocery_lines_to_drop(&[(a, "Crème"), (b, "Lardons")], &["oignon"]),
            [a, b]
        );
        assert_eq!(
            grocery_lines_to_drop(&[(a, "Crème"), (b, "Lardons")], &[]),
            [a, b]
        );
    }

    #[test]
    fn keeps_a_line_another_planned_recipe_needs_whatever_its_case() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        assert_eq!(
            grocery_lines_to_drop(&[(a, " Crème "), (b, "Lardons")], &["crème"]),
            [b]
        );
        assert_eq!(
            grocery_lines_to_drop(&[(a, "crème"), (b, "LARDONS")], &[" Lardons", "Crème"]),
            Vec::<Uuid>::new()
        );
    }

    #[test]
    fn nothing_to_drop_without_a_line() {
        assert!(grocery_lines_to_drop(&[], &["crème"]).is_empty());
    }

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

    // -- merge_plan (#811) -------------------------------------------------

    fn day(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    const TODAY: &str = "2026-11-13";

    fn planned(on: &str, meal: Option<MealSlot>) -> Plan {
        Plan {
            planned_on: Some(day(on)),
            meal_slot: meal,
        }
    }

    fn merge(
        current: Plan,
        planned_on: Option<Option<&str>>,
        meal_slot: Option<Option<&str>>,
    ) -> AppResult<Plan> {
        merge_plan(
            current,
            planned_on.map(|d| d.map(day)),
            meal_slot,
            day(TODAY),
        )
    }

    fn refused(result: AppResult<Plan>, code: &str) -> bool {
        matches!(result, Err(AppError::BadRequest(m)) if m == code)
    }

    #[test]
    fn nothing_given_leaves_the_plan_as_it_is() {
        assert_eq!(merge(Plan::default(), None, None).unwrap(), Plan::default());
        let lunch = planned("2026-11-20", Some(MealSlot::Lunch));
        assert_eq!(merge(lunch, None, None).unwrap(), lunch);
    }

    #[test]
    fn sets_a_day_alone_or_with_a_meal() {
        assert_eq!(
            merge(Plan::default(), Some(Some("2026-11-20")), None).unwrap(),
            planned("2026-11-20", None)
        );
        assert_eq!(
            merge(
                Plan::default(),
                Some(Some("2026-11-20")),
                Some(Some("lunch"))
            )
            .unwrap(),
            planned("2026-11-20", Some(MealSlot::Lunch))
        );
        assert_eq!(
            merge(
                Plan::default(),
                Some(Some("2026-11-20")),
                Some(Some("dinner"))
            )
            .unwrap(),
            planned("2026-11-20", Some(MealSlot::Dinner))
        );
    }

    #[test]
    fn changes_the_day_and_keeps_the_meal() {
        let lunch = planned("2026-11-20", Some(MealSlot::Lunch));
        assert_eq!(
            merge(lunch, Some(Some("2026-11-21")), None).unwrap(),
            planned("2026-11-21", Some(MealSlot::Lunch))
        );
    }

    #[test]
    fn sets_or_removes_the_meal_of_a_dated_plan() {
        let bare = planned("2026-11-20", None);
        assert_eq!(
            merge(bare, None, Some(Some("dinner"))).unwrap(),
            planned("2026-11-20", Some(MealSlot::Dinner))
        );
        let dinner = planned("2026-11-20", Some(MealSlot::Dinner));
        assert_eq!(merge(dinner, None, Some(None)).unwrap(), bare);
    }

    #[test]
    fn removes_the_day_with_its_meal() {
        let lunch = planned("2026-11-20", Some(MealSlot::Lunch));
        assert_eq!(
            merge(lunch, Some(None), Some(None)).unwrap(),
            Plan::default()
        );
        assert_eq!(
            merge(planned("2026-11-20", None), Some(None), None).unwrap(),
            Plan::default()
        );
    }

    #[test]
    fn refuses_a_meal_without_a_day() {
        assert!(refused(
            merge(Plan::default(), None, Some(Some("lunch"))),
            "invalid_meal_slot"
        ));
        assert!(refused(
            merge(Plan::default(), Some(None), Some(Some("dinner"))),
            "invalid_meal_slot"
        ));
        // Removing the day of a plan that keeps its meal.
        let lunch = planned("2026-11-20", Some(MealSlot::Lunch));
        assert!(refused(merge(lunch, Some(None), None), "invalid_meal_slot"));
    }

    #[test]
    fn refuses_an_unknown_meal() {
        for meal in ["breakfast", "Lunch", "", " lunch"] {
            assert!(
                refused(
                    merge(Plan::default(), Some(Some("2026-11-20")), Some(Some(meal))),
                    "invalid_meal_slot"
                ),
                "{meal:?}"
            );
        }
    }

    #[test]
    fn accepts_a_day_up_to_the_horizon_either_way() {
        for on in ["2025-11-12", "2026-11-13", "2027-11-14"] {
            assert_eq!(
                merge(Plan::default(), Some(Some(on)), None).unwrap(),
                planned(on, None),
                "{on}"
            );
        }
        let today = day(TODAY);
        assert_eq!(
            (day("2027-11-14") - today).num_days(),
            PLAN_HORIZON_DAYS,
            "the fixture sits on the bound"
        );
        assert_eq!((today - day("2025-11-12")).num_days(), PLAN_HORIZON_DAYS);
    }

    #[test]
    fn refuses_a_day_past_the_horizon_either_way() {
        for on in ["2025-11-11", "2027-11-15", "1970-01-01", "9999-12-31"] {
            assert!(
                refused(
                    merge(Plan::default(), Some(Some(on)), None),
                    "invalid_planned_on"
                ),
                "{on}"
            );
        }
    }

    #[test]
    fn a_kept_day_is_not_judged_again() {
        let old = planned("2020-01-01", Some(MealSlot::Lunch));
        assert_eq!(merge(old, None, None).unwrap(), old);
        assert_eq!(
            merge(old, None, Some(Some("dinner"))).unwrap(),
            planned("2020-01-01", Some(MealSlot::Dinner))
        );
    }

    // -- plan_key (#811) ---------------------------------------------------

    #[test]
    fn orders_by_day_then_meal_and_undated_last() {
        let mut items = [
            ("undated-a", None, None),
            ("21-none", Some(day("2026-11-21")), None),
            ("21-dinner", Some(day("2026-11-21")), Some("dinner")),
            ("undated-b", None, None),
            ("20-dinner", Some(day("2026-11-20")), Some("dinner")),
            ("21-lunch", Some(day("2026-11-21")), Some("lunch")),
            ("19-none", Some(day("2026-11-19")), None),
        ];
        items.sort_by_key(|(_, on, meal)| plan_key(*on, *meal));
        let order: Vec<&str> = items.iter().map(|(name, _, _)| *name).collect();
        assert_eq!(
            order,
            [
                "19-none",
                "20-dinner",
                "21-lunch",
                "21-dinner",
                "21-none",
                "undated-a",
                "undated-b",
            ]
        );
    }
}
