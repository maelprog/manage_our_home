use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{NaiveDate, Utc};
use manage_our_home_shared::dto::stocks::SORT_BY_EXPIRY;
use manage_our_home_shared::validation::auth::paris_day;
use manage_our_home_shared::validation::stocks::{expiry_status, ExpiryStatus};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::stocks::can_modify;
use crate::AppState;

#[derive(Deserialize)]
pub struct CreateStockItemRequest {
    pub name: String,
    pub category: Option<String>,
    #[serde(default)]
    pub quantity: f64,
    #[serde(default = "default_unit")]
    pub unit: String,
    pub reorder_threshold: Option<f64>,
    /// The nearest expiry date among the article's units (#401: one date per
    /// article, no per-batch tracking). Absent or `null` → no date.
    pub expires_on: Option<NaiveDate>,
}

fn default_unit() -> String {
    "unit".to_string()
}

/// Serde's blanket `Option<T>` impl treats an explicit JSON `null` the same
/// as a missing key (both call `visit_none`), so a naive `Option<Option<T>>`
/// field can never observe `Some(None)` — `{"field": null}` deserializes to
/// `None` just like an absent field. Forcing the value through this
/// `deserialize_with` skips that blanket impl: it only runs when the key is
/// present, deserializes the inner `Option<T>` (which *does* turn `null`
/// into `None`), and wraps the result in `Some`, so `Some(None)` becomes
/// reachable again. See https://github.com/serde-rs/serde/issues/984.
fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
pub struct UpdateStockItemRequest {
    pub name: Option<String>,
    /// `Some(None)` clears the category; `None` leaves it untouched.
    #[serde(default, deserialize_with = "deserialize_some")]
    pub category: Option<Option<String>>,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    /// `Some(None)` clears the threshold; `None` leaves it untouched.
    #[serde(default, deserialize_with = "deserialize_some")]
    pub reorder_threshold: Option<Option<f64>>,
    /// `Some(None)` clears the expiry date; `None` leaves it untouched.
    #[serde(default, deserialize_with = "deserialize_some")]
    pub expires_on: Option<Option<NaiveDate>>,
}

impl UpdateStockItemRequest {
    /// Which permission bar this PATCH must clear. A **quantity-only**
    /// adjustment (or a no-op that touches no field) is open to any group
    /// member — the shared-inventory "any member may adjust the quantity"
    /// tier from issue #19. Touching any full-record field (name, category,
    /// unit, reorder_threshold, expires_on) makes it a full edit, which stays behind
    /// `can_modify` (creator/admin/owner), the same bar as delete.
    fn touches_full_record(&self) -> bool {
        self.name.is_some()
            || self.category.is_some()
            || self.unit.is_some()
            || self.reorder_threshold.is_some()
            || self.expires_on.is_some()
    }
}

#[derive(Serialize)]
pub struct StockItemResponse {
    pub id: Uuid,
    pub group_id: Uuid,
    pub created_by: Uuid,
    pub name: String,
    pub category: Option<String>,
    pub quantity: f64,
    pub unit: String,
    pub reorder_threshold: Option<f64>,
    pub low_stock: bool,
    pub expires_on: Option<NaiveDate>,
    /// Derived on read from `expires_on` against today in Europe/Paris — the
    /// fixed v1 display timezone — never stored, like `low_stock` (#401).
    pub expiry_status: ExpiryStatus,
}

struct StockItemRow {
    id: Uuid,
    group_id: Uuid,
    created_by: Uuid,
    name: String,
    category: Option<String>,
    quantity: f64,
    unit: String,
    reorder_threshold: Option<f64>,
    expires_on: Option<NaiveDate>,
}

/// "Today" for the expiry status: the civil day in Europe/Paris, the fixed v1
/// display timezone the web pages already judge dates in (`today_paris`). The
/// database's `current_date` would be the server's UTC day, wrong between
/// midnight and 1–2 a.m. in Paris.
fn today() -> NaiveDate {
    paris_day(Utc::now())
}

