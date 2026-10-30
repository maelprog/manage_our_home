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
/// (the backend defaults it to today, the Paris day); `grocery_item_id` is only ever
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

// -- Categories and caps (#532) -----------------------------------------------
//
// Unlike the shapes above, these are not mirrors: `apps/api` serializes and
// deserializes these very structs, so the backend and `apps/web` (#533)
// cannot drift. Caps are euros on the wire, cents in the database, as
// `amount`. A cap of `null` is « no cap ».

/// Serde's blanket `Option<T>` impl reads an explicit `null` and a missing key
/// as the same `None`; this `deserialize_with` runs only when the key is
/// present, so `Some(None)` — « clear it » — is reachable (see the twin in
/// `dto/grocery_list.rs`).
fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// A spending category of the family. `color` is one of
/// `validation::budget::CATEGORY_COLORS`, a named key, never a hex value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetCategory {
    pub id: Uuid,
    pub group_id: Uuid,
    pub name: String,
    pub color: String,
    pub monthly_cap: Option<f64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `GET /groups/:id/budget-categories` (`{ "categories": [...] }`, by name).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetCategoryList {
    pub categories: Vec<BudgetCategory>,
}

/// `POST /groups/:id/budget-categories` request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateBudgetCategoryRequest {
    pub name: String,
    pub color: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly_cap: Option<f64>,
}

/// `PATCH /groups/:id/budget-categories/:category_id` request body. A field
/// left out keeps its value; `monthly_cap: null` removes the cap.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateBudgetCategoryRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_some",
        skip_serializing_if = "Option::is_none"
    )]
    pub monthly_cap: Option<Option<f64>>,
}

/// `GET` and `PUT /groups/:id/budget-settings`, response and request body:
/// the monthly cap for the whole family. `PUT` replaces it, so a body
/// without `monthly_cap` removes it, as `null` does.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BudgetSettings {
    #[serde(default)]
    pub monthly_cap: Option<f64>,
}

/// `GET` and `PATCH /groups/:id/budget-alert-settings` response (#536): the
/// family's two switches of the cap alerts, both on unless turned off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetAlertSettings {
    /// The alerts on the family's monthly cap: 80 % reached, cap reached.
    pub global_cap_alerts: bool,
    /// The alerts on a category's cap: cap exceeded.
    pub category_cap_alerts: bool,
}

/// `PATCH /groups/:id/budget-alert-settings` request body: a switch named
/// is set, a switch left out keeps its value.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct UpdateBudgetAlertSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_cap_alerts: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_cap_alerts: Option<bool>,
}

/// One category's spend over the summary's month.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetCategoryUsage {
    pub id: Uuid,
    pub name: String,
    pub color: String,
    pub total: f64,
    pub monthly_cap: Option<f64>,
    /// `validation::budget::cap_usage`: the total as a share of the cap,
    /// rounded down; `null` without a cap.
    pub percent: Option<i64>,
    /// The total is strictly above the cap.
    pub over_cap: bool,
}

/// The `month` member of `GET /groups/:id/budget-entries/summary`: one month
/// (`?month=YYYY-MM`, else the current one in Paris) against the caps. Every
/// category is listed, spent in or not, by name; `uncategorized` is the
/// spend filed under none (« Sans catégorie »). `total` is the whole month,
/// `uncategorized` included.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetMonthSummary {
    /// First day of the month.
    pub period: NaiveDate,
    pub total: f64,
    pub monthly_cap: Option<f64>,
    pub percent: Option<i64>,
    pub over_cap: bool,
    pub categories: Vec<BudgetCategoryUsage>,
    pub uncategorized: f64,
}
