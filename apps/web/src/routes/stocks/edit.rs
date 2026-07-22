//! `/stocks/:id/edit` — full edit of a stock item (name/category/quantity/
//! unit/threshold). Same permission bar as delete (`can_modify`): a
//! non-permitted user never sees the form (GET → forbidden page), and the
//! backend 403 is mapped to `?error=forbidden` on the detail page defensively.
//! A blank category or threshold *clears* it (sent as `Some(None)` per the
//! backend's double-`Option` PATCH contract). Success (200) → PRG
//! `/stocks/:id?notice=item_updated`.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::stocks::{StockItemResponse, UpdateStockItemRequest};
use manage_our_home_shared::validation::stocks::validate_item_form;
use uuid::Uuid;

use crate::app::{html_escape, shell};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::new::{
    error_message, form_error_code, form_fields, parse_quantity, parse_threshold, ItemForm,
};
use super::{
    can_modify, family_context, forbidden_page, item_not_found_page, service_unavailable_page,
    stocks_cookie,
};

fn page(header: &str, id: Uuid, name: &str, form: &ItemForm, error: Option<&str>) -> String {
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let fields = form_fields(
        &form.name,
        &form.category,
        &form.quantity,
        &form.unit,
        &form.reorder_threshold,
    );
    let body = format!(
        r#"{header}
<h1>Modifier — {name_esc}</h1>
{error_html}
<form method="post" action="/stocks/{id}/edit">
{fields}
<button type="submit">Enregistrer</button>
</form>
<div class="links"><a href="/stocks/{id}">Retour au détail</a></div>"#,
        name_esc = html_escape(name),
    );
    shell("Modifier l'article", &body)
}

/// Fetches an item, returning it or an early `Response` (404/unavailable).
async fn fetch_item(
    state: &AppState,
    gid: Uuid,
    item_id: Uuid,
    cookie: Option<&str>,
) -> Result<StockItemResponse, Response> {
    match api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/stock-items/{item_id}"),
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<StockItemResponse>(resp.body)
                .map_err(|_| service_unavailable_page().into_response())
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            Err(item_not_found_page().into_response())
        }
        _ => Err(service_unavailable_page().into_response()),
    }
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(item_id): Path<Uuid>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, &format!("/stocks/{item_id}/edit")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = stocks_cookie(&headers);
    let item = match fetch_item(&state, fam.gid, item_id, cookie.as_deref()).await {
        Ok(item) => item,
        Err(resp) => return resp,
    };

    if !can_modify(&fam.role, item.created_by == me.user_id) {
        return forbidden_page().into_response();
    }

    let form = ItemForm {
        name: item.name.clone(),
        category: item.category.clone().unwrap_or_default(),
        quantity: super::fmt_num(item.quantity),
        unit: item.unit.clone(),
        reorder_threshold: item
            .reorder_threshold
            .map(super::fmt_num)
            .unwrap_or_default(),
    };
    Html(page(&fam.header, item_id, &item.name, &form, None)).into_response()
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(item_id): Path<Uuid>,
    Form(form): Form<ItemForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, &format!("/stocks/{item_id}/edit")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };

    let render_error = |code: &str| {
        Html(page(
            &fam.header,
            item_id,
            &form.name,
            &form,
            Some(error_message(code)),
        ))
        .into_response()
    };

    let Some(quantity) = parse_quantity(&form.quantity) else {
        return render_error("quantity_must_be_non_negative");
    };
    let Ok(reorder_threshold) = parse_threshold(&form.reorder_threshold) else {
        return render_error("reorder_threshold_must_be_non_negative");
    };
    if let Err(e) = validate_item_form(&form.name, &form.unit, quantity, reorder_threshold) {
        return render_error(form_error_code(e));
    }

    // A blank category clears it (`Some(None)`); the edit form always sends
    // every field, so each is `Some(...)` (never "leave untouched").
    let category = {
        let c = form.category.trim();
        Some((!c.is_empty()).then(|| c.to_string()))
    };
    let body = UpdateStockItemRequest {
        name: Some(form.name.trim().to_string()),
        category,
        quantity: Some(quantity),
        unit: Some(form.unit.trim().to_string()),
        reorder_threshold: Some(reorder_threshold),
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

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            Redirect::to(&format!("/stocks/{item_id}?notice=item_updated")).into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            Redirect::to(&format!("/stocks/{item_id}?error=forbidden")).into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            item_not_found_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            let code = resp
                .body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unavailable");
            render_error(code)
        }
        Ok(_) | Err(_) => render_error("unavailable"),
    }
}
