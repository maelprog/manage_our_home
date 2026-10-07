//! `/stocks` — the active family's inventory, sorted by name (backend order),
//! with a low-stock badge derived on read. `?low_stock=1` filters to low-stock
//! items only (delegated to the backend's `?low_stock=true` param).
//! `?order=expiry` re-sorts the page "à consommer en premier" — soonest expiry
//! date first, undated items last (#401); the two parameters combine. Each row
//! carries its expiry date and, when expired or close, a worded badge. PRG
//! banners after create/update/adjust/delete.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use leptos::prelude::*;
use manage_our_home_shared::dto::stocks::{StockItemList, StockItemResponse};
use manage_our_home_shared::validation::stocks::{
    expiry_status, is_low_stock, sort_soonest_expiry_first,
};

use crate::app::{shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::routes::agenda::today_paris;
use crate::state::{api_request_auth, AppState};

use super::{
    expiry_badge, family_context, fmt_date, fmt_num, service_unavailable_page, stocks_cookie,
};

#[derive(serde::Deserialize)]
pub struct ListQuery {
    low_stock: Option<String>,
    order: Option<String>,
    notice: Option<String>,
}

/// `/stocks` with the given filter and order, so each toggle keeps the other.
fn list_href(only_low: bool, by_expiry: bool) -> &'static str {
    match (only_low, by_expiry) {
        (false, false) => "/stocks",
        (true, false) => "/stocks?low_stock=1",
        (false, true) => "/stocks?order=expiry",
        (true, true) => "/stocks?low_stock=1&order=expiry",
    }
}

fn notice_text(code: &str) -> Option<&'static str> {
    match code {
        "item_created" => Some("Article créé."),
        "item_updated" => Some("Article mis à jour."),
        "quantity_adjusted" => Some("Quantité mise à jour."),
        "item_deleted" => Some("Article supprimé."),
        _ => None,
    }
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/stocks").await else {
        return Redirect::to("/groups/new").into_response();
    };

    let only_low = matches!(query.low_stock.as_deref(), Some("1") | Some("true"));
    let by_expiry = query.order.as_deref() == Some("expiry");
    let path = if only_low {
        format!("/groups/{}/stock-items?low_stock=true", fam.gid)
    } else {
        format!("/groups/{}/stock-items", fam.gid)
    };

    let cookie = stocks_cookie(&headers);
    let mut items: Vec<StockItemResponse> = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &path,
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<StockItemList>(resp.body)
                .map(|l| l.items)
                .unwrap_or_default()
        }
        // A non-member (403, unreachable once family is resolved) or any
        // other status: render an empty list rather than leaking JSON.
        Ok(_) => Vec::new(),
        Err(_) => return service_unavailable_page().into_response(),
    };

    if by_expiry {
        sort_soonest_expiry_first(&mut items, |item| item.expires_on);
    }

    let notice = query.notice.as_deref().and_then(notice_text);
    let today = today_paris();

    let rows = items
        .iter()
        .map(|item| {
            let href = format!("/stocks/{}", item.id);
            let low = is_low_stock(item.quantity, item.reorder_threshold);
            let qty = match item.expires_on {
                Some(d) => format!(
                    "{} {} · péremption le {}",
                    fmt_num(item.quantity),
                    item.unit,
                    fmt_date(d)
                ),
                None => format!("{} {}", fmt_num(item.quantity), item.unit),
            };
            let expiry = expiry_badge(expiry_status(item.expires_on, today));
            let category = item.category.clone().filter(|c| !c.is_empty());
            view! {
                <li class="list-row">
                    <span>
                        <a href=href><strong>{item.name.clone()}</strong></a>
                        {category.map(|c| view! { " " <span class="muted">"· "{c}</span> })}
                        {low.then(|| view! {
                            " "
                            <span class="badge warn">"Stock bas"</span>
                        })}
                        {expiry.map(|(class, text)| view! {
                            " "
                            <span class=class>{text}</span>
                        })}
                    </span>
                    <span class="muted">{qty}</span>
                </li>
            }
        })
        .collect::<Vec<_>>();

    let filter_link = list_href(!only_low, by_expiry);
    let filter_label = if only_low {
        "Voir tout le stock"
    } else {
        "Voir uniquement le stock bas"
    };
    let order_link = list_href(only_low, !by_expiry);
    let order_label = if by_expiry {
        "Trier par nom"
    } else {
        "À consommer en premier"
    };

    let empty_text = if only_low {
        "Aucun article en stock bas."
    } else {
        "Aucun article pour le moment."
    };

    let body = view! {
        <div class="page-header">
            <h1>"Stocks"</h1>
            <span class="actions">
                <a class="btn secondary" href=filter_link>{filter_label}</a>
                <a class="btn secondary" href=order_link>{order_label}</a>
                <a class="btn" href="/stocks/new">"Nouvel article"</a>
            </span>
        </div>
        {notice.map(|n| view! { <p class="notice success">{n}</p> })}
        {if items.is_empty() {
            Some(view! { <p class="muted">{empty_text}</p> })
        } else {
            None
        }}
        <ul class="list">{rows}</ul>
    };
    Html(shell_with_header(
        Width::Full,
        "Stocks",
        &fam.header,
        &body.to_html(),
    ))
    .into_response()
}
