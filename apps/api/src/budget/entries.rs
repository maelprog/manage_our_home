use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Datelike, Months, NaiveDate, Utc};
use manage_our_home_shared::dto::budget::{
    BudgetCategoryUsage, BudgetMonthSummary, BudgetPayerTotal,
};
use manage_our_home_shared::validation::budget::{cap_usage, parse_month};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::budget::alerts;
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
    /// One grocery item to link the expense to: a shorthand for
    /// `grocery_item_ids` naming it alone (#633), kept from before the
    /// expense could link to several. Both may be given.
    pub grocery_item_id: Option<Uuid>,
    /// The grocery items of the family the expense links to (#633): each
    /// once ([`grocery_item_ids`]), none already linked to another expense.
    #[serde(default)]
    pub grocery_item_ids: Vec<Uuid>,
    /// A category of the family (#532); none files it under « Sans
    /// catégorie ».
    pub category_id: Option<Uuid>,
    /// The member of the family who paid (#638); none, nobody.
    pub paid_by: Option<Uuid>,
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
    /// The grocery items the expense links to from now on, replacing those
    /// it linked to (#633): `[]` unlinks them all; left out, or `null`,
    /// the links stay.
    pub grocery_item_ids: Option<Vec<Uuid>>,
    /// `Some(None)` (`null`) leaves the entry without a payer (#638);
    /// `None` (left out) keeps its payer.
    #[serde(default, deserialize_with = "deserialize_some")]
    pub paid_by: Option<Option<Uuid>>,
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

/// A grocery item an expense links to, under its current name.
#[derive(Serialize)]
pub struct LinkedGroceryItem {
    pub id: Uuid,
    pub name: String,
}

