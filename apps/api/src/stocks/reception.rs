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
//! falling back to the same rule for an item sent without a mode. Both plan
//! the lot with the same function (`plan`), so a lot sent back as `GET`
//! proposed it is applied exactly as proposed.
//!
//! Permissions (user arbitration of 2026-10-25): those of stock creation,
//! any group member — `can_modify` is deliberately not called. A member
//! receives, and so removes from the list, items another member added; a
//! merge is a quantity-only adjustment, which any member may already make.

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
/// and the stock line concerned — on `GET` the existing line a merge would
/// add to (`null` for `new`, and for a merge into the line an earlier
/// article of the lot creates), on `POST` the line as the reception left it.
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

/// The largest quantity a received article may carry, and a merged line
/// reach: far above any household stock, far below where `float8` overflows
/// (a sum of two huge quantities used to end in a 500).
const MAX_QUANTITY: f64 = 1_000_000_000.0;

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
/// quantity must be positive and at most `MAX_QUANTITY`.
fn received_line(item: &ReceivedItem, grocery: &GroceryLine) -> AppResult<Line> {
    let name = item.name.as_deref().unwrap_or(&grocery.name).trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("name_required".into()));
    }
    let quantity = item.quantity.or(grocery.quantity).unwrap_or(1.0);
    if !quantity.is_finite() || quantity > MAX_QUANTITY {
        return Err(AppError::BadRequest("quantity_too_large".into()));
    }
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

/// Where an article of a lot goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// A line of its own.
    New,
    /// An existing stock line.
    Stock(Uuid),
    /// The line an earlier article of the same lot creates (its index in
    /// the lot).
    Lot(usize),
}

impl Target {
    fn mode(self) -> ReceptionMode {
        match self {
            Target::New => ReceptionMode::New,
            Target::Stock(_) | Target::Lot(_) => ReceptionMode::Merge,
        }
    }
}

