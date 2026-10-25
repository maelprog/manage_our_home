//! Receiving the groceries into the stock (#471). The checked items of the
//! shopping list are received as one lot: each one is added to an existing
//! stock line (`merge`) or makes a line of its own (`new`), and leaves the
//! list — all in one transaction, so a lot with a single bad item applies
//! nothing.
//!
//! The default mode is per article (`merge_target` in the shared crate): an
//! article merges into a line of the same name and unit unless a date
//! different from that line's is typed. `GET` returns that proposal for the
//! checked items so the web can pre-fill its form; `POST` applies a lot,
//! falling back to the same rule for an item sent without a mode.
//!
//! Permissions are those of stock creation: any group member. Receiving an
//! item removes it from the list whoever added it, and a merge is a
//! quantity-only adjustment, which any member may already make.

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use chrono::NaiveDate;
use manage_our_home_shared::validation::stocks::{
    merge_target, normalize_unit, ReceptionMode, StockCandidate,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::stocks::items::{today, StockItemResponse, StockItemRow};
use crate::AppState;

/// `POST /groups/:id/stock-items/reception` body.
#[derive(Deserialize)]
pub struct ReceiveRequest {
    pub items: Vec<ReceivedItem>,
}

/// One received article. Every field but the grocery item's id is optional:
/// an absent one takes the grocery item's value (see `received_line`).
#[derive(Deserialize)]
pub struct ReceivedItem {
    pub grocery_item_id: Uuid,
    pub name: Option<String>,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    pub expires_on: Option<NaiveDate>,
    /// Absent → the default proposal (`merge_target`).
    pub mode: Option<ReceptionMode>,
}

/// One line of the reception: the article as it enters the stock, its mode,
/// and the stock line concerned — on `GET` the line a merge would add to
/// (`null` for `new`), on `POST` the line as the reception left it.
#[derive(Serialize)]
pub struct ReceptionLineResponse {
    pub grocery_item_id: Uuid,
    pub name: String,
    pub quantity: f64,
    pub unit: String,
    pub expires_on: Option<NaiveDate>,
    pub mode: ReceptionMode,
    pub stock_item: Option<StockItemResponse>,
}

/// The grocery-item fields a received article falls back on.
struct GroceryLine {
    id: Uuid,
    name: String,
    quantity: Option<f64>,
    unit: Option<String>,
}

/// An article as it enters the stock, once its defaults are applied.
#[derive(Debug, PartialEq)]
struct Line {
    name: String,
    quantity: f64,
    unit: String,
    expires_on: Option<NaiveDate>,
}

/// The lot must name at least one item, and each item once.
fn check_lot(ids: &[Uuid]) -> AppResult<()> {
    if ids.is_empty() {
        return Err(AppError::BadRequest("items_required".into()));
    }
    let mut seen = HashSet::with_capacity(ids.len());
    if !ids.iter().all(|id| seen.insert(id)) {
        return Err(AppError::BadRequest("duplicate_grocery_item".into()));
    }
    Ok(())
}

/// Applies the grocery item's values to the fields the article leaves out:
/// its name, its quantity (1 when the list had none), its unit (normalized
/// like a stock creation, #458). The name must not be blank and the
/// quantity must be positive.
fn received_line(item: &ReceivedItem, grocery: &GroceryLine) -> AppResult<Line> {
    let name = item.name.as_deref().unwrap_or(&grocery.name).trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("name_required".into()));
    }
    let quantity = item.quantity.or(grocery.quantity).unwrap_or(1.0);
    if quantity <= 0.0 {
        return Err(AppError::BadRequest("quantity_must_be_positive".into()));
    }
    let unit = item
        .unit
        .as_deref()
        .or(grocery.unit.as_deref())
        .unwrap_or("");
    Ok(Line {
        name: name.to_string(),
        quantity,
        unit: normalize_unit(unit),
        expires_on: item.expires_on,
    })
}

/// The stock line the article goes to: `Some(id)` to merge, `None` for a
/// new line. Without a mode, the default proposal; `merge` with nothing to
/// merge into is a conflict.
fn resolve_target(mode: Option<ReceptionMode>, proposal: Option<Uuid>) -> AppResult<Option<Uuid>> {
    match mode {
        None => Ok(proposal),
        Some(ReceptionMode::New) => Ok(None),
        Some(ReceptionMode::Merge) => proposal
            .map(Some)
            .ok_or_else(|| AppError::Conflict("no_stock_to_merge".into())),
    }
}

fn candidates(stock: &[StockItemRow]) -> Vec<StockCandidate<'_>> {
    stock
        .iter()
        .map(|s| StockCandidate {
            id: s.id,
            name: &s.name,
            unit: &s.unit,
            expires_on: s.expires_on,
        })
        .collect()
}

fn mode_of(target: Option<Uuid>) -> ReceptionMode {
    if target.is_some() {
        ReceptionMode::Merge
    } else {
        ReceptionMode::New
    }
}