impl StockItemResponse {
    fn from_row(r: StockItemRow, today: NaiveDate) -> Self {
        let low_stock = r
            .reorder_threshold
            .map(|t| r.quantity <= t)
            .unwrap_or(false);
        StockItemResponse {
            id: r.id,
            group_id: r.group_id,
            created_by: r.created_by,
            name: r.name,
            category: r.category,
            quantity: r.quantity,
            unit: r.unit,
            reorder_threshold: r.reorder_threshold,
            low_stock,
            expires_on: r.expires_on,
            expiry_status: expiry_status(r.expires_on, today),
        }
    }
}

fn validate_request(quantity: f64, reorder_threshold: Option<f64>) -> AppResult<()> {
    if quantity < 0.0 {
        return Err(AppError::BadRequest("quantity_must_be_non_negative".into()));
    }
    if let Some(t) = reorder_threshold {
        if t < 0.0 {
            return Err(AppError::BadRequest(
                "reorder_threshold_must_be_non_negative".into(),
            ));
        }
    }
    Ok(())
}

pub async fn create_stock_item(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<CreateStockItemRequest>,
) -> AppResult<impl IntoResponse> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("name_required".into()));
    }
    if body.unit.trim().is_empty() {
        return Err(AppError::BadRequest("unit_required".into()));
    }
    validate_request(body.quantity, body.reorder_threshold)?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let item = sqlx::query_as!(
        StockItemRow,
        r#"
        INSERT INTO stock_items (group_id, created_by, name, category, quantity, unit, reorder_threshold, expires_on)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING id, group_id, created_by, name, category, quantity, unit, reorder_threshold, expires_on
        "#,
        group_id,
        auth.user_id,
        name,
        body.category,
        body.quantity,
        body.unit,
        body.reorder_threshold,
        body.expires_on,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(StockItemResponse::from_row(item, today())),
    ))
}

