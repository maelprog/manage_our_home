//! `/stocks/:id` — item detail with the low-stock and expiry states, plus the
//! quantity-adjust action and the delete action; full edit lives in `edit.rs`.
//!
//! Permission bar: the quantity-adjust form renders for **any** family member
//! (shared inventory — issue #39); the edit link and delete button render only
//! for the item's creator or a group admin/owner (`can_modify`). A standard
//! member viewing another member's item still gets the adjust form, plus a
//! muted note that editing/deleting is reserved. The backend stays the
//! authority — a forged full-edit/DELETE is 403'd and mapped to
//! `?error=forbidden` here defensively.
//!
//! Error table: 404 (`get_stock_item`) unknown/foreign item → introuvable
//! page; 403 (`update`/`delete` permission bar) → `?error=forbidden`.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::stocks::{StockItemResponse, UpdateStockItemRequest};
use manage_our_home_shared::validation::stocks::{
    check_quantities, format_quantity, is_low_stock, ItemFormError,
};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::new::parse_quantity;
use super::{
    can_modify, expiry_badge, family_context, fmt_date, fmt_num, forbidden_page,
    item_not_found_page, service_unavailable_page, stocks_cookie,
};

#[derive(serde::Deserialize)]
pub struct DetailQuery {
    notice: Option<String>,
    error: Option<String>,
}

fn notice_text(code: &str) -> Option<&'static str> {
    match code {
        "item_updated" => Some("Article mis à jour."),
        "quantity_adjusted" => Some("Quantité mise à jour."),
        _ => None,
    }
}

fn error_text(code: &str) -> Option<&'static str> {
    match code {
        "forbidden" => Some("Vous n'avez pas les droits nécessaires pour cette action."),
        "unavailable" => Some("Service momentanément indisponible, merci de réessayer."),
        "quantity_too_large" => Some("La quantité doit être un nombre d'au plus 1 000 000 000."),
        _ => None,
    }
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(item_id): Path<Uuid>,
    Query(query): Query<DetailQuery>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, &format!("/stocks/{item_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = stocks_cookie(&headers);

    let item = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/stock-items/{}", fam.gid, item_id),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match serde_json::from_value::<StockItemResponse>(resp.body) {
                Ok(i) => i,
                Err(_) => return service_unavailable_page().into_response(),
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return item_not_found_page().into_response()
        }
        _ => return service_unavailable_page().into_response(),
    };

    let can_edit = can_modify(&fam.role, item.created_by == me.user_id);
    let notice = query.notice.as_deref().and_then(notice_text);
    let error = query.error.as_deref().and_then(error_text);
    Html(page(&fam.header, &item, can_edit, notice, error)).into_response()
}

