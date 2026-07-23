//! Request/response shapes for the Budget endpoints
//! (`apps/api/src/budget/entries.rs`), consumed by `apps/web`'s SSR client.
//! Kept field-for-field identical to `apps/api`'s wire structs so there is one
//! documented shape; only the fields `apps/web` needs are declared (serde
//! ignores extras on deserialize). The backend is *not* modified by this epic
//! — these mirror it, they don't replace it.
//!
//! `amount` is **euros** (`f64`) on the wire, matching the API boundary; the
//! backend converts to/from `amount_cents` internally. The front never does
//! money maths — it displays and parses the euro `f64` and leaves cents to the
//! backend (`validation::budget` for the display/parse helpers).

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `POST /groups/:id/budget-entries` request body. Mirrors
/// `CreateBudgetEntryRequest`. `spent_at` is omitted from the wire when `None`
/// (the backend defaults it to `current_date`); `grocery_item_id` is only ever
/// set by the price-on-checkout path, never by the manual `/budget/new` form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateBudgetEntryRequest {
    pub name: String,
    pub amount: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spent_at: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grocery_item_id: Option<Uuid>,
}

/// `PATCH /groups/:id/budget-entries/:entry_id` request body. Mirrors
/// `UpdateBudgetEntryRequest`. **No double-`Option`**: `spent_at` is `NOT NULL`
/// in the DB, so a plain `Option` means "leave as-is" only — there is no
/// "clear" state (unlike the grocery/stock quantity fields). Each field, when
/// present, overwrites; when omitted, the backend keeps the stored value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateBudgetEntryRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spent_at: Option<NaiveDate>,
}

/// `POST /groups/:id/grocery-items/:item_id/price` request body. Mirrors
/// `SetGroceryItemPriceRequest`. Associates a price with a grocery item once
/// it's checked/bought; the backend upserts on the item so a re-tap is
/// idempotent (re-tarifies rather than double-counting).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetGroceryItemPriceRequest {
    pub amount: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spent_at: Option<NaiveDate>,
}

/// `GET/POST/PATCH /groups/:id/budget-entries[/:entry_id]` response body.
/// Mirrors `BudgetEntryResponse`. `grocery_item_id` is set only when the entry
/// was created via the price-on-checkout path; `name` is denormalized so it
/// survives the grocery item being deleted. `amount` is euros.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetEntryResponse {
    pub id: Uuid,
    pub group_id: Uuid,
    pub created_by: Uuid,
    pub grocery_item_id: Option<Uuid>,
    pub name: String,
    pub amount: f64,
    pub spent_at: NaiveDate,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `GET /groups/:id/budget-entries` response envelope (`{ "entries": [...] }`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetEntryList {
    pub entries: Vec<BudgetEntryResponse>,
}

/// One month's cumulated spend. Mirrors `BudgetPeriodTotal`: `period` is the
/// first day of the month, `total` is euros (the backend sums `amount_cents`
/// then divides). The wire field is `total` (euros), *not* `total_cents`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetPeriodTotal {
    pub period: NaiveDate,
    pub total: f64,
}

/// `GET /groups/:id/budget-entries/summary` response envelope
/// (`{ "periods": [...] }`, newest month first).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetSummary {
    pub periods: Vec<BudgetPeriodTotal>,
}