pub async fn get_stock_item(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, item_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let item = sqlx::query_as!(
        StockItemRow,
        r#"SELECT id, group_id, created_by, name, category, quantity, unit, reorder_threshold, expires_on
           FROM stock_items WHERE id = $1 AND group_id = $2"#,
        item_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    tx.commit().await?;

    Ok(Json(StockItemResponse::from_row(item, today())))
}

#[derive(Deserialize)]
pub struct ListStockItemsQuery {
    /// When true, only items at or below their reorder threshold are
    /// returned (used by the future Recipes/Grocery-list epics to compute
    /// "what's missing").
    #[serde(default)]
    pub low_stock: bool,
    /// `expires_on` → soonest expiry first, undated items last, then by name
    /// ("à consommer en premier", #401). Absent → by name. Anything else is a
    /// 400 rather than a silently ignored typo.
    pub sort: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StockSort {
    Name,
    ExpiresOn,
}

fn parse_sort(sort: Option<&str>) -> AppResult<StockSort> {
    match sort {
        None => Ok(StockSort::Name),
        Some(SORT_BY_EXPIRY) => Ok(StockSort::ExpiresOn),
        Some(_) => Err(AppError::BadRequest("invalid_sort".into())),
    }
}

pub async fn list_stock_items(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Query(query): Query<ListStockItemsQuery>,
) -> AppResult<impl IntoResponse> {
    let by_expiry = parse_sort(query.sort.as_deref())? == StockSort::ExpiresOn;
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let rows = sqlx::query_as!(
        StockItemRow,
        r#"
        SELECT id, group_id, created_by, name, category, quantity, unit, reorder_threshold, expires_on
        FROM stock_items
        WHERE group_id = $1
        -- With $2 false the CASE is NULL on every row and only `name` orders.
        ORDER BY CASE WHEN $2 THEN expires_on END ASC NULLS LAST, name
        "#,
        group_id,
        by_expiry,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let today = today();
    let items: Vec<StockItemResponse> = rows
        .into_iter()
        .map(|row| StockItemResponse::from_row(row, today))
        .filter(|item| !query.low_stock || item.low_stock)
        .collect();

    Ok(Json(json!({ "items": items })))
}

pub async fn update_stock_item(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, item_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateStockItemRequest>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;

    // FOR UPDATE locks the row for the rest of this transaction, so a
    // concurrent PATCH on the same item blocks on this SELECT until we
    // commit instead of both transactions reading the same stale quantity
    // and one overwriting the other's write (lost update).
    let existing = sqlx::query!(
        "SELECT created_by, name, category, quantity, unit, reorder_threshold, expires_on FROM stock_items WHERE id = $1 AND group_id = $2 FOR UPDATE",
        item_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    // A quantity-only adjustment is allowed to any member (shared inventory);
    // touching any full-record field requires the creator/admin/owner bar.
    if body.touches_full_record() && !can_modify(&actor_role, existing.created_by == auth.user_id) {
        return Err(AppError::Forbidden);
    }

    let name = match body.name.as_deref().map(str::trim) {
        Some("") => return Err(AppError::BadRequest("name_required".into())),
        Some(n) => Some(n.to_string()),
        None => None,
    };
    let unit = match body.unit.as_deref().map(str::trim) {
        Some("") => return Err(AppError::BadRequest("unit_required".into())),
        Some(u) => Some(u.to_string()),
        None => None,
    };
    let category = match body.category {
        Some(c) => c,
        None => existing.category,
    };

    let quantity = body.quantity.unwrap_or(existing.quantity);
    let reorder_threshold = match body.reorder_threshold {
        Some(t) => t,
        None => existing.reorder_threshold,
    };
    let expires_on = match body.expires_on {
        Some(d) => d,
        None => existing.expires_on,
    };
    validate_request(quantity, reorder_threshold)?;

    let item = sqlx::query_as!(
        StockItemRow,
        r#"
        UPDATE stock_items SET
            name = COALESCE($3, name),
            category = $4,
            quantity = $5,
            unit = COALESCE($6, unit),
            reorder_threshold = $7,
            expires_on = $8,
            updated_at = now()
        WHERE id = $1 AND group_id = $2
        RETURNING id, group_id, created_by, name, category, quantity, unit, reorder_threshold, expires_on
        "#,
        item_id,
        group_id,
        name,
        category,
        quantity,
        unit,
        reorder_threshold,
        expires_on,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(StockItemResponse::from_row(item, today())))
}

pub async fn delete_stock_item(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, item_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;

    let existing = sqlx::query!(
        "SELECT created_by FROM stock_items WHERE id = $1 AND group_id = $2",
        item_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    if !can_modify(&actor_role, existing.created_by == auth.user_id) {
        return Err(AppError::Forbidden);
    }

    sqlx::query!(
        "DELETE FROM stock_items WHERE id = $1 AND group_id = $2",
        item_id,
        group_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_update() -> UpdateStockItemRequest {
        UpdateStockItemRequest {
            name: None,
            category: None,
            quantity: None,
            unit: None,
            reorder_threshold: None,
            expires_on: None,
        }
    }

    #[test]
    fn quantity_only_is_not_a_full_record_edit() {
        let req = UpdateStockItemRequest {
            quantity: Some(3.0),
            ..empty_update()
        };
        assert!(!req.touches_full_record());
    }

    #[test]
    fn a_no_op_patch_is_not_a_full_record_edit() {
        assert!(!empty_update().touches_full_record());
    }

    #[test]
    fn changing_the_name_is_a_full_record_edit() {
        let req = UpdateStockItemRequest {
            name: Some("Farine".into()),
            ..empty_update()
        };
        assert!(req.touches_full_record());
    }

    #[test]
    fn changing_the_unit_is_a_full_record_edit() {
        let req = UpdateStockItemRequest {
            unit: Some("kg".into()),
            ..empty_update()
        };
        assert!(req.touches_full_record());
    }

    #[test]
    fn setting_or_clearing_the_category_is_a_full_record_edit() {
        let set = UpdateStockItemRequest {
            category: Some(Some("Cereales".into())),
            ..empty_update()
        };
        assert!(set.touches_full_record());
        let cleared = UpdateStockItemRequest {
            category: Some(None),
            ..empty_update()
        };
        assert!(cleared.touches_full_record());
    }

    #[test]
    fn setting_or_clearing_the_reorder_threshold_is_a_full_record_edit() {
        let set = UpdateStockItemRequest {
            reorder_threshold: Some(Some(0.5)),
            ..empty_update()
        };
        assert!(set.touches_full_record());
        let cleared = UpdateStockItemRequest {
            reorder_threshold: Some(None),
            ..empty_update()
        };
        assert!(cleared.touches_full_record());
    }

    #[test]
    fn setting_or_clearing_the_expiry_date_is_a_full_record_edit() {
        let set = UpdateStockItemRequest {
            expires_on: Some(NaiveDate::from_ymd_opt(2026, 10, 9)),
            ..empty_update()
        };
        assert!(set.touches_full_record());
        let cleared = UpdateStockItemRequest {
            expires_on: Some(None),
            ..empty_update()
        };
        assert!(cleared.touches_full_record());
    }

    #[test]
    fn an_explicit_null_expiry_date_is_a_clear_not_an_absent_field() {
        let cleared: UpdateStockItemRequest =
            serde_json::from_value(serde_json::json!({ "expires_on": null })).unwrap();
        assert_eq!(cleared.expires_on, Some(None));
        let absent: UpdateStockItemRequest =
            serde_json::from_value(serde_json::json!({ "quantity": 1.0 })).unwrap();
        assert_eq!(absent.expires_on, None);
    }

    fn row(expires_on: Option<NaiveDate>) -> StockItemRow {
        StockItemRow {
            id: Uuid::nil(),
            group_id: Uuid::nil(),
            created_by: Uuid::nil(),
            name: "Yaourt".into(),
            category: None,
            quantity: 1.0,
            unit: "pot".into(),
            reorder_threshold: None,
            expires_on,
        }
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn the_response_derives_the_expiry_status_from_the_date_and_today() {
        let today = day(2026, 10, 7);
        let status = |d| StockItemResponse::from_row(row(d), today).expiry_status;
        assert_eq!(status(Some(day(2026, 10, 6))), ExpiryStatus::Expired);
        assert_eq!(status(Some(day(2026, 10, 7))), ExpiryStatus::Soon);
        assert_eq!(status(Some(day(2026, 10, 10))), ExpiryStatus::Soon);
        assert_eq!(status(Some(day(2026, 10, 11))), ExpiryStatus::Ok);
        assert_eq!(status(None), ExpiryStatus::Unknown);
    }

    #[test]
    fn the_expiry_status_goes_on_the_wire_in_snake_case() {
        let body = serde_json::to_value(StockItemResponse::from_row(
            row(Some(day(2026, 10, 6))),
            day(2026, 10, 7),
        ))
        .unwrap();
        assert_eq!(body["expiry_status"], "expired");
        assert_eq!(body["expires_on"], "2026-10-06");
        let undated =
            serde_json::to_value(StockItemResponse::from_row(row(None), day(2026, 10, 7))).unwrap();
        assert_eq!(undated["expiry_status"], "unknown");
    }

    #[test]
    fn no_sort_parameter_is_name_order() {
        assert_eq!(parse_sort(None).unwrap(), StockSort::Name);
    }

    #[test]
    fn sort_expires_on_is_expiry_order() {
        assert_eq!(
            parse_sort(Some("expires_on")).unwrap(),
            StockSort::ExpiresOn
        );
    }

    #[test]
    fn an_unknown_sort_is_a_bad_request() {
        for bad in ["", "expiry", "name", "EXPIRES_ON", "expires_on desc"] {
            assert!(
                matches!(parse_sort(Some(bad)), Err(AppError::BadRequest(ref c)) if c == "invalid_sort"),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn quantity_plus_any_full_field_is_still_a_full_record_edit() {
        let req = UpdateStockItemRequest {
            quantity: Some(1.0),
            name: Some("Riz".into()),
            ..empty_update()
        };
        assert!(req.touches_full_record());
    }
}
