//! Marking a planned recipe done or abandoned (#473). Each action has a
//! read-only preview (`GET`) and an application (`POST`) carrying the
//! user's corrections, on `/groups/:id/recipes/:recipe_id/selection/done`
//! and `…/selection/abandoned`. Like the other selection endpoints they name
//! the recipe, whose `planned` selection is unique; without one, or for a
//! recipe of another group, the answer is 404.
//!
//! **Done.** The preview lists every ingredient of the recipe, its quantity
//! scaled to the selection's number of people as the grocery list scales it
//! (`grocery_list::planned::scale_quantity`), and the stock lines (`lots`)
//! the quantity is proposed from (`propose`): every line of its name and
//! unit, the one expiring first first (`lots_in_order`), as many as needed,
//! so 100 g + 500 g cover 400 g. The proposal is capped only when they all
//! together hold less. Without a line of that name, without a quantity or
//! in another unit (no conversion), 0 is proposed with a warning, from the
//! line `match_stock` picks. The application takes one quantity per stock
//! line, in that line's unit, so an ingredient spread over several lots is
//! several lines, each checked against its own lot. A quantity may exceed
//! the recipe's (user precision of 2026-10-21); 0 or `exclude` removes
//! nothing. A quantity above what the line holds now
//! is **refused** (409 `insufficient_stock`), never capped: the preview
//! already capped, so a larger quantity means the stock changed since, or a
//! request asking for more than there is. A line gone since is 409
//! `stock_item_gone`. The stock is decremented, a `meal_history` entry is
//! inserted as « log a meal » does, and the selection becomes `done`.
//!
//! **Other products (#540).** The application also takes `others`, products
//! used that the recipe does not call for: `{stock_item_id, quantity}`, the
//! quantity above 0, in the line's unit, refused above what the line holds
//! as above. A line of `removals`, on the other hand, must name one of the
//! recipe's ingredients (`article_key`), or 400 `stock_item_not_in_recipe`,
//! as before #540. A line of `others` the group's stock does not hold
//! (another group's, or none) is 404. With `add_to_recipe`, each product
//! becomes an ingredient of the recipe (`ingredients_to_add`): its line's
//! name and unit, the quantity used brought back to the recipe's number of
//! people, unless the recipe already has an ingredient of that name
//! (`article_key`), so that replaying adds it once. Changing the recipe
//! stays for its creator, an admin or the owner (`can_modify`, as for
//! `PATCH`): anyone else gets 403 for an `add_to_recipe` that adds
//! something (products the recipe already names add nothing), and the
//! preview says whether the caller may (`can_edit_recipe`).
//!
//! **Updating the recipe (#558).** The application also takes
//! `recipe_updates`, `{ingredient_id, action}` where `action` is
//! `set_quantity` (with `quantity`, above 0, for the recipe's number of
//! people), `remove` or `make_optional`: what the cook learned for next
//! time. They are applied in the same transaction as the removals, for the
//! same people as `add_to_recipe` (403 otherwise, and nothing applied); an
//! ingredient the recipe does not have, of another recipe or group, is 404
//! and nothing is applied. The preview gives what the page needs to propose
//! them: each ingredient's quantity as written (`recipe_quantity`) and the
//! recipe's number of people (`recipe_servings`). Any change to the recipe
//! first locks its row, as `PATCH` does.
//!
//! **Abandoned.** The preview lists the unchecked grocery items whose
//! `source_recipe_id` names the recipe. The application takes the ids to
//! remove (the others are kept); an id that is no longer such an item
//! (deleted, checked, or never one) is 409 `grocery_item_changed`. The
//! selection becomes `abandoned`.
//!
//! Each application is one transaction that first locks the selection, so
//! two applications at once end with one 200 and one 404. Any member may
//! apply either action, like logging a meal. Removing from the stock is a
//! quantity-only adjustment, already open to any member (#39). Removing
//! grocery items is not otherwise: deleting one directly is for its creator,
//! an admin or the owner (`grocery_list::can_modify`). Through an abandon it
//! is open to any member, by user arbitration of 2026-10-25, as for the
//! reception of the groceries (#471), and `can_modify` is deliberately not
//! called: only unchecked items generated for the abandoned recipe can go,
//! and only those the request names.

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use manage_our_home_shared::validation::stocks::{article_key, normalize_unit};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::grocery_list::planned::{scale_quantity, without_binary_noise};
use crate::groups::require_role;
use crate::recipes::can_modify;
use crate::recipes::meal_history::MealHistoryEntryResponse;
use crate::recipes::selections::SelectionResponse;
use crate::stocks::items::{day_or_today, today, StockItemResponse, StockItemRow};
use crate::AppState;

// -- Pure part ----------------------------------------------------------------

/// A stock line an ingredient could be taken from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StockLine<'a> {
    pub id: Uuid,
    pub name: &'a str,
    pub unit: &'a str,
    pub quantity: f64,
    pub expires_on: Option<NaiveDate>,
}

/// An ingredient of the recipe, its quantity already scaled to the
/// selection's number of people.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Ingredient<'a> {
    pub name: &'a str,
    pub quantity: Option<f64>,
    pub unit: Option<&'a str>,
}

/// Why a proposed quantity is not the recipe's. On the wire: `"no_stock"`,
/// `"unit_mismatch"`, `"no_quantity"`, `"capped"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalWarning {
    /// No stock line has the ingredient's name: nothing to remove.
    NoStock,
    /// The line is in another unit; there is no conversion, so 0.
    UnitMismatch,
    /// The recipe gives no quantity, so 0.
    NoQuantity,
    /// The lots of its name and unit hold less, together, than the recipe
    /// calls for: all they hold is proposed.
    Capped,
}

/// What the preview proposes to take from one stock line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Take {
    pub stock_item_id: Uuid,
    pub quantity: f64,
}

/// What the preview proposes to take from the stock for one ingredient:
/// one or several lines (`lots`) and their total (`quantity`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Proposal {
    pub lots: Vec<Take>,
    pub quantity: f64,
    pub warning: Option<RemovalWarning>,
}

/// One corrected line of a `done` request: the quantity to take from a
/// stock line, in its unit.
#[derive(Debug, Clone, Deserialize)]
pub struct Removal {
    pub stock_item_id: Uuid,
    pub quantity: f64,
    /// `true` removes nothing from this line, whatever its quantity.
    #[serde(default)]
    pub exclude: bool,
}

/// The stock line an ingredient matches: one of the same name
/// (`article_key`), preferring the ingredient's unit, then a line that is
/// not empty, then the earliest expiry date (undated lines last), then the
/// smallest id. It decides the warning, and is the line shown when nothing
/// is proposed; the quantity is spread by `lots_in_order`. `unit` is the
/// ingredient's, already normalized.
pub(crate) fn match_stock<'a, 's>(
    name: &str,
    unit: &str,
    stock: &'a [StockLine<'s>],
) -> Option<&'a StockLine<'s>> {
    let (name, unit) = (article_key(name), article_key(unit));
    stock
        .iter()
        .filter(|s| article_key(s.name) == name)
        // `false` sorts first.
        .min_by_key(|s| {
            (
                article_key(s.unit) != unit,
                s.quantity <= 0.0,
                s.expires_on.is_none(),
                s.expires_on,
                s.id,
            )
        })
}

