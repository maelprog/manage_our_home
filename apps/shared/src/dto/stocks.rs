//! Request/response shapes for the Stocks endpoints
//! (`apps/api/src/stocks/items.rs`), consumed by `apps/web`'s SSR client.
//! Kept field-for-field identical to `apps/api`'s wire structs so there is one
//! documented shape; only the fields `apps/web` needs are declared (serde
//! ignores extras on deserialize). The backend is *not* modified by this epic
//! — these mirror it, they don't replace it.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::validation::stocks::ExpiryStatus;

/// `POST /groups/:id/stock-items` request body. Mirrors
/// `CreateStockItemRequest`. `category`/`reorder_threshold` are omitted from
/// the wire when `None` (the backend treats an absent `category` as `NULL`,
/// and an absent `quantity` as `0.0` — we always send `quantity`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateStockItemRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub quantity: f64,
    pub unit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reorder_threshold: Option<f64>,
    /// Nearest expiry date (#401). Omitted when `None` (no date).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_on: Option<NaiveDate>,
    /// EAN/UPC code (#402), as the scan returned it. Omitted when `None`.
    /// One article per code in a family: a second one is a 409.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub barcode: Option<String>,
}

/// Serde's blanket `Option<T>` impl collapses an explicit `null` and a missing
/// key to the same `None`, so a naive `Option<Option<T>>` can never observe
/// `Some(None)`. This `deserialize_with` only runs when the key is present and
/// deserializes the inner `Option<T>` (which *does* map `null` to `None`),
/// making `Some(None)` reachable again — matching the backend's identical
/// helper so this DTO is a faithful, round-trippable mirror. See
/// <https://github.com/serde-rs/serde/issues/984>.
fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// `PATCH /groups/:id/stock-items/:item_id` request body. Mirrors
/// `UpdateStockItemRequest`. For `category`/`reorder_threshold`/`expires_on` the outer
/// `Option` distinguishes "leave untouched" (`None`, omitted from the wire)
/// from "clear" (`Some(None)`, sent as `null`) from "set" (`Some(Some(v))`).
/// The quantity-adjust action sends only `quantity`; the full edit sends every
/// field.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateStockItemRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_some",
        skip_serializing_if = "Option::is_none"
    )]
    pub category: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_some",
        skip_serializing_if = "Option::is_none"
    )]
    pub reorder_threshold: Option<Option<f64>>,
    #[serde(
        default,
        deserialize_with = "deserialize_some",
        skip_serializing_if = "Option::is_none"
    )]
    pub expires_on: Option<Option<NaiveDate>>,
}

/// `GET/POST/PATCH /groups/:id/stock-items[/:item_id]` response body. Mirrors
/// `StockItemResponse`. `low_stock` is derived by the backend on read
/// (`quantity <= reorder_threshold`), never stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Nearest expiry date (#401).
    #[serde(default)]
    pub expires_on: Option<NaiveDate>,
    /// Derived by the backend on read from `expires_on` and today in
    /// Europe/Paris (`validation::stocks::expiry_status`), never stored.
    pub expiry_status: ExpiryStatus,
    /// EAN/UPC code (#402), `None` for an article entered without one.
    #[serde(default)]
    pub barcode: Option<String>,
}

/// `GET /groups/:id/stock-items?sort=…` values. Absent → name order; the
/// backend 400s anything else (`invalid_sort`).
pub const SORT_BY_EXPIRY: &str = "expires_on";

/// `GET /groups/:id/stock-items` response envelope (`{ "items": [...] }`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StockItemList {
    pub items: Vec<StockItemResponse>,
}

/// `POST /groups/:id/stock-items/scan` request body (#402): the string a
/// barcode decoder produced, or the code typed by hand, as is. Every check
/// on it happens in apps/api (`stocks::barcode`, `stocks::gs1`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanRequest {
    pub raw: String,
}

/// What Open Food Facts knows of a scanned product, kept only when the
/// record has a name (a nameless record reads as unknown).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScannedProduct {
    pub name: String,
    /// The pack size as Open Food Facts writes it (`"400 g"`), free text.
    pub quantity: Option<String>,
    /// Open Food Facts' category tags (`"en:spreads"`).
    #[serde(default)]
    pub categories_tags: Vec<String>,
}

/// Where a pre-filled expiry date comes from: `gs1`, the date a GS1 2D code
/// carries (#403); `category`, the scan's day plus a default shelf life for
/// the product's Open Food Facts category (#404).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpiresOnSource {
    Gs1,
    Category,
}

/// `POST /groups/:id/stock-items/scan` response body (#402). The scan
/// writes nothing to the family's stock: it reads the code, the stock and
/// the product record, and the page decides what to offer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanResult {
    /// The code in its stored form (a UPC-A becomes its EAN-13, and the
    /// GTIN-14 of a GS1 2D code the EAN/UPC of the same product). A string
    /// that is not a code gets a 422 instead of a `ScanResult`, so this is
    /// always `Some` in a 200.
    pub code: Option<String>,
    /// A store's weighing label (GS1 prefixes 20–29, and 020–029 for a
    /// UPC-A in its EAN-13 form): Open Food Facts was
    /// not asked, and `product` is `None`.
    pub weighed: bool,
    /// `None` when the product is unknown, its record has no name, the code
    /// is a weighing label, an article of the family already carries the
    /// code, or Open Food Facts could not be reached.
    pub product: Option<ScannedProduct>,
    /// The family's article that already carries this code: a rescan adds
    /// to it rather than creating a second one.
    pub existing_item_id: Option<Uuid>,
    /// The date proposed for the article, in this order: the one a GS1 2D
    /// code carries (#403) — its expiry date `(17)`, else its best-before
    /// date `(15)`; else, when `product` is known, the scan's day (Paris)
    /// plus its category's default shelf life (#404); else `None`. Never
    /// written to an article by the scan: the page pre-fills it, or
    /// proposes it for the article already in stock.
    pub expires_on: Option<NaiveDate>,
    /// Where `expires_on` comes from; `Some` exactly when it is.
    pub expires_on_source: Option<ExpiresOnSource>,
    /// The French name of the category that gave `expires_on`, `Some`
    /// exactly when `expires_on_source` is `Category`.
    pub expires_on_category: Option<String>,
}