/// The default proposal for each checked item of the list, pre-filled with
/// the item's values and no date.
pub async fn get_reception(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let checked = sqlx::query_as!(
        GroceryLine,
        r#"SELECT id, name, quantity, unit FROM grocery_items
           WHERE group_id = $1 AND checked = true ORDER BY name, id"#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    let stock = sqlx::query_as!(
        StockItemRow,
        r#"SELECT id, group_id, created_by, name, category, quantity, unit, reorder_threshold, expires_on, barcode
           FROM stock_items WHERE group_id = $1"#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let today = today();
    let mut lines = Vec::with_capacity(checked.len());
    for grocery in &checked {
        let item = ReceivedItem {
            grocery_item_id: grocery.id,
            name: None,
            quantity: None,
            unit: None,
            expires_on: None,
            mode: None,
        };
        // A checked item the list holds with a blank name or a zero
        // quantity cannot be pre-filled; the POST will refuse it the same way.
        let Ok(line) = received_line(&item, grocery) else {
            continue;
        };
        let target = merge_target(&line.name, &line.unit, None, &candidates(&stock));
        let stock_item = target
            .and_then(|id| stock.iter().find(|s| s.id == id))
            .map(|s| StockItemResponse::from_row(s.clone(), today));
        lines.push(ReceptionLineResponse {
            grocery_item_id: grocery.id,
            name: line.name,
            quantity: line.quantity,
            unit: line.unit,
            expires_on: None,
            mode: mode_of(target),
            stock_item,
        });
    }

    Ok(Json(json!({ "items": lines })))
}

/// Receives a lot of grocery items into the stock, in one transaction: an
/// item of another group (or already gone) is a 404 and an invalid one a
/// 400/409, and either leaves the list and the stock untouched.
pub async fn receive(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<ReceiveRequest>,
) -> AppResult<impl IntoResponse> {
    let ids: Vec<Uuid> = body.items.iter().map(|i| i.grocery_item_id).collect();
    check_lot(&ids)?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    // FOR UPDATE: a concurrent reception of the same items waits for this
    // one, then finds them gone (404) instead of receiving them twice.
    let groceries = sqlx::query_as!(
        GroceryLine,
        r#"SELECT id, name, quantity, unit FROM grocery_items
           WHERE group_id = $1 AND id = ANY($2) FOR UPDATE"#,
        group_id,
        &ids,
    )
    .fetch_all(&mut *tx)
    .await?;
    if groceries.len() != ids.len() {
        return Err(AppError::NotFound);
    }

    let mut stock = sqlx::query_as!(
        StockItemRow,
        r#"SELECT id, group_id, created_by, name, category, quantity, unit, reorder_threshold, expires_on, barcode
           FROM stock_items WHERE group_id = $1"#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;

    let mut received = Vec::with_capacity(body.items.len());
    for item in &body.items {
        let grocery = groceries
            .iter()
            .find(|g| g.id == item.grocery_item_id)
            .ok_or(AppError::NotFound)?;
        let line = received_line(item, grocery)?;
        let proposal = merge_target(&line.name, &line.unit, line.expires_on, &candidates(&stock));
        let target = resolve_target(item.mode, proposal)?;

        let row = match target {
            // The increment is done by the database, so a concurrent
            // quantity adjustment is not lost.
            Some(stock_id) => sqlx::query_as!(
                StockItemRow,
                r#"
                UPDATE stock_items SET quantity = quantity + $3, updated_at = now()
                WHERE id = $1 AND group_id = $2
                RETURNING id, group_id, created_by, name, category, quantity, unit, reorder_threshold, expires_on, barcode
                "#,
                stock_id,
                group_id,
                line.quantity,
            )
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(AppError::NotFound)?,
            None => sqlx::query_as!(
                StockItemRow,
                r#"
                INSERT INTO stock_items (group_id, created_by, name, quantity, unit, expires_on)
                VALUES ($1, $2, $3, $4, $5, $6)
                RETURNING id, group_id, created_by, name, category, quantity, unit, reorder_threshold, expires_on, barcode
                "#,
                group_id,
                auth.user_id,
                line.name,
                line.quantity,
                line.unit,
                line.expires_on,
            )
            .fetch_one(&mut *tx)
            .await?,
        };

        // Later items of the same lot see this line as it now stands: two
        // received « lait » merge into the line the first one created.
        match stock.iter_mut().find(|s| s.id == row.id) {
            Some(s) => *s = row.clone(),
            None => stock.push(row.clone()),
        }
        received.push((item.grocery_item_id, line, mode_of(target), row));
    }

    sqlx::query!(
        "DELETE FROM grocery_items WHERE group_id = $1 AND id = ANY($2)",
        group_id,
        &ids,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let today = today();
    let lines: Vec<ReceptionLineResponse> = received
        .into_iter()
        .map(|(grocery_item_id, line, mode, row)| ReceptionLineResponse {
            grocery_item_id,
            name: line.name,
            quantity: line.quantity,
            unit: line.unit,
            expires_on: line.expires_on,
            mode,
            stock_item: Some(StockItemResponse::from_row(row, today)),
        })
        .collect();

    Ok(Json(json!({ "items": lines })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grocery(quantity: Option<f64>, unit: Option<&str>) -> GroceryLine {
        GroceryLine {
            id: Uuid::nil(),
            name: "Lait".into(),
            quantity,
            unit: unit.map(str::to_string),
        }
    }

    fn bare() -> ReceivedItem {
        ReceivedItem {
            grocery_item_id: Uuid::nil(),
            name: None,
            quantity: None,
            unit: None,
            expires_on: None,
            mode: None,
        }
    }

    fn bad_request(r: AppResult<impl std::fmt::Debug>) -> String {
        match r {
            Err(AppError::BadRequest(code)) => code,
            other => panic!("expected a 400, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_lot_is_refused() {
        assert_eq!(bad_request(check_lot(&[])), "items_required");
    }

    #[test]
    fn an_item_named_twice_in_the_lot_is_refused() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        assert_eq!(bad_request(check_lot(&[a, b, a])), "duplicate_grocery_item");
    }

    #[test]
    fn distinct_items_make_a_valid_lot() {
        assert!(check_lot(&[Uuid::from_u128(1), Uuid::from_u128(2)]).is_ok());
    }

    #[test]
    fn a_bare_item_takes_the_grocery_items_values() {
        let line = received_line(&bare(), &grocery(Some(2.0), Some("L"))).unwrap();
        assert_eq!(
            line,
            Line {
                name: "Lait".into(),
                quantity: 2.0,
                unit: "L".into(),
                expires_on: None,
            }
        );
    }

    #[test]
    fn the_received_values_override_the_grocery_items() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
        let item = ReceivedItem {
            name: Some("  Lait demi-écrémé ".into()),
            quantity: Some(6.0),
            unit: Some(" bouteille ".into()),
            expires_on: Some(date),
            ..bare()
        };
        let line = received_line(&item, &grocery(Some(2.0), Some("L"))).unwrap();
        assert_eq!(
            line,
            Line {
                name: "Lait demi-écrémé".into(),
                quantity: 6.0,
                unit: "bouteille".into(),
                expires_on: Some(date),
            }
        );
    }

    #[test]
    fn without_any_quantity_one_unit_is_received() {
        let line = received_line(&bare(), &grocery(None, None)).unwrap();
        assert_eq!(line.quantity, 1.0);
        assert_eq!(line.unit, "unité");
    }

    #[test]
    fn a_blank_unit_becomes_the_default_one() {
        let item = ReceivedItem {
            unit: Some("  ".into()),
            ..bare()
        };
        let line = received_line(&item, &grocery(Some(2.0), Some("L"))).unwrap();
        assert_eq!(line.unit, "unité");
    }

    #[test]
    fn a_blank_name_is_refused() {
        let item = ReceivedItem {
            name: Some("   ".into()),
            ..bare()
        };
        assert_eq!(
            bad_request(received_line(&item, &grocery(None, None))),
            "name_required"
        );
    }

    #[test]
    fn a_zero_or_negative_quantity_is_refused() {
        for q in [0.0, -1.0] {
            let item = ReceivedItem {
                quantity: Some(q),
                ..bare()
            };
            assert_eq!(
                bad_request(received_line(&item, &grocery(None, None))),
                "quantity_must_be_positive",
                "{q}"
            );
        }
        // A list item saved with a zero quantity cannot be received as is.
        assert_eq!(
            bad_request(received_line(&bare(), &grocery(Some(0.0), None))),
            "quantity_must_be_positive"
        );
    }

    #[test]
    fn without_a_mode_the_proposal_applies() {
        let line = Uuid::from_u128(1);
        assert_eq!(resolve_target(None, Some(line)).unwrap(), Some(line));
        assert_eq!(resolve_target(None, None).unwrap(), None);
    }

    #[test]
    fn new_always_makes_a_new_line() {
        let line = Uuid::from_u128(1);
        assert_eq!(
            resolve_target(Some(ReceptionMode::New), Some(line)).unwrap(),
            None
        );
        assert_eq!(
            resolve_target(Some(ReceptionMode::New), None).unwrap(),
            None
        );
    }

    #[test]
    fn merge_goes_to_the_proposed_line() {
        let line = Uuid::from_u128(1);
        assert_eq!(
            resolve_target(Some(ReceptionMode::Merge), Some(line)).unwrap(),
            Some(line)
        );
    }

    #[test]
    fn merge_with_nothing_to_merge_into_is_a_conflict() {
        assert!(matches!(
            resolve_target(Some(ReceptionMode::Merge), None),
            Err(AppError::Conflict(ref c)) if c == "no_stock_to_merge"
        ));
    }
}