/// Plans a lot, article by article in its order, so that `GET` proposes
/// exactly what `POST` applies. An article that makes a new line is, for the
/// articles after it, a candidate like any stock line, under its grocery
/// item's id: two « lait » of one lot give `new` then `merge`.
fn plan<'a>(
    articles: &[(Uuid, &'a Line, Option<ReceptionMode>)],
    stock: &[StockCandidate<'a>],
) -> AppResult<Vec<Target>> {
    let mut candidates = stock.to_vec();
    // The grocery item id each new line of the lot is a candidate under,
    // and the index of the article that creates it.
    let mut created: Vec<(Uuid, usize)> = Vec::new();
    let mut targets = Vec::with_capacity(articles.len());
    for (index, (grocery_item_id, line, mode)) in articles.iter().enumerate() {
        let proposal = merge_target(&line.name, &line.unit, line.expires_on, &candidates);
        let target = match resolve_target(*mode, proposal)? {
            Some(id) => match created.iter().find(|(c, _)| *c == id) {
                Some(&(_, creator)) => Target::Lot(creator),
                None => Target::Stock(id),
            },
            None => {
                candidates.push(StockCandidate {
                    id: *grocery_item_id,
                    name: &line.name,
                    unit: &line.unit,
                    expires_on: line.expires_on,
                });
                created.push((*grocery_item_id, index));
                Target::New
            }
        };
        targets.push(target);
    }
    Ok(targets)
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

    // A checked item the list holds with a zero quantity (or one above
    // `MAX_QUANTITY`) cannot be pre-filled; the POST refuses it the same way.
    let received: Vec<(Uuid, Line)> = checked
        .iter()
        .filter_map(|grocery| {
            let item = ReceivedItem {
                grocery_item_id: grocery.id,
                name: None,
                quantity: None,
                unit: None,
                expires_on: None,
                mode: None,
            };
            received_line(&item, grocery).ok().map(|l| (grocery.id, l))
        })
        .collect();
    // The lot as the web would send it back unchanged, in this order.
    let articles: Vec<_> = received.iter().map(|(id, l)| (*id, l, None)).collect();
    let targets = plan(&articles, &candidates(&stock))?;

    let today = today();
    let lines: Vec<ReceptionLineResponse> = received
        .into_iter()
        .zip(targets)
        .map(|((grocery_item_id, line), target)| ReceptionLineResponse {
            grocery_item_id,
            name: line.name,
            quantity: line.quantity,
            unit: line.unit,
            expires_on: None,
            mode: target.mode(),
            stock_item: match target {
                Target::Stock(id) => stock
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| StockItemResponse::from_row(s.clone(), today)),
                Target::New | Target::Lot(_) => None,
            },
        })
        .collect();

    Ok(Json(json!({ "items": lines })))
}

/// Receives a lot of grocery items into the stock, in one transaction: an
/// item of another group (or already gone) is a 404 and an invalid one a
/// 400/409, and either leaves the list and the stock untouched. Open to any
/// group member, whoever added the items (see the module doc).
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

    let stock = sqlx::query_as!(
        StockItemRow,
        r#"SELECT id, group_id, created_by, name, category, quantity, unit, reorder_threshold, expires_on, barcode
           FROM stock_items WHERE group_id = $1"#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;

    let mut lines = Vec::with_capacity(body.items.len());
    for item in &body.items {
        let grocery = groceries
            .iter()
            .find(|g| g.id == item.grocery_item_id)
            .ok_or(AppError::NotFound)?;
        lines.push(received_line(item, grocery)?);
    }
    let articles: Vec<_> = body
        .items
        .iter()
        .zip(&lines)
        .map(|(item, line)| (item.grocery_item_id, line, item.mode))
        .collect();
    let targets = plan(&articles, &candidates(&stock))?;

    // The id of the line each article of the lot ended on, for the
    // articles that merge into a line an earlier one created.
    let mut landed: Vec<Uuid> = Vec::with_capacity(lines.len());
    let mut received = Vec::with_capacity(lines.len());
    for ((item, line), target) in body.items.iter().zip(lines).zip(targets) {
        let merge_into = match target {
            Target::Stock(id) => Some(id),
            Target::Lot(creator) => Some(landed[creator]),
            Target::New => None,
        };
        let row = match merge_into {
            // The increment is done by the database, so a concurrent
            // quantity adjustment is not lost; the guard keeps the sum under
            // `MAX_QUANTITY` (and `float8` from overflowing) without
            // computing it first.
            Some(stock_id) => match sqlx::query_as!(
                StockItemRow,
                r#"
                UPDATE stock_items SET quantity = quantity + $3, updated_at = now()
                WHERE id = $1 AND group_id = $2 AND quantity <= $4::float8 - $3::float8
                RETURNING id, group_id, created_by, name, category, quantity, unit, reorder_threshold, expires_on, barcode
                "#,
                stock_id,
                group_id,
                line.quantity,
                MAX_QUANTITY,
            )
            .fetch_optional(&mut *tx)
            .await?
            {
                Some(row) => row,
                None => {
                    let exists = sqlx::query_scalar!(
                        r#"SELECT EXISTS (SELECT 1 FROM stock_items WHERE id = $1 AND group_id = $2) AS "exists!""#,
                        stock_id,
                        group_id,
                    )
                    .fetch_one(&mut *tx)
                    .await?;
                    return Err(if exists {
                        AppError::BadRequest("quantity_too_large".into())
                    } else {
                        AppError::NotFound
                    });
                }
            },
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

        landed.push(row.id);
        received.push((item.grocery_item_id, line, target.mode(), row));
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
    fn a_quantity_above_the_bound_is_refused() {
        let item = ReceivedItem {
            quantity: Some(MAX_QUANTITY * 2.0),
            ..bare()
        };
        assert_eq!(
            bad_request(received_line(&item, &grocery(None, None))),
            "quantity_too_large"
        );
        // A list item saved with a huge quantity cannot be received as is.
        assert_eq!(
            bad_request(received_line(&bare(), &grocery(Some(1e308), None))),
            "quantity_too_large"
        );
        let at_bound = ReceivedItem {
            quantity: Some(MAX_QUANTITY),
            ..bare()
        };
        assert!(received_line(&at_bound, &grocery(None, None)).is_ok());
    }

    #[test]
    fn a_non_finite_quantity_is_refused() {
        for q in [f64::INFINITY, f64::NAN] {
            let item = ReceivedItem {
                quantity: Some(q),
                ..bare()
            };
            assert!(
                matches!(
                    received_line(&item, &grocery(None, None)),
                    Err(AppError::BadRequest(_))
                ),
                "{q}"
            );
        }
    }

    fn article(name: &str, unit: &str, expires_on: Option<NaiveDate>) -> Line {
        Line {
            name: name.into(),
            quantity: 1.0,
            unit: unit.into(),
            expires_on,
        }
    }

    fn gid(n: u128) -> Uuid {
        Uuid::from_u128(1000 + n)
    }

    #[test]
    fn two_articles_of_one_lot_without_stock_give_new_then_merge() {
        let a = article("Lait", "L", None);
        let b = article("lait", "L", None);
        let lot = [(gid(1), &a, None), (gid(2), &b, None)];
        assert_eq!(plan(&lot, &[]).unwrap(), vec![Target::New, Target::Lot(0)]);
    }

    #[test]
    fn an_article_matching_the_stock_merges_into_it() {
        let milk = Uuid::from_u128(1);
        let stock = [StockCandidate {
            id: milk,
            name: "Lait",
            unit: "L",
            expires_on: None,
        }];
        let a = article("lait", "L", None);
        let b = article("lait", "L", None);
        let lot = [(gid(1), &a, None), (gid(2), &b, None)];
        assert_eq!(
            plan(&lot, &stock).unwrap(),
            vec![Target::Stock(milk), Target::Stock(milk)]
        );
    }

    #[test]
    fn a_forced_new_line_is_a_candidate_for_the_articles_after_it() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 5);
        let a = article("Lait", "L", date);
        let b = article("Lait", "L", date);
        let c = article("Lait", "L", None);
        let lot = [
            (gid(1), &a, Some(ReceptionMode::New)),
            (gid(2), &b, None),
            (gid(3), &c, Some(ReceptionMode::Merge)),
        ];
        assert_eq!(
            plan(&lot, &[]).unwrap(),
            vec![Target::New, Target::Lot(0), Target::Lot(0)]
        );
    }

    #[test]
    fn a_lot_line_dated_differently_is_not_merged_into() {
        let a = article("Lait", "L", NaiveDate::from_ymd_opt(2026, 11, 1));
        let b = article("Lait", "L", NaiveDate::from_ymd_opt(2026, 11, 5));
        let lot = [(gid(1), &a, None), (gid(2), &b, None)];
        assert_eq!(plan(&lot, &[]).unwrap(), vec![Target::New, Target::New]);
    }

    #[test]
    fn a_forced_merge_with_nothing_to_merge_into_refuses_the_plan() {
        let a = article("Lait", "L", None);
        let lot = [(gid(1), &a, Some(ReceptionMode::Merge))];
        assert!(matches!(
            plan(&lot, &[]),
            Err(AppError::Conflict(ref c)) if c == "no_stock_to_merge"
        ));
    }

    #[test]
    fn the_target_gives_the_mode() {
        assert_eq!(Target::New.mode(), ReceptionMode::New);
        assert_eq!(Target::Stock(Uuid::nil()).mode(), ReceptionMode::Merge);
        assert_eq!(Target::Lot(0).mode(), ReceptionMode::Merge);
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
