//! Request/response shapes for the Stocks endpoints
//! (`apps/api/src/stocks/items.rs`), consumed by `apps/web`'s SSR client.
//! Kept field-for-field identical to `apps/api`'s wire structs so there is one
//! documented shape; only the fields `apps/web` needs are declared (serde
//! ignores extras on deserialize). The backend is *not* modified by this epic
//! — these mirror it, they don't replace it.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
/// `UpdateStockItemRequest`. For `category`/`reorder_threshold` the outer
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
}

/// `GET /groups/:id/stock-items` response envelope (`{ "items": [...] }`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StockItemList {
    pub items: Vec<StockItemResponse>,
}