/// What can be taken of `wanted` from a line holding `available`: never
/// more than it holds, never below 0.
pub(crate) fn cap(wanted: f64, available: f64) -> f64 {
    wanted.min(available).max(0.0)
}

/// What a line holding `available` keeps once `removed` is taken, or `None`
/// when it holds less. Both the stock and the result are read without their
/// binary noise (`without_binary_noise`): `1.0 - 0.9` keeps `0.1`, not
/// `0.09999999999999998`, from which a later `0.1` could not be taken.
pub(crate) fn remaining_after(available: f64, removed: f64) -> Option<f64> {
    let available = without_binary_noise(available);
    (removed <= available).then(|| without_binary_noise(available - removed).max(0.0))
}

/// The lines of the ingredient's name and unit, in the order they are taken
/// from: the earliest expiry date first, undated lines last, then the
/// smallest id. What expires first is used first, so the lot left over is
/// the one that keeps longest. `unit` is the ingredient's, already
/// normalized.
pub(crate) fn lots_in_order<'a, 's>(
    name: &str,
    unit: &str,
    stock: &'a [StockLine<'s>],
) -> Vec<&'a StockLine<'s>> {
    let (name, unit) = (article_key(name), article_key(unit));
    let mut lots: Vec<_> = stock
        .iter()
        .filter(|s| article_key(s.name) == name && article_key(s.unit) == unit)
        .collect();
    lots.sort_by_key(|s| (s.expires_on.is_none(), s.expires_on, s.id));
    lots
}

/// The proposal for each ingredient, in order. An ingredient is taken from
/// as many lines of its name and unit as it needs, in `lots_in_order`; it
/// is capped only when they all together hold less. Two ingredients
/// matching the same lines share what they hold: the second is capped at
/// what the first left.
pub(crate) fn propose(ingredients: &[Ingredient<'_>], stock: &[StockLine<'_>]) -> Vec<Proposal> {
    let mut remaining: Vec<(Uuid, f64)> = stock.iter().map(|s| (s.id, s.quantity)).collect();
    ingredients
        .iter()
        .map(|ing| {
            let unit = normalize_unit(ing.unit.unwrap_or(""));
            let Some(line) = match_stock(ing.name, &unit, stock) else {
                return Proposal {
                    lots: vec![],
                    quantity: 0.0,
                    warning: Some(RemovalWarning::NoStock),
                };
            };
            let nothing_from_line = || {
                vec![Take {
                    stock_item_id: line.id,
                    quantity: 0.0,
                }]
            };
            let nothing = |warning| Proposal {
                lots: nothing_from_line(),
                quantity: 0.0,
                warning: Some(warning),
            };
            if article_key(line.unit) != article_key(&unit) {
                return nothing(RemovalWarning::UnitMismatch);
            }
            let Some(wanted) = ing.quantity else {
                return nothing(RemovalWarning::NoQuantity);
            };
            let mut lots = Vec::new();
            let mut still = wanted;
            let mut total = 0.0;
            for lot in lots_in_order(ing.name, &unit, stock) {
                if still <= 0.0 {
                    break;
                }
                let left = remaining
                    .iter_mut()
                    .find(|(id, _)| *id == lot.id)
                    .map(|(_, q)| q)
                    .expect("every stock line is in `remaining`");
                let quantity = cap(still, without_binary_noise(*left));
                if quantity <= 0.0 {
                    continue;
                }
                *left = remaining_after(*left, quantity).unwrap_or(0.0);
                still = without_binary_noise(still - quantity);
                total = without_binary_noise(total + quantity);
                lots.push(Take {
                    stock_item_id: lot.id,
                    quantity,
                });
            }
            if lots.is_empty() {
                lots = nothing_from_line();
            }
            Proposal {
                lots,
                quantity: total,
                warning: (total < wanted).then_some(RemovalWarning::Capped),
            }
        })
        .collect()
}

/// The `(stock line, quantity)` pairs a `done` request removes: the lines
/// neither excluded nor at 0. Each line once, each quantity finite and not
/// negative, or 400.
pub(crate) fn removals_to_apply(removals: &[Removal]) -> AppResult<Vec<(Uuid, f64)>> {
    let mut seen = HashSet::with_capacity(removals.len());
    for r in removals {
        if !seen.insert(r.stock_item_id) {
            return Err(AppError::BadRequest("duplicate_stock_item".into()));
        }
        if !r.quantity.is_finite() || r.quantity < 0.0 {
            return Err(AppError::BadRequest("invalid_quantity".into()));
        }
    }
    Ok(removals
        .iter()
        .filter(|r| !r.exclude && r.quantity > 0.0)
        .map(|r| (r.stock_item_id, r.quantity))
        .collect())
}

/// The grocery items an `abandoned` request removes: each once, or 400.
pub(crate) fn check_grocery_ids(ids: &[Uuid]) -> AppResult<()> {
    let mut seen = HashSet::with_capacity(ids.len());
    if ids.iter().all(|id| seen.insert(id)) {
        Ok(())
    } else {
        Err(AppError::BadRequest("duplicate_grocery_item".into()))
    }
}

/// A product used that the recipe does not call for (#540): the quantity
/// to take from a stock line, in its unit.
#[derive(Debug, Clone, Deserialize)]
pub struct OtherRemoval {
    pub stock_item_id: Uuid,
    pub quantity: f64,
}

/// Whether a stock line's name is one of the recipe's ingredients
/// (`article_key`): a line of `removals` takes only from those.
pub(crate) fn names_an_ingredient(name: &str, ingredients: &[&str]) -> bool {
    let key = article_key(name);
    ingredients.iter().any(|i| article_key(i) == key)
}

/// The `(stock line, quantity)` pairs of a `done` request's other products.
/// Each quantity above 0, and each line once across `removals` and
/// `others`, or 400.
pub(crate) fn check_others(
    removals: &[Removal],
    others: &[OtherRemoval],
) -> AppResult<Vec<(Uuid, f64)>> {
    let mut seen: HashSet<Uuid> = removals.iter().map(|r| r.stock_item_id).collect();
    for o in others {
        if !seen.insert(o.stock_item_id) {
            return Err(AppError::BadRequest("duplicate_stock_item".into()));
        }
        if !o.quantity.is_finite() || o.quantity <= 0.0 {
            return Err(AppError::BadRequest("invalid_quantity".into()));
        }
    }
    Ok(others
        .iter()
        .map(|o| (o.stock_item_id, o.quantity))
        .collect())
}

/// An other product as taken from the stock: its line's name and unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Used<'a> {
    pub name: &'a str,
    pub unit: &'a str,
    pub quantity: f64,
}

/// An ingredient to add to the recipe.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NewIngredient {
    pub name: String,
    pub quantity: f64,
    pub unit: String,
}

