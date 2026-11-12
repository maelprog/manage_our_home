//! `/stocks/:id` — item detail with the low-stock and expiry states, plus the
//! quantity-adjust action and the delete action; full edit lives in `edit.rs`.
//! Under « Utilisé dans », the planned recipes that take from the article
//! and their total against the stock (#712).
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
    item_not_found_page, planned_total, service_unavailable_page, stocks_cookie,
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
        "quantity_too_large" => Some(super::QUANTITY_TOO_LARGE_MESSAGE),
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
    let planned_html = planned_html(item);

    let body = format!(
        r#"<h1>{name}</h1>
{notice_html}{error_html}
<p><strong>Quantité :</strong> {qty}</p>
{low_html}
{category_html}
{threshold_html}
{expiry_html}
{planned_html}
{controls_html}
<div class="links"><a href="/stocks">Retour aux stocks</a></div>"#,
        name = html_escape(&item.name),
        qty = html_escape(&format_quantity(item.quantity, &item.unit)),
    );
    shell_with_header(Width::Read, &item.name, header, &body)
}

/// « Utilisé dans » (#712): each planned recipe that takes from the article,
/// linked, with what it would take (`planned_usage`, in the article's unit),
/// then their total against the stock — and, when the stock falls short,
/// what is missing, in words on a `.badge.warn`. Nothing when no planned
/// recipe uses the article.
fn planned_html(item: &StockItemResponse) -> String {
    let usage = item.planned_usage.as_deref().unwrap_or_default();
    let Some(total) = planned_total(usage, item.quantity, &item.unit) else {
        return String::new();
    };
    let rows: String = usage
        .iter()
        .map(|u| {
            format!(
                r#"<li class="list-row"><a href="/recipes/{id}">{name}</a><span>{qty}{optional}</span></li>"#,
                id = u.recipe_id,
                name = html_escape(&u.recipe_name),
                qty = html_escape(&format_quantity(u.quantity, &item.unit)),
                optional = if u.is_optional {
                    r#" <span class="muted">(facultatif)</span>"#
                } else {
                    ""
                },
            )
        })
        .collect();
    let short = total
        .short_by
        .map(|s| {
            format!(
                r#" <span class="badge warn">Il manque {}</span>"#,
                html_escape(&s)
            )
        })
        .unwrap_or_default();
    format!(
        r#"<h2>Utilisé dans</h2>
<ul class="list">{rows}</ul>
<p><strong>Total prévu :</strong> {total} pour {stock} en stock{short}</p>"#,
        total = html_escape(&total.total),
        stock = html_escape(&total.stock),
    )
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
/// article's page otherwise. An article the adjustment `removed` (#800) has
/// no page left: it lands on the bare list instead, with its own notice.
pub(crate) fn adjusted_target(back: &str, item_id: Uuid, removed: bool) -> String {
    let notice = if removed {
        "item_depleted"
    } else {
        "quantity_adjusted"
    };
    // `/stocks` then nothing, or its query: no path can follow, so no
    // other page — let alone another site — is reachable through it. The
    // query is the one `list::list_href` writes, percent-encoded: visible
    // ASCII only, which also keeps the `Location` header valid.
    match back.strip_prefix("/stocks") {
        Some("") => format!("/stocks?notice={notice}"),
        Some(query)
            if query.starts_with('?')
                && !query.contains('#')
                && query.chars().all(|c| c.is_ascii_graphic()) =>
        {
            format!("/stocks{query}&notice={notice}")
        }
        _ if removed => format!("/stocks?notice={notice}"),
        _ => format!("/stocks/{item_id}?notice={notice}"),
    }
}

/// Whether the PATCH answer says the adjustment removed the article (#800).
fn removed_by_adjust(body: &serde_json::Value) -> bool {
    body.get("removed").and_then(serde_json::Value::as_bool) == Some(true)
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
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            adjusted_target(&form.back, item_id, removed_by_adjust(&resp.body))
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
            assert_eq!(
                adjust_guard(typed),
                Ok(parse_quantity(typed).unwrap()),
                "{typed}"
            );
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
            planned_usage: None,
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

    // -- « Utilisé dans » (#712) ---------------------------------------------

    use crate::routes::stocks::tests::usage;

    #[test]
    fn no_planned_recipe_shows_no_section() {
        let mut pates = item(2.0, "paquet", None);
        for planned in [None, Some(vec![])] {
            pates.planned_usage = planned;
            let html = page("", &pates, false, None, None);
            assert!(!html.contains("Utilisé dans"), "{html}");
            assert!(!html.contains("Total prévu"), "{html}");
        }
    }

    #[test]
    fn one_planned_recipe_is_linked_with_its_quantity_and_the_total() {
        let mut pates = item(3.0, "paquet", None);
        pates.planned_usage = Some(vec![usage(1, "Pâtes <bolo>", 1.0, "paquet")]);
        let html = page("", &pates, false, None, None);
        assert!(html.contains("<h2>Utilisé dans</h2>"), "{html}");
        assert!(
            html.contains(
                r#"<ul class="list"><li class="list-row"><a href="/recipes/00000000-0000-0000-0000-000000000001">Pâtes &lt;bolo&gt;</a><span>1 paquet</span></li></ul>"#
            ),
            "{html}"
        );
        assert!(
            html.contains("<p><strong>Total prévu :</strong> 1 paquet pour 3 paquets en stock</p>"),
            "{html}"
        );
        assert!(!html.contains("Il manque"), "{html}");
    }

    #[test]
    fn several_planned_recipes_are_listed_in_order_an_optional_one_said_so() {
        let mut pates = item(4.0, "paquet", None);
        let mut salade = usage(2, "Salade", 0.5, "paquet");
        salade.is_optional = true;
        pates.planned_usage = Some(vec![usage(1, "Bolo", 2.0, "paquet"), salade]);
        let html = page("", &pates, false, None, None);
        assert_eq!(
            html.matches(r#"<li class="list-row">"#).count(),
            2,
            "{html}"
        );
        let bolo = html.find(">Bolo</a><span>2 paquets</span>").expect(&html);
        let salade = html
            .find(r#">Salade</a><span>0,5 paquet <span class="muted">(facultatif)</span></span>"#)
            .expect(&html);
        assert!(bolo < salade, "{html}");
        assert!(
            html.contains("<strong>Total prévu :</strong> 2,5 paquets pour 4 paquets en stock</p>"),
            "{html}"
        );
    }

    #[test]
    fn a_total_above_the_stock_is_worded_not_only_coloured() {
        let mut pates = item(2.0, "paquet", None);
        pates.planned_usage = Some(vec![
            usage(1, "Bolo", 2.0, "paquet"),
            usage(2, "Gratin", 1.0, "paquet"),
        ]);
        let html = page("", &pates, false, None, None);
        assert!(
            html.contains(
                r#"<p><strong>Total prévu :</strong> 3 paquets pour 2 paquets en stock <span class="badge warn">Il manque 1 paquet</span></p>"#
            ),
            "{html}"
        );
    }

    #[test]
    fn the_list_is_where_its_stepper_lands_filters_kept() {
        assert_eq!(
            adjusted_target("/stocks", id(), false),
            "/stocks?notice=quantity_adjusted"
        );
        assert_eq!(
            adjusted_target("/stocks?lieu=Frigo&q=lait", id(), false),
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
            assert_eq!(adjusted_target(back, id(), false), detail, "{back}");
        }
    }

    /// #800: an article the adjustment emptied is gone, so the way back is
    /// the list — its filters kept when the stepper came from it — with its
    /// own notice, never the article's page, which would be a 404.
    #[test]
    fn an_emptied_article_lands_on_the_list_with_its_notice() {
        assert_eq!(
            adjusted_target("/stocks", id(), true),
            "/stocks?notice=item_depleted"
        );
        assert_eq!(
            adjusted_target("/stocks?lieu=Frigo&q=lait", id(), true),
            "/stocks?lieu=Frigo&q=lait&notice=item_depleted"
        );
        for back in ["", "https://example.org/stocks", "/stocks/new", "/stocks#x"] {
            assert_eq!(
                adjusted_target(back, id(), true),
                "/stocks?notice=item_depleted",
                "{back}"
            );
        }
    }

    #[test]
    fn the_api_tells_whether_the_adjustment_removed_the_article() {
        assert!(removed_by_adjust(&serde_json::json!({"removed": true})));
        assert!(!removed_by_adjust(&serde_json::json!({"quantity": 0.0})));
        assert!(!removed_by_adjust(&serde_json::json!({"removed": false})));
        assert!(!removed_by_adjust(&serde_json::Value::Null));
    }
}
