//! `/stocks/:id` — item detail with the low-stock state, plus the
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
use manage_our_home_shared::validation::stocks::is_low_stock;
use uuid::Uuid;

use crate::app::{html_escape, shell};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::new::parse_quantity;
use super::{
    can_modify, family_context, fmt_num, forbidden_page, item_not_found_page,
    service_unavailable_page, stocks_cookie,
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
        r#"<p><span style="font-size:0.85rem;padding:0.1rem 0.5rem;border-radius:3px;background:var(--error);color:#fff;">Stock bas</span></p>"#.to_string()
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
            "<p><strong>Seuil de réappro :</strong> {} {}</p>",
            html_escape(&fmt_num(t)),
            html_escape(&item.unit),
        ),
        None => r#"<p class="muted">Aucun seuil de réappro défini.</p>"#.to_string(),
    };

    // Quantity adjustment is open to any family member (shared inventory);
    // the full-record edit and delete stay behind the `can_edit` bar.
    let adjust_form = format!(
        r#"<form method="post" action="/stocks/{id}/adjust" style="border:1px solid var(--border);padding:0.75rem;margin-top:1rem;display:flex;gap:0.5rem;align-items:flex-end;flex-wrap:wrap;">
<label style="margin:0;">Ajuster la quantité
<input type="number" name="quantity" step="any" min="0" value="{qty}"/></label>
<button type="submit">Mettre à jour la quantité</button>
</form>"#,
        qty = html_escape(&fmt_num(item.quantity)),
    );
    let edit_delete_html = if can_edit {
        format!(
            r#"<div style="display:flex;gap:0.5rem;margin-top:1rem;">
<a class="button secondary" href="/stocks/{id}/edit">Modifier l'article</a>
<form method="post" action="/stocks/{id}/delete" style="margin:0;">
<button type="submit" class="secondary" style="color:var(--error);">Supprimer</button>
</form>
</div>"#
        )
    } else {
        r#"<p class="muted" style="margin-top:1rem;">Seul le créateur ou un administrateur peut modifier ou supprimer cet article.</p>"#.to_string()
    };
    let controls_html = format!("{adjust_form}{edit_delete_html}");

    let body = format!(
        r#"{header}
<h1>{name}</h1>
{notice_html}{error_html}
<p><strong>Quantité :</strong> {qty} {unit}</p>
{low_html}
{category_html}
{threshold_html}
{controls_html}
<div class="links"><a href="/stocks">Retour aux stocks</a></div>"#,
        name = html_escape(&item.name),
        qty = html_escape(&fmt_num(item.quantity)),
        unit = html_escape(&item.unit),
    );
    shell(&item.name, &body)
}

// -- mutations --------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct AdjustForm {
    quantity: String,
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

    let Some(quantity) = parse_quantity(&form.quantity) else {
        return Redirect::to(&format!("{detail}?error=unavailable")).into_response();
    };
    if quantity < 0.0 {
        return Redirect::to(&format!("{detail}?error=unavailable")).into_response();
    }

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
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            format!("{detail}?notice=quantity_adjusted")
        }
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