#[derive(Serialize)]
pub struct BudgetEntryResponse {
    pub id: Uuid,
    pub group_id: Uuid,
    pub created_by: Uuid,
    /// The grocery items the expense links to (#633), by name.
    pub grocery_items: Vec<LinkedGroceryItem>,
    pub category_id: Option<Uuid>,
    /// The member who paid (#638): `null` for nobody, and once the payer's
    /// membership ends (`budget_entries_paid_by_fkey`, 0039).
    pub paid_by: Option<Uuid>,
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
    category_id: Option<Uuid>,
    paid_by: Option<Uuid>,
    name: String,
    amount_cents: i64,
    spent_at: NaiveDate,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl BudgetEntryResponse {
    fn new(r: BudgetEntryRow, grocery_items: Vec<LinkedGroceryItem>) -> Self {
        BudgetEntryResponse {
            id: r.id,
            group_id: r.group_id,
            created_by: r.created_by,
            grocery_items,
            category_id: r.category_id,
            paid_by: r.paid_by,
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

/// The most grocery items one expense links to (user's decision of
/// 2026-10-30), counted once duplicates are merged.
pub(crate) const GROCERY_ITEMS_MAX: usize = 100;

/// The grocery items an expense is to be linked to, as the request names
/// them: each once, in the order first named (the same item named twice,
/// e.g. two brands of one article, is one link), and at most
/// [`GROCERY_ITEMS_MAX`] of them.
pub(crate) fn grocery_item_ids(ids: impl IntoIterator<Item = Uuid>) -> AppResult<Vec<Uuid>> {
    let mut seen = HashSet::new();
    let ids: Vec<Uuid> = ids.into_iter().filter(|id| seen.insert(*id)).collect();
    if ids.len() > GROCERY_ITEMS_MAX {
        return Err(AppError::BadRequest("too_many_grocery_items".into()));
    }
    Ok(ids)
}

type Tx<'c> = sqlx::Transaction<'c, sqlx::Postgres>;

/// The grocery items each of `entry_ids` links to, by name.
async fn linked_items(
    tx: &mut Tx<'_>,
    group_id: Uuid,
    entry_ids: &[Uuid],
) -> AppResult<HashMap<Uuid, Vec<LinkedGroceryItem>>> {
    let rows = sqlx::query!(
        r#"SELECT l.entry_id, gi.id, gi.name
           FROM budget_entry_grocery_items l
           JOIN grocery_items gi ON gi.id = l.grocery_item_id AND gi.group_id = l.group_id
           WHERE l.group_id = $1 AND l.entry_id = ANY($2)
           ORDER BY lower(gi.name), gi.id"#,
        group_id,
        entry_ids,
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut items: HashMap<Uuid, Vec<LinkedGroceryItem>> = HashMap::new();
    for r in rows {
        items
            .entry(r.entry_id)
            .or_default()
            .push(LinkedGroceryItem {
                id: r.id,
                name: r.name,
            });
    }
    Ok(items)
}

/// One expense as the routes answer it, its links read in `tx`.
async fn entry_response(
    tx: &mut Tx<'_>,
    group_id: Uuid,
    entry: BudgetEntryRow,
) -> AppResult<BudgetEntryResponse> {
    let items = linked_items(tx, group_id, &[entry.id])
        .await?
        .remove(&entry.id)
        .unwrap_or_default();
    Ok(BudgetEntryResponse::new(entry, items))
}

/// Links the expense `entry_id` to exactly `item_ids` ([`grocery_item_ids`]),
/// dropping the links it had to any other item. An item that is not one of
/// the family's is the 404 of any other referenced resource; an item linked
/// to another expense is a 409, as before #633: an item is paid once.
///
/// To call under the family's lock (`alerts::lock_family`), which every
/// write of a link takes: two expenses linking one item at once see each
/// other's link instead of tripping on its unique key. The items are
/// locked against their deletion until the transaction ends.
async fn set_links(
    tx: &mut Tx<'_>,
    group_id: Uuid,
    entry_id: Uuid,
    item_ids: &[Uuid],
) -> AppResult<()> {
    let found = sqlx::query_scalar!(
        "SELECT id FROM grocery_items WHERE group_id = $1 AND id = ANY($2) FOR KEY SHARE",
        group_id,
        item_ids,
    )
    .fetch_all(&mut **tx)
    .await?;
    if found.len() != item_ids.len() {
        return Err(AppError::NotFound);
    }
    let taken = sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM budget_entry_grocery_items
               WHERE group_id = $1 AND grocery_item_id = ANY($2) AND entry_id <> $3
           ) AS "taken!""#,
        group_id,
        item_ids,
        entry_id,
    )
    .fetch_one(&mut **tx)
    .await?;
    if taken {
        return Err(AppError::Conflict("grocery_item_already_priced".into()));
    }
    sqlx::query!(
        "DELETE FROM budget_entry_grocery_items
         WHERE group_id = $1 AND entry_id = $2 AND NOT (grocery_item_id = ANY($3))",
        group_id,
        entry_id,
        item_ids,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "INSERT INTO budget_entry_grocery_items (entry_id, grocery_item_id, group_id)
         SELECT $1, item_id, $2 FROM unnest($3::uuid[]) AS item_id
         ON CONFLICT (entry_id, grocery_item_id) DO NOTHING",
        entry_id,
        group_id,
        item_ids,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
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
    if let Some(payer) = body.paid_by {
        require_payer(&mut tx, group_id, payer).await?;
    }

    let item_ids = grocery_item_ids(
        body.grocery_item_id
            .into_iter()
            .chain(body.grocery_item_ids),
    )?;
    if !item_ids.is_empty() {
        // The family's lock before any link is read (`set_links`).
        alerts::lock_family(&mut tx, group_id).await?;
    }

    let spent_at = day_or_today(body.spent_at, Utc::now());
    let watch = alerts::watch(&mut tx, group_id, spent_at, body.category_id).await?;
    let entry = sqlx::query_as!(
        BudgetEntryRow,
        r#"
        INSERT INTO budget_entries (group_id, created_by, name, amount_cents, spent_at, category_id, paid_by)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, group_id, created_by, category_id, paid_by, name, amount_cents, spent_at, created_at, updated_at
        "#,
        group_id,
        auth.user_id,
        name,
        amount_cents,
        spent_at,
        body.category_id,
        body.paid_by,
    )
    .fetch_one(&mut *tx)
    .await?;
    if !item_ids.is_empty() {
        set_links(&mut tx, group_id, entry.id, &item_ids).await?;
    }
    let alerted = match watch {
        Some(watch) => watch.fire(&mut tx).await?,
        None => Vec::new(),
    };
    let entry = entry_response(&mut tx, group_id, entry).await?;
    tx.commit().await?;
    alerts::push_in_background(&state, alerted);

    Ok((StatusCode::CREATED, Json(entry)))
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

/// A member of this family, as the payer of an expense must be (#638), or
/// the 400 `paid_by_not_a_member` — a member of another family and an
/// unknown id alike. The membership is locked against its deletion until
/// the transaction ends, so the payer cannot leave between this check and
/// the write that names them (the composite key of `budget_entries` would
/// refuse it then, as a 500).
async fn require_payer(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    user_id: Uuid,
) -> AppResult<()> {
    sqlx::query_scalar!(
        "SELECT user_id FROM group_members WHERE group_id = $1 AND user_id = $2 FOR KEY SHARE",
        group_id,
        user_id,
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| AppError::BadRequest("paid_by_not_a_member".into()))?;
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

    // The family's lock before anything is read: the expense the item is
    // linked to, and its category, are then those no other write of the
    // family can change before this one commits.
    alerts::lock_family(&mut tx, group_id).await?;
    let item = sqlx::query!(
        "SELECT name FROM grocery_items WHERE id = $1 AND group_id = $2 FOR KEY SHARE",
        item_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    // "Set" is idempotent: a price set again on an item updates the expense
    // it is linked to instead of creating a second, double-counted one.
    let existing = sqlx::query!(
        r#"SELECT e.id, e.category_id,
                  (SELECT count(*) FROM budget_entry_grocery_items o
                   WHERE o.entry_id = e.id AND o.group_id = e.group_id) AS "links!"
           FROM budget_entry_grocery_items l
           JOIN budget_entries e ON e.id = l.entry_id AND e.group_id = l.group_id
           WHERE l.grocery_item_id = $1 AND l.group_id = $2
           FOR UPDATE OF e"#,
        item_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?;
    // An expense linked to other items too (#633) is their price as much
    // as this one's: this item's own price would replace it.
    if existing.as_ref().is_some_and(|e| e.links > 1) {
        return Err(AppError::Conflict("grocery_item_priced_with_others".into()));
    }
    // A price set again keeps its entry's category (the update below leaves
    // `category_id` alone): that is the category whose cap it may cross.
    let category_id = existing.as_ref().and_then(|e| e.category_id);
    let spent_at = day_or_today(body.spent_at, Utc::now());
    let watch = alerts::watch(&mut tx, group_id, spent_at, category_id).await?;

    let entry = match existing {
        Some(existing) => {
            sqlx::query_as!(
                BudgetEntryRow,
                r#"
                UPDATE budget_entries SET
                    name = $3,
                    amount_cents = $4,
                    spent_at = $5,
                    updated_at = now()
                WHERE id = $1 AND group_id = $2
                RETURNING id, group_id, created_by, category_id, paid_by, name, amount_cents, spent_at, created_at, updated_at
                "#,
                existing.id,
                group_id,
                item.name,
                amount_cents,
                spent_at,
            )
            .fetch_one(&mut *tx)
            .await?
        }
        None => {
            let entry = sqlx::query_as!(
                BudgetEntryRow,
                r#"
                INSERT INTO budget_entries (group_id, created_by, name, amount_cents, spent_at)
                VALUES ($1, $2, $3, $4, $5)
                RETURNING id, group_id, created_by, category_id, paid_by, name, amount_cents, spent_at, created_at, updated_at
                "#,
                group_id,
                auth.user_id,
                item.name,
                amount_cents,
                spent_at,
            )
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query!(
                "INSERT INTO budget_entry_grocery_items (entry_id, grocery_item_id, group_id)
                 VALUES ($1, $2, $3)",
                entry.id,
                item_id,
                group_id,
            )
            .execute(&mut *tx)
            .await?;
            entry
        }
    };
    let alerted = match watch {
        Some(watch) => watch.fire(&mut tx).await?,
        None => Vec::new(),
    };
    let entry = entry_response(&mut tx, group_id, entry).await?;
    tx.commit().await?;
    alerts::push_in_background(&state, alerted);

    Ok((StatusCode::CREATED, Json(entry)))
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
        r#"SELECT id, group_id, created_by, category_id, paid_by, name, amount_cents, spent_at, created_at, updated_at
           FROM budget_entries WHERE id = $1 AND group_id = $2"#,
        entry_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    let entry = entry_response(&mut tx, group_id, entry).await?;
    tx.commit().await?;

    Ok(Json(entry))
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
        SELECT id, group_id, created_by, category_id, paid_by, name, amount_cents, spent_at, created_at, updated_at
        FROM budget_entries
        WHERE group_id = $1
        ORDER BY spent_at DESC, created_at DESC
        "#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let mut items = linked_items(&mut tx, group_id, &ids).await?;
    tx.commit().await?;

    let entries: Vec<BudgetEntryResponse> = rows
        .into_iter()
        .map(|r| {
            let linked = items.remove(&r.id).unwrap_or_default();
            BudgetEntryResponse::new(r, linked)
        })
        .collect();

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

    // The family's lock before the row's, as every write of an expense
    // (`alerts::lock_family`): the other order deadlocks with a price.
    alerts::lock_family(&mut tx, group_id).await?;
    // FOR UPDATE locks the row for the rest of this transaction, mirroring
    // grocery_list/items.rs's and stocks/items.rs's concurrent-write protection.
    let existing = sqlx::query!(
        "SELECT created_by, name, amount_cents, spent_at, category_id, paid_by FROM budget_entries WHERE id = $1 AND group_id = $2 FOR UPDATE",
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
    let paid_by = match body.paid_by {
        Some(Some(payer)) => {
            require_payer(&mut tx, group_id, payer).await?;
            Some(payer)
        }
        Some(None) => None,
        None => existing.paid_by,
    };
    let item_ids = body.grocery_item_ids.map(grocery_item_ids).transpose()?;
    let watch = alerts::watch(&mut tx, group_id, spent_at, category_id).await?;

    let entry = sqlx::query_as!(
        BudgetEntryRow,
        r#"
        UPDATE budget_entries SET
            name = $3,
            amount_cents = $4,
            spent_at = $5,
            category_id = $6,
            paid_by = $7,
            updated_at = now()
        WHERE id = $1 AND group_id = $2
        RETURNING id, group_id, created_by, category_id, paid_by, name, amount_cents, spent_at, created_at, updated_at
        "#,
        entry_id,
        group_id,
        name,
        amount_cents,
        spent_at,
        category_id,
        paid_by,
    )
    .fetch_one(&mut *tx)
    .await?;
    if let Some(item_ids) = item_ids {
        set_links(&mut tx, group_id, entry_id, &item_ids).await?;
    }
    let alerted = match watch {
        Some(watch) => watch.fire(&mut tx).await?,
        None => Vec::new(),
    };
    let entry = entry_response(&mut tx, group_id, entry).await?;
    tx.commit().await?;
    alerts::push_in_background(&state, alerted);

    Ok(Json(entry))
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
/// (`validation::budget::cap_usage`). Since #638, per member who paid some
/// of it, their total, and what nobody is named as having paid.
pub async fn budget_summary(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    query: Result<Query<SummaryQuery>, QueryRejection>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_budget_access(&mut tx, group_id, auth.user_id).await?;
    // The bar first, the query after, as the bodies of the other routes: a
    // query the extractor cannot read (`month` given twice) is a standard
    // member's 404 too, and past the bar the 400 of an invalid month.
    let Query(query) = query.map_err(|_| AppError::BadRequest("invalid_month".into()))?;

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
    // A payer is a current member (0039): the name they go by now. The
    // largest total first, then by name, then by id for a stable order.
    let payers = sqlx::query!(
        r#"
        SELECT e.paid_by AS "user_id!", u.display_name,
               SUM(e.amount_cents)::bigint AS "total_cents!"
        FROM budget_entries e
        JOIN users u ON u.id = e.paid_by
        WHERE e.group_id = $1 AND e.paid_by IS NOT NULL
          AND e.spent_at >= $2 AND e.spent_at < $3
        GROUP BY e.paid_by, u.display_name
        ORDER BY 3 DESC, lower(u.display_name), e.paid_by
        "#,
        group_id,
        month,
        next_month,
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

    let total_cents = categories.iter().fold(uncategorized_cents, |sum, c| {
        sum.saturating_add(c.total_cents)
    });
    let paid_cents = payers
        .iter()
        .fold(0i64, |sum, p| sum.saturating_add(p.total_cents));
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
        payers: payers
            .into_iter()
            .map(|p| BudgetPayerTotal {
                user_id: p.user_id,
                display_name: p.display_name,
                total: cents_to_euros(p.total_cents),
            })
            .collect(),
        without_payer: cents_to_euros(total_cents.saturating_sub(paid_cents)),
    };

    Ok(Json(json!({ "periods": periods, "month": month })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn too_many(result: AppResult<Vec<Uuid>>) -> bool {
        matches!(result, Err(AppError::BadRequest(code)) if code == "too_many_grocery_items")
    }

    #[test]
    fn no_item_is_no_link() {
        assert_eq!(grocery_item_ids([]).unwrap(), Vec::<Uuid>::new());
    }

    #[test]
    fn items_keep_the_order_they_are_named_in() {
        assert_eq!(
            grocery_item_ids([id(3), id(1), id(2)]).unwrap(),
            [id(3), id(1), id(2)]
        );
    }

    #[test]
    fn an_item_named_twice_is_one_link_at_its_first_place() {
        assert_eq!(
            grocery_item_ids([id(2), id(1), id(2), id(1), id(3)]).unwrap(),
            [id(2), id(1), id(3)]
        );
    }

    #[test]
    fn a_hundred_items_pass_and_one_more_does_not() {
        let hundred: Vec<Uuid> = (1..=100).map(id).collect();
        assert_eq!(grocery_item_ids(hundred.clone()).unwrap(), hundred);
        assert!(too_many(grocery_item_ids((1..=101).map(id))));
    }

    #[test]
    fn duplicates_do_not_count_against_the_bound() {
        let named = (1..=100).map(id).chain((1..=100).map(id));
        assert_eq!(grocery_item_ids(named).unwrap().len(), 100);
    }
}
