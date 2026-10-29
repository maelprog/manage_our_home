//! `/stocks` — the active family's inventory, laid out after the mock-ups
//! « Mobile — Cuisine — Stocks » and « PC — Stocks » (#527): a search field,
//! a row of filters, then one card per article — its name, where it is kept,
//! a worded expiry pastille, and a − / + stepper around the quantity and its
//! unit (`format_quantity`). On a phone the cards are rows; on a PC, a grid.
//!
//! Filters, all plain links or a GET form, so nothing needs JavaScript:
//! - `?lieu=` keeps one place — the article's free-text `category`, offered
//!   as a filter for every place the list holds (`places`) — and `?q=` the
//!   names that contain the words searched (`shown`). Both are applied here,
//!   on the list apps/api returned.
//! - `?low_stock=1` keeps the low-stock articles (the backend's
//!   `?low_stock=true`); `?sort=expires_on` lists « à consommer en premier »
//!   — soonest expiry first, undated last, then by name (#401), the
//!   backend's identical `sort`. Every link keeps the other parameters
//!   (`list_href`).
//!
//! The stepper posts the absolute quantity, one up or one down, to the
//! detail page's adjust route — the quantity-only PATCH any member may make
//! (#39) — with the list's own address in `back`, so the member lands back
//! here, on the same filters. PRG banners after create/update/adjust/delete.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::{Datelike, NaiveDate};
use manage_our_home_shared::dto::stocks::{StockItemList, StockItemResponse, SORT_BY_EXPIRY};
use manage_our_home_shared::validation::stocks::{article_key, format_quantity, is_low_stock};

use crate::app::{html_escape, kitchen_segment, shell_with_header, Width};
use crate::icons::{icon, Icon};
use crate::layout::CurrentUser;
use crate::routes::agenda::today_paris;
use crate::state::{api_request_auth, AppState};

use super::{family_context, fmt_date, fmt_num, service_unavailable_page, stocks_cookie};

#[derive(serde::Deserialize)]
pub struct ListQuery {
    low_stock: Option<String>,
    sort: Option<String>,
    lieu: Option<String>,
    q: Option<String>,
    notice: Option<String>,
}

/// What the list shows: the two backend parameters, and the place and the
/// search applied here. `place` and `search` are trimmed, `None` when blank.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ListState {
    pub only_low: bool,
    pub by_expiry: bool,
    pub place: Option<String>,
    pub search: Option<String>,
}

fn non_blank(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `/stocks` for `state`, so each filter keeps the others. The backend
/// parameters come first, in the order the list has always written them.
pub(crate) fn list_href(state: &ListState) -> String {
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    if state.only_low {
        pairs.push(("low_stock", "1"));
    }
    if state.by_expiry {
        pairs.push(("sort", SORT_BY_EXPIRY));
    }
    if let Some(place) = &state.place {
        pairs.push(("lieu", place));
    }
    if let Some(search) = &state.search {
        pairs.push(("q", search));
    }
    if pairs.is_empty() {
        "/stocks".to_string()
    } else {
        format!(
            "/stocks?{}",
            serde_urlencoded::to_string(&pairs).unwrap_or_default()
        )
    }
}

/// Every place the list holds, once each — compared without case or
/// surrounding spaces, written as first met — in alphabetical order.
pub(crate) fn places(items: &[StockItemResponse]) -> Vec<String> {
    let mut found: Vec<(String, String)> = Vec::new();
    for place in items
        .iter()
        .filter_map(|i| i.category.as_deref())
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        let key = article_key(place);
        if !found.iter().any(|(k, _)| *k == key) {
            found.push((key, place.to_string()));
        }
    }
    found.sort();
    found.into_iter().map(|(_, place)| place).collect()
}

/// Whether `item` is kept in `place` (same key) and its name contains
/// `search` (same key), each when given.
pub(crate) fn shown(item: &StockItemResponse, place: Option<&str>, search: Option<&str>) -> bool {
    let in_place =
        place.is_none_or(|p| item.category.as_deref().map(article_key) == Some(article_key(p)));
    let named = search.is_none_or(|q| article_key(&item.name).contains(&article_key(q)));
    in_place && named
}