/// The ingredients `add_to_recipe` adds: one per product name
/// (`article_key`) the recipe does not have yet (`existing`), in the order
/// the products were given, named and counted in the unit of its first
/// line. Several lines of that name and unit add up; a line in another unit
/// cannot be added to them (no conversion) and is left out. The quantity
/// used, for the selection's number of people (`cooked_for`), is brought
/// back to the recipe's (`written_for`), as the preview scales the other
/// way (`scale_quantity`); as used when either is missing.
pub(crate) fn ingredients_to_add(
    used: &[Used<'_>],
    existing: &[&str],
    cooked_for: Option<i32>,
    written_for: Option<i32>,
) -> Vec<NewIngredient> {
    let existing: HashSet<String> = existing.iter().map(|n| article_key(n)).collect();
    let mut added: Vec<(String, NewIngredient)> = Vec::new();
    for u in used {
        let key = article_key(u.name);
        if existing.contains(&key) {
            continue;
        }
        match added.iter_mut().find(|(k, _)| *k == key) {
            Some((_, ing)) if article_key(&ing.unit) == article_key(u.unit) => {
                ing.quantity = without_binary_noise(ing.quantity + u.quantity);
            }
            Some(_) => {}
            None => added.push((
                key,
                NewIngredient {
                    name: u.name.trim().to_string(),
                    quantity: u.quantity,
                    unit: u.unit.trim().to_string(),
                },
            )),
        }
    }
    added
        .into_iter()
        .map(|(_, ing)| NewIngredient {
            quantity: scale_quantity(Some(ing.quantity), cooked_for, written_for)
                .unwrap_or(ing.quantity),
            ..ing
        })
        .collect()
}

/// What a `done` request changes in one of the recipe's ingredients
/// (#558). On the wire, beside `ingredient_id`: `"action": "set_quantity"`
/// with `"quantity"`, `"action": "remove"` or `"action": "make_optional"`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RecipeChange {
    /// The ingredient's new quantity, for the recipe's number of people,
    /// in its unit.
    SetQuantity { quantity: f64 },
    /// The ingredient leaves the recipe.
    Remove,
    /// The ingredient becomes optional; its quantity is kept.
    MakeOptional,
}

/// One change to the recipe a `done` request carries (#558).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RecipeUpdate {
    pub ingredient_id: Uuid,
    #[serde(flatten)]
    pub change: RecipeChange,
}

/// The changes to the recipe a `done` request carries: each ingredient
/// once, and a new quantity finite and above 0 (an ingredient no longer
/// used is removed or made optional, not set to 0), or 400.
pub(crate) fn check_recipe_updates(updates: &[RecipeUpdate]) -> AppResult<()> {
    let mut seen = HashSet::with_capacity(updates.len());
    for u in updates {
        if !seen.insert(u.ingredient_id) {
            return Err(AppError::BadRequest("duplicate_ingredient".into()));
        }
        if let RecipeChange::SetQuantity { quantity } = u.change {
            if !quantity.is_finite() || quantity <= 0.0 {
                return Err(AppError::BadRequest("invalid_quantity".into()));
            }
        }
    }
    Ok(())
}

// -- Handlers -----------------------------------------------------------------

/// The recipe's `planned` selection, with the recipe's own number of people.
struct PlannedRow {
    id: Uuid,
    recipe_id: Uuid,
    recipe_name: String,
    status: String,
    servings: Option<i32>,
    created_by: Uuid,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    recipe_servings: Option<i32>,
    recipe_created_by: Uuid,
}