fn page(
    header: &str,
    item: &StockItemResponse,
    can_edit: bool,
    notice: Option<&str>,
    error: Option<&str>,
) -> String {
    let id = item.id;
    let notice_html = notice
        .map(|n| format!(r#"<p class="notice success">{}</p>"#, html_escape(n)))
        .unwrap_or_default();
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();

    let low = is_low_stock(item.quantity, item.reorder_threshold);
    let low_html = if low {
        r#"<p><span class="badge warn">Stock bas</span></p>"#.to_string()
    } else {
        String::new()
    };
    let category_html = item
        .category
        .as_deref()
        .filter(|c| !c.is_empty())
        .map(|c| format!("<p><strong>Catégorie :</strong> {}</p>", html_escape(c)))
        .unwrap_or_default();
    let threshold_html = match item.reorder_threshold {
        Some(t) => format!(
            "<p><strong>Seuil de réappro :</strong> {}</p>",
            html_escape(&format_quantity(t, &item.unit)),
        ),
        None => r#"<p class="muted">Aucun seuil de réappro défini.</p>"#.to_string(),
    };
    let expiry_html = match item.expires_on {
        Some(d) => {
            let badge = expiry_badge(item.expiry_status)
                .map(|(class, text)| format!(r#" <span class="{class}">{text}</span>"#))
                .unwrap_or_default();
            format!(
                "<p><strong>Péremption :</strong> {}{badge}</p>",
                fmt_date(d)
            )
        }
        None => r#"<p class="muted">Aucune date de péremption.</p>"#.to_string(),
    };

    // Quantity adjustment is open to any family member (shared inventory);
    // the full-record edit and delete stay behind the `can_edit` bar.
    let adjust_form = format!(
        r#"<form method="post" action="/stocks/{id}/adjust" class="card inline">
<label>Ajuster la quantité
<input type="number" name="quantity" step="any" min="0" value="{qty}"/></label>
<button type="submit">Mettre à jour la quantité</button>
</form>"#,
        qty = html_escape(&fmt_num(item.quantity)),
    );
    let edit_delete_html = if can_edit {
        format!(
            r#"<div class="actions">
<a class="btn secondary" href="/stocks/{id}/edit">Modifier l'article</a>
<form method="post" action="/stocks/{id}/delete">
<button type="submit" class="secondary danger">Supprimer</button>
</form>
</div>"#
        )
    } else {
        r#"<p class="muted">Seul le créateur ou un administrateur peut modifier ou supprimer cet article.</p>"#.to_string()
    };
    let controls_html = format!("{adjust_form}{edit_delete_html}");

    let body = format!(
        r#"<h1>{name}</h1>
{notice_html}{error_html}
<p><strong>Quantité :</strong> {qty}</p>
{low_html}
{category_html}
{threshold_html}
{expiry_html}
{controls_html}
<div class="links"><a href="/stocks">Retour aux stocks</a></div>"#,
        name = html_escape(&item.name),
        qty = html_escape(&format_quantity(item.quantity, &item.unit)),
    );
    shell_with_header(Width::Read, &item.name, header, &body)
}

// -- mutations --------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct AdjustForm {
    quantity: String,
    /// Where the stocks list's stepper came from (#527): the list's own
    /// address, filters included. Absent from the detail page's form.
    #[serde(default)]
    back: String,
}

/// Where an adjustment lands once saved: `back` with the notice, when it is
/// the stocks list (`/stocks`, bare or with a query) — anything else is
/// refused, so the field cannot send the member off the site — and the
/// article's page otherwise.
pub(crate) fn adjusted_target(back: &str, item_id: Uuid) -> String {
    // `/stocks` then nothing, or its query: no path can follow, so no
    // other page — let alone another site — is reachable through it. The
    // query is the one `list::list_href` writes, percent-encoded: visible
    // ASCII only, which also keeps the `Location` header valid.
    match back.strip_prefix("/stocks") {
        Some("") => "/stocks?notice=quantity_adjusted".to_string(),
        Some(query)
            if query.starts_with('?')
                && !query.contains('#')
                && query.chars().all(|c| c.is_ascii_graphic()) =>
        {
            format!("/stocks{query}&notice=quantity_adjusted")
        }
        _ => format!("/stocks/{item_id}?notice=quantity_adjusted"),
    }
}

/// The quantity the adjust form posts, or the `?error=` code it lands on.
/// The API's guards (#649): a stepper at the bound, or a forged value above
/// it or not finite, reads `quantity_too_large`; a negative or unreadable
/// value, which the stepper never posts, stays `unavailable`.
fn adjust_guard(typed: &str) -> Result<f64, &'static str> {
    let quantity = parse_quantity(typed).ok_or("unavailable")?;
    match check_quantities(quantity, None) {
        Ok(()) => Ok(quantity),
        Err(ItemFormError::QuantityTooLarge) => Err("quantity_too_large"),
        Err(_) => Err("unavailable"),
    }
}

pub async fn adjust(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(item_id): Path<Uuid>,
    Form(form): Form<AdjustForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, &format!("/stocks/{item_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let detail = format!("/stocks/{item_id}");

    let quantity = match adjust_guard(&form.quantity) {
        Ok(quantity) => quantity,
        Err(code) => return Redirect::to(&format!("{detail}?error={code}")).into_response(),
    };

    let body = UpdateStockItemRequest {
        quantity: Some(quantity),
        ..Default::default()
    };
    let cookie = stocks_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::PATCH,
        &format!("/groups/{}/stock-items/{}", fam.gid, item_id),
        cookie.as_deref(),
        Some(serde_json::to_value(&body).unwrap()),
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => adjusted_target(&form.back, item_id),
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{detail}?error=forbidden")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return item_not_found_page().into_response()
        }
        Ok(_) | Err(_) => format!("{detail}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}

pub async fn delete(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(item_id): Path<Uuid>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, &format!("/stocks/{item_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = stocks_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &format!("/groups/{}/stock-items/{}", fam.gid, item_id),
        cookie.as_deref(),
        None,
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            Redirect::to("/stocks?notice=item_deleted").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            forbidden_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            item_not_found_page().into_response()
        }
        Ok(_) | Err(_) => service_unavailable_page().into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> Uuid {
        Uuid::from_u128(7)
    }

    // -- adjust_guard (#649) -------------------------------------------------

    #[test]
    fn a_quantity_within_the_bound_is_sent() {
        for typed in ["0", "3", "1.5", "1000000000", ""] {
            assert_eq!(adjust_guard(typed), Ok(parse_quantity(typed).unwrap()), "{typed}");
        }
    }

    #[test]
    fn a_quantity_above_the_bound_or_not_finite_reads_its_own_error() {
        for typed in ["1000000001", "1e300", "nan", "inf"] {
            assert_eq!(adjust_guard(typed), Err("quantity_too_large"), "{typed}");
        }
    }

    #[test]
    fn a_negative_or_unreadable_quantity_stays_unavailable() {
        for typed in ["-1", "-inf", "abc"] {
            assert_eq!(adjust_guard(typed), Err("unavailable"), "{typed}");
        }
    }

    #[test]
    fn every_adjust_error_has_a_message() {
        for code in ["forbidden", "unavailable", "quantity_too_large"] {
            assert!(error_text(code).is_some(), "{code}");
        }
        assert_eq!(error_text("anything-else"), None);
    }

    fn item(quantity: f64, unit: &str, threshold: Option<f64>) -> StockItemResponse {
        StockItemResponse {
            id: id(),
            group_id: Uuid::nil(),
            created_by: Uuid::nil(),
            name: "Pâtes".to_string(),
            category: None,
            quantity,
            unit: unit.to_string(),
            reorder_threshold: threshold,
            low_stock: false,
            expires_on: None,
            expiry_status: manage_our_home_shared::validation::stocks::ExpiryStatus::Unknown,
            barcode: None,
        }
    }

    #[test]
    fn the_quantity_and_the_threshold_read_as_the_list_writes_them() {
        let html = page("", &item(6.0, "unité", Some(1.5)), false, None, None);
        assert!(html.contains("<strong>Quantité :</strong> 6</p>"), "{html}");
        assert!(
            html.contains("<strong>Seuil de réappro :</strong> 1,5</p>"),
            "{html}"
        );
        let html = page("", &item(2.0, "paquet", Some(1.0)), false, None, None);
        assert!(
            html.contains("<strong>Quantité :</strong> 2 paquets</p>"),
            "{html}"
        );
        assert!(
            html.contains("<strong>Seuil de réappro :</strong> 1 paquet</p>"),
            "{html}"
        );
    }

    #[test]
    fn the_list_is_where_its_stepper_lands_filters_kept() {
        assert_eq!(
            adjusted_target("/stocks", id()),
            "/stocks?notice=quantity_adjusted"
        );
        assert_eq!(
            adjusted_target("/stocks?lieu=Frigo&q=lait", id()),
            "/stocks?lieu=Frigo&q=lait&notice=quantity_adjusted"
        );
    }

    #[test]
    fn anything_but_the_list_lands_on_the_article() {
        let detail = "/stocks/00000000-0000-0000-0000-000000000007?notice=quantity_adjusted";
        for back in [
            "",
            "https://example.org/stocks",
            "//example.org",
            "/stocks/new",
            "/stocksx",
            "/stocks/../account",
            "/stocks#x",
            "/stocks?q=a#x",
            "/stocks?q=a\r\nSet-Cookie: x",
            "/stocks?lieu=Congélateur",
            "/budget",
        ] {
            assert_eq!(adjusted_target(back, id()), detail, "{back}");
        }
    }
}
