//! `POST /groups/:id/stock-items/scan` (#402): what a scanned code stands
//! for in this family. Open to any member, like reading the stock, and it
//! writes nothing to the family's stock — adding the article, or adding one
//! to the article that already carries the code, is the page's next request
//! (`POST /groups/:id/stock-items`, or the quantity-only PATCH).
//!
//! The only write is to the Open Food Facts cache (`off_products`), public
//! data that belongs to no family (see migration 0026).
//!
//! `raw` is an EAN/UPC (`barcode`), or a GS1 2D code (#403): a DataMatrix
//! or QR element string, or a GS1 Digital Link URL (`gs1`). Its GTIN then
//! takes the EAN/UPC path, and the date it carries comes back in
//! `expires_on`, for the page to pre-fill — never written here.

use axum::extract::{Path, State};
use axum::Json;
use chrono::Utc;
use manage_our_home_shared::dto::stocks::{
    ExpiresOnSource, ScanRequest, ScanResult, ScannedProduct,
};
use manage_our_home_shared::validation::auth::paris_day;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::stocks::{barcode, gs1, openfoodfacts};
use crate::AppState;

pub async fn scan_stock_item(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<ScanRequest>,
) -> AppResult<Json<ScanResult>> {
    // The GS1 two-digit years are placed from the Paris day, like
    // `expiry_status`.
    let (code, expires_on) = match barcode::normalize(&body.raw) {
        Some(code) => (code, None),
        None => gs1::read(&body.raw, paris_day(Utc::now()))
            .map(|read| (read.code, read.expires_on))
            .ok_or_else(|| AppError::Unprocessable("invalid_barcode".into()))?,
    };
    let weighed = barcode::is_weighed(&code);

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    let existing_item_id = sqlx::query_scalar!(
        "SELECT id FROM stock_items WHERE group_id = $1 AND barcode = $2",
        group_id,
        code,
    )
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;

    // An article already carrying the code needs no product record: the page
    // offers to add one to it. A weighing label has no record anywhere.
    let product = if weighed || existing_item_id.is_some() {
        None
    } else {
        product_for(&state, &code).await?
    };

    Ok(Json(ScanResult {
        code: Some(code),
        weighed,
        product,
        existing_item_id,
        expires_on,
        expires_on_source: expires_on.map(|_| ExpiresOnSource::Gs1),
    }))
}

/// The product behind `code`: from the cache while it is fresh, else from
/// Open Food Facts, whose definite answers (a product, or none) are cached.
/// Open Food Facts failing or being throttled is not an error of the scan:
/// the answer is the expired record if there is one, else "no product",
/// and the page offers the manual form.
async fn product_for(state: &AppState, code: &str) -> AppResult<Option<ScannedProduct>> {
    // Outside the family's transaction: no row here belongs to a family, and
    // the call below may take up to `outbound_http::REQUEST_TIMEOUT`, which
    // no transaction should be held open across.
    let cached = sqlx::query!(
        "SELECT name, quantity, categories_tags, fetched_at FROM off_products WHERE code = $1",
        code,
    )
    .fetch_optional(&state.db)
    .await?;
    let mut stale = None;
    if let Some(row) = cached {
        let fresh = openfoodfacts::is_fresh(row.fetched_at, row.name.is_some(), Utc::now());
        let product = row.name.map(|name| ScannedProduct {
            name,
            quantity: row.quantity,
            categories_tags: row.categories_tags,
        });
        if fresh {
            return Ok(product);
        }
        stale = product;
    }

    match openfoodfacts::fetch(&state.openfoodfacts_base_url, code, &state.off_throttle).await {
        Ok(product) => {
            let (name, quantity, tags) = match &product {
                Some(p) => (
                    Some(p.name.as_str()),
                    p.quantity.as_deref(),
                    p.categories_tags.clone(),
                ),
                None => (None, None, Vec::new()),
            };
            sqlx::query!(
                r#"
                INSERT INTO off_products (code, name, quantity, categories_tags, fetched_at)
                VALUES ($1, $2, $3, $4, now())
                ON CONFLICT (code) DO UPDATE SET
                    name = EXCLUDED.name,
                    quantity = EXCLUDED.quantity,
                    categories_tags = EXCLUDED.categories_tags,
                    fetched_at = EXCLUDED.fetched_at
                "#,
                code,
                name,
                quantity,
                &tags,
            )
            .execute(&state.db)
            .await?;
            Ok(product)
        }
        // An expired record still beats no record at all.
        Err(_) => Ok(stale),
    }
}