impl PlannedRow {
    fn response(&self) -> SelectionResponse {
        SelectionResponse {
            id: self.id,
            recipe_id: self.recipe_id,
            recipe_name: self.recipe_name.clone(),
            status: self.status.clone(),
            servings: self.servings,
            created_by: self.created_by,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// The recipe's `planned` selection, or 404 (none, or a recipe of another
/// group). `lock` takes it `FOR UPDATE`, for an application.
async fn planned_selection(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    recipe_id: Uuid,
    lock: bool,
) -> AppResult<PlannedRow> {
    let row = if lock {
        sqlx::query_as!(
            PlannedRow,
            r#"
            SELECT s.id, s.recipe_id, r.name AS recipe_name, s.status, s.servings,
                   s.created_by, s.created_at, s.updated_at, r.servings AS recipe_servings,
                   r.created_by AS recipe_created_by
            FROM recipe_selections s
            JOIN recipes r ON r.id = s.recipe_id
            WHERE s.recipe_id = $1 AND s.group_id = $2 AND s.status = 'planned'
            FOR UPDATE OF s
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
            SELECT s.id, s.recipe_id, r.name AS recipe_name, s.status, s.servings,
                   s.created_by, s.created_at, s.updated_at, r.servings AS recipe_servings,
                   r.created_by AS recipe_created_by
            FROM recipe_selections s
            JOIN recipes r ON r.id = s.recipe_id
            WHERE s.recipe_id = $1 AND s.group_id = $2 AND s.status = 'planned'
            "#,
            recipe_id,
            group_id,
        )
        .fetch_optional(&mut **tx)
        .await?
    };
    row.ok_or(AppError::NotFound)
}

/// Moves the selection to `status` (`done` or `abandoned`).
async fn close_selection(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    selection: &PlannedRow,
    status: &str,
) -> AppResult<SelectionResponse> {
    let updated_at = sqlx::query_scalar!(
        r#"UPDATE recipe_selections SET status = $2, updated_at = now()
           WHERE id = $1 RETURNING updated_at"#,
        selection.id,
        status,
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(SelectionResponse {
        status: status.to_string(),
        updated_at,
        ..selection.response()
    })
}

struct IngredientRow {
    id: Uuid,
    name: String,
    quantity: Option<f64>,
    unit: Option<String>,
    is_optional: bool,
}

async fn ingredients(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    recipe_id: Uuid,
) -> AppResult<Vec<IngredientRow>> {
    Ok(sqlx::query_as!(
        IngredientRow,
        r#"SELECT id, name, quantity, unit, is_optional FROM recipe_ingredients
           WHERE recipe_id = $1 AND group_id = $2 ORDER BY name, id"#,
        recipe_id,
        group_id,
    )
    .fetch_all(&mut **tx)
    .await?)
}

/// One ingredient of the `done` preview.
#[derive(Serialize)]
pub struct DoneLineResponse {
    pub ingredient_id: Uuid,
    pub name: String,
    /// The recipe's quantity, scaled to the selection's number of people.
    pub quantity: Option<f64>,
    /// The recipe's quantity as written, not scaled (#558).
    pub recipe_quantity: Option<f64>,
    pub unit: Option<String>,
    pub is_optional: bool,
    /// The stock lines proposed, in the order they are taken from; empty
    /// when no line has the ingredient's name. A line at 0 (`unit_mismatch`,
    /// `no_quantity`, or only empty lines) shows the matched line.
    pub lots: Vec<DoneLotResponse>,
    /// The lots' total, in their unit.
    pub proposed_quantity: f64,
    pub warning: Option<RemovalWarning>,
}

/// One stock line of a `done` preview, and what is proposed from it.
#[derive(Serialize)]
pub struct DoneLotResponse {
    pub stock_item: StockItemResponse,
    /// In the stock line's unit.
    pub quantity: f64,
}

/// `GET …/selection/done`: what marking the recipe done would remove.
pub async fn preview_done(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let role = require_role(&mut tx, group_id, auth.user_id).await?;
    let selection = planned_selection(&mut tx, group_id, recipe_id, false).await?;
    let can_edit_recipe = can_modify(&role, selection.recipe_created_by == auth.user_id);
    let rows = ingredients(&mut tx, group_id, recipe_id).await?;
    let stock = sqlx::query_as!(
        StockItemRow,
        r#"SELECT id, group_id, created_by, name, category, quantity, unit, reorder_threshold, expires_on, barcode
           FROM stock_items WHERE group_id = $1"#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let scaled: Vec<Option<f64>> = rows
        .iter()
        .map(|r| scale_quantity(r.quantity, selection.recipe_servings, selection.servings))
        .collect();
    let wanted: Vec<Ingredient<'_>> = rows
        .iter()
        .zip(&scaled)
        .map(|(r, q)| Ingredient {
            name: &r.name,
            quantity: *q,
            unit: r.unit.as_deref(),
        })
        .collect();
    let lines: Vec<StockLine<'_>> = stock
        .iter()
        .map(|s| StockLine {
            id: s.id,
            name: &s.name,
            unit: &s.unit,
            quantity: s.quantity,
            expires_on: s.expires_on,
        })
        .collect();
    let proposals = propose(&wanted, &lines);

    let today = today();
    let ingredients: Vec<DoneLineResponse> = rows
        .iter()
        .zip(scaled)
        .zip(proposals)
        .map(|((r, quantity), p)| DoneLineResponse {
            ingredient_id: r.id,
            name: r.name.clone(),
            quantity,
            recipe_quantity: r.quantity,
            unit: r.unit.clone(),
            is_optional: r.is_optional,
            lots: p
                .lots
                .iter()
                .filter_map(|t| {
                    stock
                        .iter()
                        .find(|s| s.id == t.stock_item_id)
                        .map(|s| DoneLotResponse {
                            stock_item: StockItemResponse::from_row(s.clone(), today),
                            quantity: t.quantity,
                        })
                })
                .collect(),
            proposed_quantity: p.quantity,
            warning: p.warning,
        })
        .collect();

    Ok(Json(json!({
        "selection": selection.response(),
        "ingredients": ingredients,
        "can_edit_recipe": can_edit_recipe,
        "recipe_servings": selection.recipe_servings,
    })))
}

/// `POST …/selection/done` body.
#[derive(Deserialize)]
pub struct DoneRequest {
    /// One line per stock line to take from; a line left out is untouched.
    #[serde(default)]
    pub removals: Vec<Removal>,
    /// The `meal_history` date; today when omitted.
    pub eaten_on: Option<NaiveDate>,
    /// Products used that the recipe does not call for (#540).
    #[serde(default)]
    pub others: Vec<OtherRemoval>,
    /// Adds `others` to the recipe, for the recipe's creator, an admin or
    /// the owner (`can_modify`).
    #[serde(default)]
    pub add_to_recipe: bool,
    /// Changes to the recipe's ingredients (#558), for the same people as
    /// `add_to_recipe`.
    #[serde(default)]
    pub recipe_updates: Vec<RecipeUpdate>,
}

/// An ingredient added by `add_to_recipe`.
#[derive(Serialize)]
pub struct AddedIngredientResponse {
    pub id: Uuid,
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
}

/// `POST …/selection/done`: takes the corrected quantities from the stock,
/// logs the meal and closes the selection, in one transaction.
pub async fn apply_done(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<DoneRequest>,
) -> AppResult<impl IntoResponse> {
    let mut to_apply = removals_to_apply(&body.removals)?;
    let others = check_others(&body.removals, &body.others)?;
    check_recipe_updates(&body.recipe_updates)?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let role = require_role(&mut tx, group_id, auth.user_id).await?;
    let selection = planned_selection(&mut tx, group_id, recipe_id, true).await?;
    let may_change = can_modify(&role, selection.recipe_created_by == auth.user_id);
    if !body.recipe_updates.is_empty() && !may_change {
        return Err(AppError::Forbidden);
    }
    let add_to_recipe = body.add_to_recipe && !others.is_empty();
    // Who may not change the recipe gets 403 for `add_to_recipe` only once
    // it is known to add something (below); it writes nothing to it.
    if (add_to_recipe && may_change) || !body.recipe_updates.is_empty() {
        // Locked as `PATCH` locks it, before its ingredients are read: a
        // `PATCH` replacing them meanwhile would otherwise leave an
        // ingredient added twice, or a change to one it deleted.
        sqlx::query_scalar!(
            "SELECT id FROM recipes WHERE id = $1 AND group_id = $2 FOR UPDATE",
            recipe_id,
            group_id,
        )
        .fetch_one(&mut *tx)
        .await?;
    }
    let rows = ingredients(&mut tx, group_id, recipe_id).await?;
    // An ingredient the recipe does not have: another recipe's, another
    // group's, or none. Nothing has been written yet.
    if body
        .recipe_updates
        .iter()
        .any(|u| !rows.iter().any(|r| r.id == u.ingredient_id))
    {
        return Err(AppError::NotFound);
    }

    // Locked in id order, so two applications taking from the same lines
    // cannot deadlock.
    let ids: Vec<Uuid> = to_apply.iter().chain(&others).map(|(id, _)| *id).collect();
    let locked = sqlx::query_as!(
        StockItemRow,
        r#"SELECT id, group_id, created_by, name, category, quantity, unit, reorder_threshold, expires_on, barcode
           FROM stock_items WHERE group_id = $1 AND id = ANY($2) ORDER BY id FOR UPDATE"#,
        group_id,
        &ids,
    )
    .fetch_all(&mut *tx)
    .await?;
    // An other product the group's stock does not hold: another group's,
    // or none. Nothing has been written yet.
    if others
        .iter()
        .any(|(id, _)| !locked.iter().any(|s| s.id == *id))
    {
        return Err(AppError::NotFound);
    }
    let used: Vec<Used<'_>> = others
        .iter()
        .filter_map(|(id, quantity)| {
            let line = locked.iter().find(|s| s.id == *id)?;
            Some(Used {
                name: &line.name,
                unit: &line.unit,
                quantity: *quantity,
            })
        })
        .collect();
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    // A line of `removals` the recipe does not call for: an ingredient
    // renamed or removed since the preview, or a product for `others`.
    if to_apply.iter().any(|(id, _)| {
        locked
            .iter()
            .find(|s| s.id == *id)
            .is_some_and(|line| !names_an_ingredient(&line.name, &names))
    }) {
        return Err(AppError::BadRequest("stock_item_not_in_recipe".into()));
    }
    let to_add = if add_to_recipe {
        ingredients_to_add(&used, &names, selection.servings, selection.recipe_servings)
    } else {
        vec![]
    };
    if !to_add.is_empty() && !may_change {
        return Err(AppError::Forbidden);
    }
    to_apply.extend(others.iter().copied());
    let mut remaining = Vec::with_capacity(to_apply.len());
    for (id, quantity) in &to_apply {
        let Some(line) = locked.iter().find(|s| s.id == *id) else {
            return Err(AppError::ConflictJson(
                json!({ "error": "stock_item_gone", "stock_item_id": id }),
            ));
        };
        let Some(left) = remaining_after(line.quantity, *quantity) else {
            return Err(AppError::ConflictJson(json!({
                "error": "insufficient_stock",
                "stock_item_id": id,
                // As `remaining_after` compared it: without its binary noise.
                "available": without_binary_noise(line.quantity),
            })));
        };
        remaining.push((*id, left));
    }

    let mut updated = Vec::with_capacity(to_apply.len());
    for (id, left) in &remaining {
        // The lines are locked since they were read, so the quantity they
        // keep is computed here (`remaining_after`: never below 0, without
        // its binary noise) rather than as `quantity - $3` in SQL.
        let row = sqlx::query_as!(
            StockItemRow,
            r#"
            UPDATE stock_items SET quantity = $3, updated_at = now()
            WHERE id = $1 AND group_id = $2
            RETURNING id, group_id, created_by, name, category, quantity, unit, reorder_threshold, expires_on, barcode
            "#,
            id,
            group_id,
            left,
        )
        .fetch_one(&mut *tx)
        .await?;
        updated.push(row);
    }

    let meal = sqlx::query_as!(
        MealHistoryEntryResponse,
        r#"
        INSERT INTO meal_history (group_id, recipe_id, eaten_on, created_by)
        VALUES ($1, $2, $3, $4)
        RETURNING id, recipe_id, eaten_on, created_by, created_at
        "#,
        group_id,
        recipe_id,
        day_or_today(body.eaten_on, Utc::now()),
        auth.user_id,
    )
    .fetch_one(&mut *tx)
    .await?;

    let mut added = Vec::new();
    for ing in to_add {
        let row = sqlx::query_as!(
            AddedIngredientResponse,
            r#"
            INSERT INTO recipe_ingredients (recipe_id, group_id, name, quantity, unit)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id, name, quantity, unit
            "#,
            recipe_id,
            group_id,
            ing.name,
            ing.quantity,
            ing.unit,
        )
        .fetch_one(&mut *tx)
        .await?;
        added.push(row);
    }
    for u in &body.recipe_updates {
        match u.change {
            RecipeChange::SetQuantity { quantity } => {
                sqlx::query!(
                    "UPDATE recipe_ingredients SET quantity = $3 WHERE id = $1 AND group_id = $2",
                    u.ingredient_id,
                    group_id,
                    quantity,
                )
                .execute(&mut *tx)
                .await?
            }
            RecipeChange::Remove => {
                sqlx::query!(
                    "DELETE FROM recipe_ingredients WHERE id = $1 AND group_id = $2",
                    u.ingredient_id,
                    group_id,
                )
                .execute(&mut *tx)
                .await?
            }
            RecipeChange::MakeOptional => {
                sqlx::query!(
                "UPDATE recipe_ingredients SET is_optional = true WHERE id = $1 AND group_id = $2",
                u.ingredient_id,
                group_id,
            )
                .execute(&mut *tx)
                .await?
            }
        };
    }
    if !added.is_empty() || !body.recipe_updates.is_empty() {
        sqlx::query!(
            "UPDATE recipes SET updated_at = now() WHERE id = $1 AND group_id = $2",
            recipe_id,
            group_id,
        )
        .execute(&mut *tx)
        .await?;
    }
    let selection = close_selection(&mut tx, &selection, "done").await?;
    tx.commit().await?;

    let today = today();
    let stock_items: Vec<StockItemResponse> = updated
        .into_iter()
        .map(|r| StockItemResponse::from_row(r, today))
        .collect();
    Ok(Json(json!({
        "selection": selection,
        "meal": meal,
        "stock_items": stock_items,
        "added_ingredients": added,
    })))
}

/// One grocery item of the `abandoned` preview.
#[derive(Serialize)]
pub struct AbandonLineResponse {
    pub id: Uuid,
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
}

/// `GET …/selection/abandoned`: the unchecked grocery items generated for
/// the recipe, each proposed for removal.
pub async fn preview_abandoned(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    let selection = planned_selection(&mut tx, group_id, recipe_id, false).await?;
    let items = sqlx::query_as!(
        AbandonLineResponse,
        r#"SELECT id, name, quantity, unit FROM grocery_items
           WHERE group_id = $1 AND source_recipe_id = $2 AND checked = false
           ORDER BY name, id"#,
        group_id,
        recipe_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(json!({
        "selection": selection.response(),
        "items": items,
    })))
}

/// `POST …/selection/abandoned` body.
#[derive(Deserialize)]
pub struct AbandonRequest {
    /// The grocery items to remove; the preview's others are kept. Required,
    /// so that a request that forgot it does not keep everything silently.
    pub remove: Vec<Uuid>,
}

/// `POST …/selection/abandoned`: removes the chosen grocery items and
/// closes the selection, in one transaction.
pub async fn apply_abandoned(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<AbandonRequest>,
) -> AppResult<impl IntoResponse> {
    check_grocery_ids(&body.remove)?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    let selection = planned_selection(&mut tx, group_id, recipe_id, true).await?;

    let found: Vec<Uuid> = sqlx::query_scalar!(
        r#"SELECT id FROM grocery_items
           WHERE group_id = $1 AND id = ANY($2) AND source_recipe_id = $3 AND checked = false
           ORDER BY id FOR UPDATE"#,
        group_id,
        &body.remove,
        recipe_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    let changed: Vec<Uuid> = body
        .remove
        .iter()
        .filter(|id| !found.contains(id))
        .copied()
        .collect();
    if !changed.is_empty() {
        return Err(AppError::ConflictJson(
            json!({ "error": "grocery_item_changed", "grocery_item_ids": changed }),
        ));
    }
    sqlx::query!(
        "DELETE FROM grocery_items WHERE group_id = $1 AND id = ANY($2)",
        group_id,
        &body.remove,
    )
    .execute(&mut *tx)
    .await?;
    let selection = close_selection(&mut tx, &selection, "abandoned").await?;
    tx.commit().await?;

    Ok(Json(json!({
        "selection": selection,
        "removed": body.remove,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn line<'a>(
        n: u128,
        name: &'a str,
        unit: &'a str,
        quantity: f64,
        expires_on: Option<NaiveDate>,
    ) -> StockLine<'a> {
        StockLine {
            id: id(n),
            name,
            unit,
            quantity,
            expires_on,
        }
    }

    fn ing<'a>(name: &'a str, quantity: Option<f64>, unit: Option<&'a str>) -> Ingredient<'a> {
        Ingredient {
            name,
            quantity,
            unit,
        }
    }

    fn take(n: u128, quantity: f64) -> Take {
        Take {
            stock_item_id: id(n),
            quantity,
        }
    }

    fn date(d: u32) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(2026, 11, d)
    }

    fn bad_request<T: std::fmt::Debug>(r: AppResult<T>) -> String {
        match r {
            Err(AppError::BadRequest(code)) => code,
            other => panic!("expected a 400, got {other:?}"),
        }
    }

    // -- match_stock ---------------------------------------------------------

    #[test]
    fn matches_by_name_trimmed_and_case_insensitive() {
        let stock = [
            line(1, "Pain", "unité", 1.0, None),
            line(2, " Farine ", "g", 500.0, None),
        ];
        assert_eq!(
            match_stock("farine", "g", &stock).map(|s| s.id),
            Some(id(2))
        );
    }

    #[test]
    fn no_line_of_that_name_matches_nothing() {
        let stock = [line(1, "Farine de blé", "g", 500.0, None)];
        assert_eq!(match_stock("Farine", "g", &stock), None);
        assert_eq!(match_stock("Farine", "g", &[]), None);
    }

    #[test]
    fn a_line_in_another_unit_still_matches_when_alone() {
        let stock = [line(1, "Farine", "kg", 1.0, None)];
        assert_eq!(
            match_stock("Farine", "g", &stock).map(|s| s.id),
            Some(id(1))
        );
    }

    #[test]
    fn a_line_in_the_same_unit_is_preferred() {
        let stock = [
            line(1, "Farine", "kg", 1.0, date(1)),
            line(2, "Farine", "G", 500.0, date(9)),
        ];
        assert_eq!(
            match_stock("Farine", "g", &stock).map(|s| s.id),
            Some(id(2))
        );
    }

    #[test]
    fn the_earliest_date_goes_first_and_undated_last() {
        let stock = [
            line(1, "Lait", "L", 1.0, None),
            line(2, "Lait", "L", 1.0, date(9)),
            line(3, "Lait", "L", 1.0, date(3)),
        ];
        assert_eq!(match_stock("Lait", "L", &stock).map(|s| s.id), Some(id(3)));
        let undated = [
            line(5, "Lait", "L", 1.0, None),
            line(4, "Lait", "L", 1.0, None),
        ];
        assert_eq!(
            match_stock("Lait", "L", &undated).map(|s| s.id),
            Some(id(4))
        );
    }

    #[test]
    fn a_line_with_stock_goes_before_an_empty_one() {
        let stock = [
            line(1, "Oeufs", "unité", 0.0, date(1)),
            line(2, "Oeufs", "unité", 6.0, date(9)),
        ];
        assert_eq!(
            match_stock("Oeufs", "unité", &stock).map(|s| s.id),
            Some(id(2))
        );
        // The unit still comes first: an empty line in the unit beats a
        // full one in another.
        let units = [
            line(1, "Oeufs", "unité", 0.0, None),
            line(2, "Oeufs", "boîte", 1.0, None),
        ];
        assert_eq!(
            match_stock("Oeufs", "unité", &units).map(|s| s.id),
            Some(id(1))
        );
    }

    // -- remaining_after -----------------------------------------------------

    #[test]
    fn remaining_after_drops_the_binary_noise() {
        assert_eq!(remaining_after(1.0, 0.9), Some(0.1));
        assert_eq!(remaining_after(0.3, 0.1), Some(0.2));
        assert_eq!(remaining_after(2.0, 2.0), Some(0.0));
    }

    #[test]
    fn remaining_after_reads_a_noisy_stock_as_its_value() {
        // 1.0 - 0.9 as `float8` computes it.
        assert_eq!(remaining_after(0.09999999999999998, 0.1), Some(0.0));
    }

    #[test]
    fn remaining_after_refuses_more_than_the_stock() {
        assert_eq!(remaining_after(0.5, 0.6), None);
        assert_eq!(remaining_after(0.0, 0.1), None);
    }

    #[test]
    fn a_noisy_stock_is_proposed_as_its_value() {
        let stock = [line(1, "Lait", "L", 0.09999999999999998, None)];
        let got = propose(&[ing("Lait", Some(0.1), Some("L"))], &stock);
        assert_eq!(got[0].quantity, 0.1);
        assert_eq!(got[0].warning, None);
    }

    // -- cap -----------------------------------------------------------------

    #[test]
    fn cap_keeps_what_the_stock_holds() {
        assert_eq!(cap(200.0, 500.0), 200.0);
        assert_eq!(cap(500.0, 500.0), 500.0);
        assert_eq!(cap(800.0, 500.0), 500.0);
    }

    #[test]
    fn cap_never_goes_below_zero() {
        assert_eq!(cap(200.0, 0.0), 0.0);
        assert_eq!(cap(200.0, -3.0), 0.0);
        assert_eq!(cap(-1.0, 5.0), 0.0);
    }

    // -- propose -------------------------------------------------------------

    #[test]
    fn proposes_the_recipe_quantity_from_the_matching_line() {
        let stock = [line(1, "Farine", "g", 500.0, None)];
        let got = propose(&[ing("farine", Some(200.0), Some("g"))], &stock);
        assert_eq!(
            got,
            vec![Proposal {
                lots: vec![take(1, 200.0)],
                quantity: 200.0,
                warning: None
            }]
        );
    }

    #[test]
    fn caps_to_the_stock_with_a_warning() {
        let stock = [line(1, "Farine", "g", 150.0, None)];
        let got = propose(&[ing("Farine", Some(200.0), Some("g"))], &stock);
        assert_eq!(
            got,
            vec![Proposal {
                lots: vec![take(1, 150.0)],
                quantity: 150.0,
                warning: Some(RemovalWarning::Capped)
            }]
        );
    }

    #[test]
    fn without_a_line_proposes_nothing() {
        let got = propose(&[ing("Sel", Some(1.0), Some("pincée"))], &[]);
        assert_eq!(
            got,
            vec![Proposal {
                lots: vec![],
                quantity: 0.0,
                warning: Some(RemovalWarning::NoStock)
            }]
        );
    }

    #[test]
    fn another_unit_proposes_zero_with_a_warning() {
        let stock = [line(1, "Farine", "kg", 1.0, None)];
        let got = propose(&[ing("Farine", Some(200.0), Some("g"))], &stock);
        assert_eq!(
            got,
            vec![Proposal {
                lots: vec![take(1, 0.0)],
                quantity: 0.0,
                warning: Some(RemovalWarning::UnitMismatch)
            }]
        );
    }

    #[test]
    fn an_ingredient_without_unit_is_counted_in_the_default_unit() {
        let stock = [line(1, "Oeufs", "unité", 6.0, None)];
        let got = propose(&[ing("Oeufs", Some(3.0), None)], &stock);
        assert_eq!(got[0].quantity, 3.0);
        assert_eq!(got[0].warning, None);
        let blank = propose(&[ing("Oeufs", Some(3.0), Some("  "))], &stock);
        assert_eq!(blank[0].quantity, 3.0);
    }

    #[test]
    fn an_ingredient_without_quantity_proposes_zero() {
        let stock = [line(1, "Sel", "unité", 1.0, None)];
        let got = propose(&[ing("Sel", None, None)], &stock);
        assert_eq!(
            got,
            vec![Proposal {
                lots: vec![take(1, 0.0)],
                quantity: 0.0,
                warning: Some(RemovalWarning::NoQuantity)
            }]
        );
    }

    #[test]
    fn two_ingredients_on_one_line_share_what_it_holds() {
        let stock = [line(1, "Beurre", "g", 250.0, None)];
        let got = propose(
            &[
                ing("Beurre", Some(200.0), Some("g")),
                ing("beurre", Some(100.0), Some("g")),
            ],
            &stock,
        );
        assert_eq!(got[0].quantity, 200.0);
        assert_eq!(got[0].warning, None);
        assert_eq!(got[1].quantity, 50.0);
        assert_eq!(got[1].warning, Some(RemovalWarning::Capped));
    }

    #[test]
    fn proposes_one_line_per_ingredient_in_order() {
        let stock = [
            line(1, "Farine", "g", 500.0, None),
            line(2, "Lait", "L", 1.0, None),
        ];
        let got = propose(
            &[
                ing("Lait", Some(0.5), Some("L")),
                ing("Farine", Some(250.0), Some("g")),
            ],
            &stock,
        );
        assert_eq!(got[0].lots, vec![take(2, 0.5)]);
        assert_eq!(got[1].lots, vec![take(1, 250.0)]);
    }

    // -- propose, over several lots -------------------------------------------

    #[test]
    fn spreads_over_lots_when_the_first_holds_too_little() {
        // 100 g + 500 g of flour, 400 g wanted: not capped at the first lot.
        let stock = [
            line(1, "Farine", "g", 100.0, None),
            line(2, "Farine", "g", 500.0, None),
        ];
        let got = propose(&[ing("Farine", Some(400.0), Some("g"))], &stock);
        assert_eq!(
            got,
            vec![Proposal {
                lots: vec![take(1, 100.0), take(2, 300.0)],
                quantity: 400.0,
                warning: None
            }]
        );
    }

    #[test]
    fn the_lot_expiring_first_is_taken_first_and_undated_last() {
        let stock = [
            line(1, "Lait", "L", 1.0, None),
            line(2, "Lait", "L", 1.0, date(9)),
            line(3, "Lait", "L", 1.0, date(3)),
        ];
        let got = propose(&[ing("Lait", Some(2.5), Some("L"))], &stock);
        assert_eq!(got[0].lots, vec![take(3, 1.0), take(2, 1.0), take(1, 0.5)]);
        assert_eq!(got[0].quantity, 2.5);
        assert_eq!(got[0].warning, None);
    }

    #[test]
    fn a_lot_not_needed_is_not_proposed() {
        let stock = [
            line(1, "Farine", "g", 100.0, date(9)),
            line(2, "Farine", "g", 500.0, date(3)),
        ];
        let got = propose(&[ing("Farine", Some(200.0), Some("g"))], &stock);
        assert_eq!(got[0].lots, vec![take(2, 200.0)]);
    }

    #[test]
    fn capped_only_when_all_lots_together_hold_less() {
        let stock = [
            line(1, "Farine", "g", 100.0, None),
            line(2, "Farine", "g", 500.0, None),
        ];
        let got = propose(&[ing("Farine", Some(700.0), Some("g"))], &stock);
        assert_eq!(
            got,
            vec![Proposal {
                lots: vec![take(1, 100.0), take(2, 500.0)],
                quantity: 600.0,
                warning: Some(RemovalWarning::Capped)
            }]
        );
    }

    #[test]
    fn empty_lots_and_lots_in_another_unit_are_not_spread_over() {
        let stock = [
            line(1, "Farine", "g", 0.0, date(1)),
            line(2, "Farine", "kg", 1.0, date(2)),
            line(3, "Farine", "g", 150.0, date(9)),
        ];
        let got = propose(&[ing("Farine", Some(200.0), Some("g"))], &stock);
        assert_eq!(
            got,
            vec![Proposal {
                lots: vec![take(3, 150.0)],
                quantity: 150.0,
                warning: Some(RemovalWarning::Capped)
            }]
        );
    }

    #[test]
    fn only_empty_lots_propose_zero_from_the_first() {
        let stock = [
            line(1, "Oeufs", "unité", 0.0, date(9)),
            line(2, "Oeufs", "unité", 0.0, date(3)),
        ];
        let got = propose(&[ing("Oeufs", Some(3.0), None)], &stock);
        assert_eq!(
            got,
            vec![Proposal {
                lots: vec![take(2, 0.0)],
                quantity: 0.0,
                warning: Some(RemovalWarning::Capped)
            }]
        );
    }

    #[test]
    fn two_ingredients_share_the_lots_in_order() {
        let stock = [
            line(1, "Beurre", "g", 100.0, None),
            line(2, "Beurre", "g", 500.0, None),
        ];
        let got = propose(
            &[
                ing("Beurre", Some(300.0), Some("g")),
                ing("beurre", Some(400.0), Some("g")),
            ],
            &stock,
        );
        assert_eq!(got[0].lots, vec![take(1, 100.0), take(2, 200.0)]);
        assert_eq!(got[0].warning, None);
        assert_eq!(got[1].lots, vec![take(2, 300.0)]);
        assert_eq!(got[1].quantity, 300.0);
        assert_eq!(got[1].warning, Some(RemovalWarning::Capped));
    }

    #[test]
    fn spreading_over_lots_carries_no_binary_noise() {
        // 0.3 - 0.1 is 0.19999999999999998 and 0.1 + 0.2 is
        // 0.30000000000000004 in binary.
        let stock = [
            line(1, "Lait", "L", 0.1, date(1)),
            line(2, "Lait", "L", 0.2, date(2)),
        ];
        let got = propose(&[ing("Lait", Some(0.3), Some("L"))], &stock);
        assert_eq!(
            got,
            vec![Proposal {
                lots: vec![take(1, 0.1), take(2, 0.2)],
                quantity: 0.3,
                warning: None
            }]
        );
    }

    // -- removals_to_apply ---------------------------------------------------

    fn removal(n: u128, quantity: f64, exclude: bool) -> Removal {
        Removal {
            stock_item_id: id(n),
            quantity,
            exclude,
        }
    }

    #[test]
    fn zero_and_excluded_lines_remove_nothing() {
        let got = removals_to_apply(&[
            removal(1, 0.0, false),
            removal(2, 3.0, true),
            removal(3, 2.5, false),
        ])
        .unwrap();
        assert_eq!(got, vec![(id(3), 2.5)]);
    }

    #[test]
    fn an_empty_request_removes_nothing() {
        assert_eq!(removals_to_apply(&[]).unwrap(), vec![]);
    }

    #[test]
    fn a_negative_or_non_finite_quantity_is_refused() {
        for q in [-0.5, f64::INFINITY, f64::NAN] {
            assert_eq!(
                bad_request(removals_to_apply(&[removal(1, q, false)])),
                "invalid_quantity",
                "{q}"
            );
        }
    }

    #[test]
    fn a_line_named_twice_is_refused() {
        assert_eq!(
            bad_request(removals_to_apply(&[
                removal(1, 1.0, false),
                removal(1, 0.0, true)
            ])),
            "duplicate_stock_item"
        );
    }

    // -- check_grocery_ids ---------------------------------------------------

    #[test]
    fn grocery_ids_named_twice_are_refused() {
        assert_eq!(
            bad_request(check_grocery_ids(&[id(1), id(2), id(1)])),
            "duplicate_grocery_item"
        );
    }

    #[test]
    fn distinct_or_no_grocery_ids_are_accepted() {
        assert!(check_grocery_ids(&[]).is_ok());
        assert!(check_grocery_ids(&[id(1), id(2)]).is_ok());
    }

    // -- names_an_ingredient -------------------------------------------------

    #[test]
    fn a_stock_line_must_name_an_ingredient() {
        let ingredients = ["Farine", "Lait"];
        assert!(names_an_ingredient(" lait ", &ingredients));
        assert!(!names_an_ingredient("Lait de coco", &ingredients));
        assert!(!names_an_ingredient("Lait", &[]));
    }

    // -- check_others ---------------------------------------------------------

    fn other(n: u128, quantity: f64) -> OtherRemoval {
        OtherRemoval {
            stock_item_id: id(n),
            quantity,
        }
    }

    #[test]
    fn other_products_are_taken_as_given() {
        let got = check_others(&[removal(1, 2.0, false)], &[other(2, 1.5), other(3, 0.25)]);
        assert_eq!(got.unwrap(), vec![(id(2), 1.5), (id(3), 0.25)]);
        assert_eq!(check_others(&[], &[]).unwrap(), vec![]);
    }

    #[test]
    fn another_product_needs_a_quantity_above_zero() {
        for q in [0.0, -1.0, f64::INFINITY, f64::NAN] {
            assert_eq!(
                bad_request(check_others(&[], &[other(2, q)])),
                "invalid_quantity",
                "{q}"
            );
        }
    }

    #[test]
    fn a_line_named_twice_across_both_lists_is_refused() {
        assert_eq!(
            bad_request(check_others(&[], &[other(2, 1.0), other(2, 2.0)])),
            "duplicate_stock_item"
        );
        // Even when the recipe's line is excluded: one line, one quantity.
        assert_eq!(
            bad_request(check_others(&[removal(2, 0.0, true)], &[other(2, 1.0)])),
            "duplicate_stock_item"
        );
    }

    // -- ingredients_to_add ---------------------------------------------------

    fn used<'a>(name: &'a str, unit: &'a str, quantity: f64) -> Used<'a> {
        Used {
            name,
            unit,
            quantity,
        }
    }

    fn new_ing(name: &str, quantity: f64, unit: &str) -> NewIngredient {
        NewIngredient {
            name: name.into(),
            quantity,
            unit: unit.into(),
        }
    }

    #[test]
    fn each_other_product_becomes_an_ingredient_with_its_stock_unit() {
        let got = ingredients_to_add(
            &[used(" Crème ", "cL", 20.0), used("Sel", "pincée", 1.0)],
            &["Farine"],
            None,
            None,
        );
        assert_eq!(
            got,
            vec![new_ing("Crème", 20.0, "cL"), new_ing("Sel", 1.0, "pincée")]
        );
    }

    #[test]
    fn a_product_the_recipe_already_names_is_not_added_again() {
        let got = ingredients_to_add(
            &[used("crème", "cL", 20.0), used("Sel", "pincée", 1.0)],
            &["Farine", " Crème"],
            None,
            None,
        );
        assert_eq!(got, vec![new_ing("Sel", 1.0, "pincée")]);
    }

    #[test]
    fn two_lots_of_one_product_are_one_ingredient() {
        // Same unit: the quantities add up. Another unit cannot be added to
        // them (no conversion): the first line's unit is kept.
        let got = ingredients_to_add(
            &[
                used("Lait", "L", 0.1),
                used("lait", "l", 0.2),
                used("Lait", "cL", 50.0),
            ],
            &[],
            None,
            None,
        );
        assert_eq!(got, vec![new_ing("Lait", 0.3, "L")]);
    }

    #[test]
    fn the_quantity_is_brought_back_to_the_recipes_number_of_people() {
        // Cooked for 4, the recipe written for 2: half of what was used.
        let got = ingredients_to_add(&[used("Crème", "cL", 30.0)], &[], Some(4), Some(2));
        assert_eq!(got, vec![new_ing("Crème", 15.0, "cL")]);
        // Either number missing: the quantity used.
        let got = ingredients_to_add(&[used("Crème", "cL", 30.0)], &[], None, Some(2));
        assert_eq!(got, vec![new_ing("Crème", 30.0, "cL")]);
    }

    // -- recipe updates (#558) -------------------------------------------------

    fn update(n: u128, change: RecipeChange) -> RecipeUpdate {
        RecipeUpdate {
            ingredient_id: id(n),
            change,
        }
    }

    #[test]
    fn a_recipe_update_reads_its_action_beside_the_ingredient() {
        let got: Vec<RecipeUpdate> = serde_json::from_value(json!([
            {"ingredient_id": id(1), "action": "set_quantity", "quantity": 250.0},
            {"ingredient_id": id(2), "action": "remove"},
            {"ingredient_id": id(3), "action": "make_optional"}
        ]))
        .unwrap();
        assert_eq!(
            got,
            vec![
                update(1, RecipeChange::SetQuantity { quantity: 250.0 }),
                update(2, RecipeChange::Remove),
                update(3, RecipeChange::MakeOptional),
            ]
        );
    }

    #[test]
    fn a_recipe_update_without_a_known_action_does_not_read() {
        for bad in [
            json!({"ingredient_id": id(1)}),
            json!({"ingredient_id": id(1), "action": "rename"}),
            json!({"ingredient_id": id(1), "action": "set_quantity"}),
        ] {
            assert!(
                serde_json::from_value::<RecipeUpdate>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn distinct_recipe_updates_are_accepted() {
        assert!(check_recipe_updates(&[]).is_ok());
        assert!(check_recipe_updates(&[
            update(1, RecipeChange::SetQuantity { quantity: 0.5 }),
            update(2, RecipeChange::Remove),
            update(3, RecipeChange::MakeOptional),
        ])
        .is_ok());
    }

    #[test]
    fn an_ingredient_updated_twice_is_refused() {
        assert_eq!(
            bad_request(check_recipe_updates(&[
                update(1, RecipeChange::Remove),
                update(1, RecipeChange::MakeOptional),
            ])),
            "duplicate_ingredient"
        );
    }

    #[test]
    fn a_new_quantity_must_be_finite_and_above_zero() {
        for q in [0.0, -1.0, f64::INFINITY, f64::NAN] {
            assert_eq!(
                bad_request(check_recipe_updates(&[update(
                    1,
                    RecipeChange::SetQuantity { quantity: q }
                )])),
                "invalid_quantity",
                "{q}"
            );
        }
    }
}
