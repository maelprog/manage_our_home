//! `/stocks` — the active family's inventory, sorted by name (backend order),
//! with a low-stock badge derived on read. `?low_stock=1` filters to low-stock
//! items only (delegated to the backend's `?low_stock=true` param). PRG
//! banners after create/update/adjust/delete.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use leptos::prelude::*;
use manage_our_home_shared::dto::stocks::{StockItemList, StockItemResponse};
use manage_our_home_shared::validation::stocks::is_low_stock;

use crate::app::shell;
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{family_context, fmt_num, service_unavailable_page, stocks_cookie};

#[derive(serde::Deserialize)]
pub struct ListQuery {
    low_stock: Option<String>,
    notice: Option<String>,
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
    let path = if only_low {
        format!("/groups/{}/stock-items?low_stock=true", fam.gid)
    } else {
        format!("/groups/{}/stock-items", fam.gid)
    };

    let cookie = stocks_cookie(&headers);
    let items: Vec<StockItemResponse> = match api_request_auth(
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

    let notice = query.notice.as_deref().and_then(notice_text);

    let rows = items
        .iter()
        .map(|item| {
            let href = format!("/stocks/{}", item.id);
            let low = is_low_stock(item.quantity, item.reorder_threshold);
            let qty = format!("{} {}", fmt_num(item.quantity), item.unit);
            let category = item.category.clone().filter(|c| !c.is_empty());
            view! {
                <li style="display:flex;justify-content:space-between;align-items:center;gap:0.75rem;padding:0.6rem 0;border-bottom:1px solid var(--border);">
                    <span>
                        <a href=href><strong>{item.name.clone()}</strong></a>
                        {category.map(|c| view! { " " <span class="muted">"· "{c}</span> })}
                        {low.then(|| view! {
                            " "
                            <span style="font-size:0.8rem;padding:0.1rem 0.4rem;border-radius:3px;background:var(--error);color:#fff;">"Stock bas"</span>
                        })}
                    </span>
                    <span class="muted">{qty}</span>
                </li>
            }
        })
        .collect::<Vec<_>>();

    let (filter_link, filter_label) = if only_low {
        ("/stocks", "Voir tout le stock")
    } else {
        ("/stocks?low_stock=1", "Voir uniquement le stock bas")
    };

    let empty_text = if only_low {
        "Aucun article en stock bas."
    } else {
        "Aucun article pour le moment."
    };

    let body = view! {
        <div inner_html=fam.header></div>
        <div style="display:flex;justify-content:space-between;align-items:center;gap:0.75rem;flex-wrap:wrap;">
            <h1 style="margin:0;">"Stocks"</h1>
            <span style="display:flex;gap:0.5rem;align-items:center;flex-wrap:wrap;">
                <a class="button secondary" href=filter_link>{filter_label}</a>
                <a class="button" href="/stocks/new">"Nouvel article"</a>
            </span>
        </div>
        {notice.map(|n| view! { <p class="notice success">{n}</p> })}
        {if items.is_empty() {
            Some(view! { <p class="muted">{empty_text}</p> })
        } else {
            None
        }}
        <ul style="list-style:none;padding:0;margin:1rem 0 0 0;">{rows}</ul>
    };
    Html(shell("Stocks", &body.to_html())).into_response()
}
