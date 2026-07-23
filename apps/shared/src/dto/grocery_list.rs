//! Request/response shapes for the Grocery-list endpoints
//! (`apps/api/src/grocery_list/items.rs`), consumed by `apps/web`'s SSR
//! client. Kept field-for-field identical to `apps/api`'s wire structs so
//! there is one documented shape; only the fields `apps/web` needs are
//! declared (serde ignores extras on deserialize). The backend is *not*
//! modified by this epic — these mirror it, they don't replace it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `POST /groups/:id/grocery-items` request body. Mirrors
/// `CreateGroceryItemRequest`. `quantity`/`unit` are omitted from the wire
/// when `None` (the backend treats an absent quantity/unit as `NULL`); only
/// `name` is required.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateGroceryItemRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

/// Serde's blanket `Option<T>` impl collapses an explicit `null` and a missing
/// key to the same `None`, so a naive `Option<Option<T>>` can never observe
/// `Some(None)`. This `deserialize_with` only runs when the key is present and
/// deserializes the inner `Option<T>` (which *does* map `null` to `None`),
/// making `Some(None)` reachable again — matching the backend's identical
/// helper so this DTO is a faithful, round-trippable mirror. See
/// <https://github.com/serde-rs/serde/issues/984> and the twins in
/// `dto/stocks.rs` / `dto/recipes.rs`.
fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// `PATCH /groups/:id/grocery-items/:item_id` request body. Mirrors
/// `UpdateGroceryItemRequest`. For `quantity`/`unit` the outer `Option`
/// distinguishes "leave untouched" (`None`, omitted from the wire) from
/// "clear" (`Some(None)`, sent as `null`) from "set" (`Some(Some(v))`). The
/// edit form always sends every field, so a blank quantity/unit clears it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateGroceryItemRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_some",
        skip_serializing_if = "Option::is_none"
    )]
    pub quantity: Option<Option<f64>>,
    #[serde(
        default,
        deserialize_with = "deserialize_some",
        skip_serializing_if = "Option::is_none"
    )]
    pub unit: Option<Option<String>>,
}

/// `POST /groups/:id/grocery-items/:item_id/check` request body. Mirrors
/// `CheckGroceryItemRequest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckGroceryItemRequest {
    pub checked: bool,
}

/// `GET/POST/PATCH /groups/:id/grocery-items[/:item_id]` response body.
/// Mirrors `GroceryItemResponse`. `source` is one of `manual`/`recipe`/
/// `low_stock` (informational provenance); `source_recipe_id` is set only
/// when the row was generated from a recipe's missing ingredients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroceryItemResponse {
    pub id: Uuid,
    pub group_id: Uuid,
    pub created_by: Uuid,
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    pub checked: bool,
    pub source: String,
    pub source_recipe_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `GET /groups/:id/grocery-items` and `POST …/generate` response envelope
/// (`{ "items": [...] }`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroceryItemList {
    pub items: Vec<GroceryItemResponse>,
}