const WEEKDAYS: [&str; 7] = [
    "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche",
];

/// The expiry pastille of a card, as `(class, text)` (#527). Red (`late`)
/// from the day before: expired, today, tomorrow; amber (`soon`) for the
/// rest of the week — two to six days out, the home tile's week — named by
/// its weekday; a later date in the neutral badge. The words carry the
/// state, the tint only doubles it (DESIGN.md — colour is never the sole
/// signal).
pub(crate) fn expiry_pill(
    expires_on: Option<NaiveDate>,
    today: NaiveDate,
) -> Option<(&'static str, String)> {
    let date = expires_on?;
    let pill = match (date - today).num_days() {
        ..=-1 => ("badge late", "périmé".to_string()),
        0 => ("badge late", "périme aujourd'hui".to_string()),
        1 => ("badge late", "périme demain".to_string()),
        2..=6 => (
            "badge soon",
            format!(
                "périme {}",
                WEEKDAYS[date.weekday().num_days_from_monday() as usize]
            ),
        ),
        _ => ("badge", format!("jusqu'au {}", fmt_date(date))),
    };
    Some(pill)
}

/// The − / + stepper of a card: two one-button forms posting the next
/// quantity to the adjust route, around the quantity and its unit. Minus
/// stops at 0, where it is disabled. `back` is the list's own address.
pub(crate) fn stepper(item: &StockItemResponse, back: &str) -> String {
    let form = |quantity: f64, label: &str, glyph: Icon, disabled: bool| {
        format!(
            r#"<form method="post" action="/stocks/{id}/adjust" data-keep-scroll><input type="hidden" name="quantity" value="{quantity}"/><input type="hidden" name="back" value="{back}"/><button type="submit" class="secondary" aria-label="{label} {name}"{disabled}>{svg}</button></form>"#,
            id = item.id,
            quantity = fmt_num(quantity),
            back = html_escape(back),
            name = html_escape(&item.name),
            disabled = if disabled { " disabled" } else { "" },
            svg = icon(glyph),
        )
    };
    format!(
        r#"<span class="stepper">{minus}<span>{qty}</span>{plus}</span>"#,
        minus = form(
            (item.quantity - 1.0).max(0.0),
            "Retirer 1 à",
            Icon::Minus,
            item.quantity <= 0.0
        ),
        qty = html_escape(&format_quantity(item.quantity, &item.unit)),
        plus = form(item.quantity + 1.0, "Ajouter 1 à", Icon::Plus, false),
    )
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

/// One article's card.
fn card(item: &StockItemResponse, today: NaiveDate, back: &str) -> String {
    let place = item
        .category
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(|c| format!("{} ", html_escape(c)))
        .unwrap_or_default();
    let pill = expiry_pill(item.expires_on, today)
        .map(|(class, text)| format!(r#"<span class="{class}">{}</span>"#, html_escape(&text)))
        .unwrap_or_default();
    let low = if is_low_stock(item.quantity, item.reorder_threshold) {
        r#" <span class="badge warn">Stock bas</span>"#
    } else {
        ""
    };
    format!(
        r#"<li class="card"><span class="what"><a href="/stocks/{id}"><strong>{name}</strong></a><span class="muted">{place}{pill}{low}</span></span>{stepper}</li>"#,
        id = item.id,
        name = html_escape(&item.name),
        stepper = stepper(item, back),
    )
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

    let view = ListState {
        only_low: matches!(query.low_stock.as_deref(), Some("1") | Some("true")),
        by_expiry: query.sort.as_deref() == Some(SORT_BY_EXPIRY),
        place: non_blank(query.lieu.as_deref()),
        search: non_blank(query.q.as_deref()),
    };
    let mut params = Vec::new();
    if view.only_low {
        params.push("low_stock=true".to_string());
    }
    if view.by_expiry {
        params.push(format!("sort={SORT_BY_EXPIRY}"));
    }
    let path = if params.is_empty() {
        format!("/groups/{}/stock-items", fam.gid)
    } else {
        format!("/groups/{}/stock-items?{}", fam.gid, params.join("&"))
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
    Html(shell_with_header(
        Width::Full,
        "Stocks",
        &fam.header,
        &format!(
            "{}{}",
            kitchen_segment("/stocks"),
            body(&view, &items, today_paris(), notice)
        ),
    ))
    .into_response()
}

/// The page under the header, from what apps/api returned.
fn body(
    view: &ListState,
    items: &[StockItemResponse],
    today: NaiveDate,
    notice: Option<&str>,
) -> String {
    let here = list_href(view);
    let link = |label: &str, state: ListState, current: bool| {
        format!(
            r#"<a href="{href}"{current}>{label}</a>"#,
            href = html_escape(&list_href(&state)),
            current = if current {
                r#" aria-current="true""#
            } else {
                ""
            },
            label = html_escape(label),
        )
    };
    let mut filters = vec![link(
        "Tout",
        ListState {
            place: None,
            ..view.clone()
        },
        view.place.is_none(),
    )];
    let current_place = view.place.as_deref().map(article_key);
    for place in places(items) {
        let current = current_place.as_deref() == Some(article_key(&place).as_str());
        filters.push(link(
            &place,
            ListState {
                place: Some(place.clone()),
                ..view.clone()
            },
            current,
        ));
    }
    filters.push(link(
        "Stock bas",
        ListState {
            only_low: !view.only_low,
            ..view.clone()
        },
        view.only_low,
    ));
    filters.push(link(
        if view.by_expiry {
            "Trier par nom"
        } else {
            "À consommer en premier"
        },
        ListState {
            by_expiry: !view.by_expiry,
            ..view.clone()
        },
        false,
    ));

    // The search keeps the other filters through hidden fields: a GET form
    // sends only its own.
    let mut kept = String::new();
    if view.only_low {
        kept.push_str(r#"<input type="hidden" name="low_stock" value="1"/>"#);
    }
    if view.by_expiry {
        kept.push_str(r#"<input type="hidden" name="sort" value="expires_on"/>"#);
    }
    if let Some(place) = &view.place {
        kept.push_str(&format!(
            r#"<input type="hidden" name="lieu" value="{}"/>"#,
            html_escape(place)
        ));
    }

    let cards: Vec<String> = items
        .iter()
        .filter(|i| shown(i, view.place.as_deref(), view.search.as_deref()))
        .map(|i| card(i, today, &here))
        .collect();
    let empty = if !cards.is_empty() {
        String::new()
    } else if !items.is_empty() {
        r#"<p class="muted">Aucun article ne correspond.</p>"#.to_string()
    } else if view.only_low {
        r#"<p class="muted">Aucun article en stock bas.</p>"#.to_string()
    } else {
        r#"<p class="muted">Aucun article pour le moment.</p>"#.to_string()
    };

    format!(
        r#"<div class="page-header"><h1>Stocks</h1><span class="actions"><a class="btn secondary pc" href="/stocks/new">{scan}Scanner un code-barres</a><a class="btn" href="/stocks/new">{plus}Ajouter un produit</a></span></div>
{notice}<form method="get" action="/stocks" role="search" class="search"><label><span class="sr-only">Chercher un produit</span>{search_icon}<input type="search" name="q" value="{q}" placeholder="Chercher un produit…"/></label>{kept}</form>
<nav class="filters" aria-label="Filtres">{filters}</nav>
{empty}<ul class="stock">{cards}</ul>"#,
        scan = icon(Icon::Scan),
        plus = icon(Icon::Plus),
        search_icon = icon(Icon::Search),
        notice = notice
            .map(|n| format!(r#"<p class="notice success">{}</p>"#, html_escape(n)))
            .unwrap_or_default(),
        q = html_escape(view.search.as_deref().unwrap_or("")),
        filters = filters.concat(),
        cards = cards.concat(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use manage_our_home_shared::validation::stocks::ExpiryStatus;
    use uuid::Uuid;

    fn item(name: &str, category: Option<&str>, quantity: f64, unit: &str) -> StockItemResponse {
        StockItemResponse {
            id: Uuid::from_u128(7),
            group_id: Uuid::nil(),
            created_by: Uuid::nil(),
            name: name.to_string(),
            category: category.map(str::to_string),
            quantity,
            unit: unit.to_string(),
            reorder_threshold: None,
            low_stock: false,
            expires_on: None,
            expiry_status: ExpiryStatus::Unknown,
            barcode: None,
        }
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    // -- list_href --------------------------------------------------------

    #[test]
    fn the_backend_parameters_keep_the_addresses_they_always_had() {
        let s = |only_low, by_expiry| ListState {
            only_low,
            by_expiry,
            ..Default::default()
        };
        assert_eq!(list_href(&s(false, false)), "/stocks");
        assert_eq!(list_href(&s(true, false)), "/stocks?low_stock=1");
        assert_eq!(list_href(&s(false, true)), "/stocks?sort=expires_on");
        assert_eq!(
            list_href(&s(true, true)),
            "/stocks?low_stock=1&sort=expires_on"
        );
    }

    #[test]
    fn the_place_and_the_search_follow_encoded() {
        let state = ListState {
            only_low: true,
            by_expiry: false,
            place: Some("Congélateur".to_string()),
            search: Some("pâtes & riz".to_string()),
        };
        assert_eq!(
            list_href(&state),
            "/stocks?low_stock=1&lieu=Cong%C3%A9lateur&q=p%C3%A2tes+%26+riz"
        );
    }

    // -- places / shown ---------------------------------------------------

    #[test]
    fn each_place_comes_once_as_first_written_in_alphabetical_order() {
        let items = [
            item("Lait", Some("Placard"), 1.0, "L"),
            item("Crème", Some(" Frigo "), 1.0, "pot"),
            item("Beurre", Some("frigo"), 1.0, "g"),
            item("Sel", None, 1.0, "kg"),
            item("Riz", Some("  "), 1.0, "kg"),
        ];
        assert_eq!(places(&items), ["Frigo", "Placard"]);
    }

    #[test]
    fn a_place_and_a_search_ignore_case_and_spaces() {
        let pates = item("Pâtes complètes", Some("Placard"), 2.0, "paquet");
        assert!(shown(&pates, None, None));
        assert!(shown(&pates, Some(" placard"), None));
        assert!(!shown(&pates, Some("Frigo"), None));
        assert!(shown(&pates, None, Some("COMPL")));
        assert!(!shown(&pates, None, Some("riz")));
        assert!(shown(&pates, Some("Placard"), Some("pâtes")));
        assert!(!shown(&pates, Some("Frigo"), Some("pâtes")));
        let sel = item("Sel", None, 1.0, "kg");
        assert!(!shown(&sel, Some("Placard"), None));
    }

    // -- expiry_pill ------------------------------------------------------

    #[test]
    fn red_from_the_day_before_amber_for_the_rest_of_the_week() {
        // 2026-10-29 is a Thursday.
        let today = d(2026, 10, 29);
        assert_eq!(
            expiry_pill(Some(d(2026, 10, 27)), today),
            Some(("badge late", "périmé".to_string()))
        );
        assert_eq!(
            expiry_pill(Some(today), today),
            Some(("badge late", "périme aujourd'hui".to_string()))
        );
        assert_eq!(
            expiry_pill(Some(d(2026, 10, 30)), today),
            Some(("badge late", "périme demain".to_string()))
        );
        assert_eq!(
            expiry_pill(Some(d(2026, 10, 31)), today),
            Some(("badge soon", "périme samedi".to_string()))
        );
        assert_eq!(
            expiry_pill(Some(d(2026, 11, 4)), today),
            Some(("badge soon", "périme mercredi".to_string()))
        );
    }

    #[test]
    fn a_later_date_is_neutral_and_none_is_nothing() {
        let today = d(2026, 10, 29);
        assert_eq!(
            expiry_pill(Some(d(2026, 11, 5)), today),
            Some(("badge", "jusqu'au 05/11/2026".to_string()))
        );
        assert_eq!(expiry_pill(None, today), None);
    }

    // -- stepper ----------------------------------------------------------

    #[test]
    fn the_stepper_posts_one_down_and_one_up_and_comes_back() {
        let html = stepper(&item("Pâtes", None, 2.0, "paquet"), "/stocks?lieu=Placard");
        assert_eq!(
            html.matches(r#"action="/stocks/00000000-0000-0000-0000-000000000007/adjust""#)
                .count(),
            2,
            "{html}"
        );
        assert!(html.contains(r#"name="quantity" value="1""#), "{html}");
        assert!(html.contains(r#"name="quantity" value="3""#), "{html}");
        assert_eq!(
            html.matches(r#"name="back" value="/stocks?lieu=Placard""#)
                .count(),
            2,
            "{html}"
        );
        assert!(html.contains(r#"aria-label="Retirer 1 à Pâtes""#), "{html}");
        assert!(html.contains(r#"aria-label="Ajouter 1 à Pâtes""#), "{html}");
        assert!(html.contains(">2 paquets<"), "{html}");
        assert!(!html.contains("disabled"), "{html}");
        assert_eq!(html.matches("data-keep-scroll").count(), 2, "{html}");
    }

    #[test]
    fn minus_stops_at_zero() {
        let empty = stepper(&item("Sel", None, 0.0, "kg"), "/stocks");
        assert!(empty.contains("disabled"), "{empty}");
        assert!(empty.contains(">0 kg<"), "{empty}");
        let half = stepper(&item("Beurre", None, 0.5, "kg"), "/stocks");
        assert!(half.contains(r#"name="quantity" value="0""#), "{half}");
        assert!(half.contains(r#"name="quantity" value="1.5""#), "{half}");
        assert!(!half.contains("disabled"), "{half}");
    }

    #[test]
    fn the_stepper_escapes_the_name() {
        let html = stepper(&item("<b>", None, 1.0, "unité"), "/stocks");
        assert!(html.contains("Ajouter 1 à &lt;b&gt;"), "{html}");
        assert!(!html.contains("<b>"), "{html}");
        assert!(html.contains(">1<"), "{html}");
    }

    // -- body -------------------------------------------------------------

    #[test]
    fn the_filters_offer_every_place_and_mark_the_current_one() {
        let items = [
            item("Lait", Some("Placard"), 1.0, "L"),
            item("Crème", Some("Frigo"), 1.0, "pot"),
        ];
        let view = ListState {
            place: Some("frigo".to_string()),
            ..Default::default()
        };
        let html = body(&view, &items, d(2026, 10, 29), None);
        assert!(html.contains(r#"<a href="/stocks">Tout</a>"#), "{html}");
        assert!(
            html.contains(r#"<a href="/stocks?lieu=Frigo" aria-current="true">Frigo</a>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<a href="/stocks?lieu=Placard">Placard</a>"#),
            "{html}"
        );
        // Only the place's article is listed, and the search keeps the place.
        assert!(html.contains("Crème"), "{html}");
        assert!(!html.contains("<strong>Lait</strong>"), "{html}");
        assert!(
            html.contains(r#"<input type="hidden" name="lieu" value="frigo"/>"#),
            "{html}"
        );
    }

    #[test]
    fn nothing_matching_is_told_apart_from_an_empty_stock() {
        let items = [item("Lait", Some("Placard"), 1.0, "L")];
        let view = ListState {
            search: Some("riz".to_string()),
            ..Default::default()
        };
        let html = body(&view, &items, d(2026, 10, 29), None);
        assert!(html.contains("Aucun article ne correspond."), "{html}");
        let html = body(&ListState::default(), &[], d(2026, 10, 29), None);
        assert!(html.contains("Aucun article pour le moment."), "{html}");
    }

    #[test]
    fn a_card_carries_its_place_its_pill_and_the_low_stock_badge() {
        let mut lait = item("Lait", Some("Frigo"), 1.0, "L");
        lait.reorder_threshold = Some(2.0);
        lait.expires_on = Some(d(2026, 10, 30));
        let html = card(&lait, d(2026, 10, 29), "/stocks");
        assert!(html.contains("Frigo "), "{html}");
        assert!(
            html.contains(r#"<span class="badge late">périme demain</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="badge warn">Stock bas</span>"#),
            "{html}"
        );
        assert!(html.contains(">1 L<"), "{html}");
    }

    #[test]
    fn fmt_num_still_writes_the_posted_quantity_with_a_dot() {
        // The hidden field is parsed by `parse_quantity`, which reads a dot.
        assert_eq!(fmt_num(1.5), "1.5");
        let _ = fmt_date(d(2026, 1, 1));
    }
}
