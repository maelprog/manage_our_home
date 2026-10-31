//! `/grocery-list` — the family's one shared list, as the mock-ups « Mobile
//! — Courses » and « PC — Courses » draw it (#526). One card holds how many
//! articles are in the basket, the quick-add form and the list in two
//! sections, « À acheter » then « Dans le panier » (backend order `checked,
//! name`), each row a no-JS-safe check-off form whose tick box is its submit
//! button (`docs/front-epic-6-grocery-list.md`). On a PC, two cards beside
//! it: low-stock suggestions and the recipes « À réaliser ». The page also
//! carries the idempotent generate button; per-row a "Modifier" link renders
//! only for items the caller may modify. PRG banners after
//! add/generate/check/update/delete.
//!
//! The generation takes the recipes « À réaliser » only (#470): its button
//! says how many, and links to the recipes when there are none.
//!
//! Once an item is checked, a link under the list leads to the reception
//! (`reception.rs`, #472), which puts the checked items in the stock.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::budget::SetGroceryItemPriceRequest;
use manage_our_home_shared::dto::grocery_list::{
    CheckGroceryItemRequest, CreateGroceryItemRequest, GroceryItemList, GroceryItemResponse,
};
use manage_our_home_shared::dto::recipes::RecipeSelectionResponse;
use manage_our_home_shared::dto::stocks::{StockItemList, StockItemResponse};
use manage_our_home_shared::validation::budget::{can_access_budget, validate_entry_form};
use manage_our_home_shared::validation::grocery_list::{
    can_modify, quantity_label, source_label, validate_item_form,
};
use manage_our_home_shared::validation::stocks::format_quantity;
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::icons::{icon, Icon};
use crate::layout::CurrentUser;
use crate::lenient_query::{lenient, LenientQuery};
use crate::routes::recipes::planned_selections;
use crate::state::{api_request_auth, AppState};

use super::{family_context, grocery_cookie, item_not_found_page, service_unavailable_page};

#[derive(serde::Deserialize)]
pub struct ListQuery {
    notice: Option<String>,
    error: Option<String>,
    /// Number of items the generate action created (only set with
    /// `notice=generated`). Reads as absent when it does not convert (#582):
    /// the banner then says nothing was added.
    #[serde(default, deserialize_with = "lenient")]
    added: Option<usize>,
    /// Number of articles a reception put in the stock (only set with
    /// `notice=received`, #472). Read like `added`.
    #[serde(default, deserialize_with = "lenient")]
    received: Option<usize>,
}

/// Success-banner copy. `generated` is special-cased on the count so a
/// re-run that added nothing reads honestly (idempotent); `received` counts
/// the articles a reception put in the stock (#472).
fn notice_html(notice: Option<&str>, added: Option<usize>, received: Option<usize>) -> String {
    let text = match notice {
        Some("item_added") => "Article ajouté.".to_string(),
        Some("item_updated") => "Article mis à jour.".to_string(),
        Some("item_deleted") => "Article supprimé.".to_string(),
        Some("item_checked") => "Article coché.".to_string(),
        Some("item_unchecked") => "Article décoché.".to_string(),
        Some("price_set") => "Prix enregistré.".to_string(),
        Some("generated") => match added.unwrap_or(0) {
            0 => "Aucun nouvel article à ajouter.".to_string(),
            1 => "1 article ajouté depuis les recettes et les stocks.".to_string(),
            n => format!("{n} articles ajoutés depuis les recettes et les stocks."),
        },
        Some("received") => match received {
            None => "Articles entrés en stock.".to_string(),
            Some(0) => "Aucun article entré en stock.".to_string(),
            Some(1) => "1 article entré en stock.".to_string(),
            Some(n) => format!("{n} articles entrés en stock."),
        },
        _ => return String::new(),
    };
    format!(r#"<p class="notice success">{}</p>"#, html_escape(&text))
}

/// How many recipes « À réaliser » the generation would take, read off the
/// selections `get` fetches for its PC card (#526); `None` when they cannot
/// be read (transport failure or any status but `200`), so the button keeps
/// its generic label rather than claiming there are none.
fn planned_count(selections: Option<&[RecipeSelectionResponse]>) -> Option<usize> {
    selections.map(<[_]>::len)
}

/// The generate button (#470), which names how many recipes « À réaliser »
/// it takes. With none, it is the stocks alone, and a link leads to the
/// recipes to choose some. `None` (the count could not be read) keeps the
/// generic label.
fn generate_html(planned: Option<usize>) -> String {
    let label = match planned {
        None => "Générer depuis les recettes et les stocks".to_string(),
        Some(0) => "Générer depuis les stocks".to_string(),
        Some(1) => "Générer depuis 1 recette et les stocks".to_string(),
        Some(n) => format!("Générer depuis {n} recettes et les stocks"),
    };
    let choose = if planned == Some(0) {
        r#"<a class="btn secondary" href="/recipes#a-realiser">Choisir les recettes à réaliser</a>
"#
    } else {
        ""
    };
    format!(
        r#"<div class="actions">
{choose}<form method="post" action="/grocery-list/generate">
<button type="submit">{label}</button>
</form>
</div>"#
    )
}

/// The way to the reception (#472), under the list once an item is checked.
fn reception_html(checked: usize) -> String {
    if checked == 0 {
        return String::new();
    }
    r#"<div class="actions">
<a class="btn" href="/grocery-list/reception">Réceptionner les articles cochés</a>
</div>"#
        .to_string()
}

fn error_html(error: Option<&str>) -> String {
    let text = match error {
        Some("name_required") => "Le nom est obligatoire.",
        Some("quantity_must_be_non_negative") => "La quantité ne peut pas être négative.",
        Some("amount_must_be_non_negative") => "Le montant ne peut pas être négatif.",
        Some("amount_too_large") => "Le montant ne peut pas dépasser 10 milliards d'euros.",
        Some("forbidden") => "Vous n'avez pas les droits nécessaires pour cette action.",
        Some("grocery_item_priced_with_others") => {
            "Cet article fait partie d'une dépense qui regroupe plusieurs articles : corrigez son montant depuis le budget."
        }
        Some("unavailable") => "Service momentanément indisponible, merci de réessayer.",
        _ => return String::new(),
    };
    format!(r#"<p class="notice error">{}</p>"#, html_escape(text))
}

/// Renders one item as a check-off form plus, for permitted users, an edit
/// link. The tick box of the mock-up (#526) is the form's submit button
/// (`.tick`): it works without JavaScript, and the hidden `checked` field
/// carries the *target* (toggled) state. Its name says what it does,
/// « Cocher Pain » / « Décocher Pain »; the check it draws once ticked is
/// `aria-hidden`, as every icon is. `data-keep-scroll` has the list reopen at
/// the height it was ticked from (#456), since the redirect reloads it — at
/// the top when the check failed, where its error banner is.
///
/// Once an item is checked (bought), the row also shows the Budget
/// price-on-checkout form (F7): an inline amount field posting to
/// `POST /grocery-list/:id/price`. A re-tap updates the expense the item is
/// linked to rather than double-counting — unless that expense carries
/// other items too (#633), which only the budget page corrects. `can_price` is
/// `can_access_budget` of the caller's role (#531): a standard member gets no
/// price field, an owner or an admin adds the spend later.
fn item_row(item: &GroceryItemResponse, can_edit: bool, can_price: bool) -> String {
    let next = !item.checked;
    let (toggle_label, tick_class, tick_icon, name_class) = if item.checked {
        ("Décocher", "tick", icon(Icon::Check), r#" class="done""#)
    } else {
        ("Cocher", "tick secondary", "", "")
    };

    let qty = quantity_label(item.quantity, item.unit.as_deref());
    let qty_html = if qty.is_empty() {
        String::new()
    } else {
        format!(r#"<span class="muted">{}</span>"#, html_escape(&qty))
    };
    let badge_html = source_label(&item.source)
        .map(|l| format!(r#"<span class="badge">{}</span>"#, html_escape(l)))
        .unwrap_or_default();

    let edit_link = if can_edit {
        format!(
            r#"<a class="btn secondary" href="/grocery-list/{id}">Modifier</a>"#,
            id = item.id
        )
    } else {
        String::new()
    };

    // Price-on-checkout (F7): only on a checked item, and only for whom
    // reaches the budget.
    let price_form = if item.checked && can_price {
        format!(
            r#"<form method="post" action="/grocery-list/{id}/price" class="actions">
<input type="number" name="amount" step="0.01" min="0" required placeholder="Prix (€)" aria-label="Prix pour {name_attr}" style="width:7rem;"/>
<button type="submit" class="secondary">Renseigner le prix</button>
</form>"#,
            id = item.id,
            name_attr = html_escape(&item.name),
        )
    } else {
        String::new()
    };

    format!(
        r#"<li class="list-row">
<form method="post" action="/grocery-list/{id}/check" data-keep-scroll>
<input type="hidden" name="checked" value="{next}"/>
<button type="submit" class="{tick_class}" aria-label="{toggle_label} {name}">{tick_icon}</button>
</form>
<span class="what"><strong{name_class}>{name}</strong>{qty_html}{badge_html}</span>
<span class="actions">{price_form}{edit_link}</span>
</li>"#,
        id = item.id,
        name = html_escape(&item.name),
    )
}

// -- the page of the mock-ups (#526) ----------------------------------------

/// Low-stock articles suggested beside the list (#526) — at most this many,
/// the home's stock card's cap.
const SUGGESTION_CAP: usize = 5;

/// The head of the list's card (#526): how many articles are in the basket,
/// of how many, and how many are left — in words, which the gauge under them
/// repeats, so the gauge is `aria-hidden` (the home's grocery tile, #524).
/// Nothing on an empty list: « 0 sur 0 » would say nothing.
fn progress_html(checked: usize, total: usize) -> String {
    if total == 0 {
        return String::new();
    }
    let left = total.saturating_sub(checked);
    let left_label = if left > 1 { "restants" } else { "restant" };
    format!(
        r#"<div class="split"><h2>{checked} sur {total} dans le panier</h2><span class="muted">{left} {left_label}</span></div>
<progress max="{total}" value="{checked}" aria-hidden="true"></progress>"#
    )
}

/// The list in its two sections (#526), « À acheter » then « Dans le
/// panier », read off the checked state alone — the order inside each is
/// the backend's. A section with nothing in it is left out. Under the
/// basket, the way to the reception (#472). `can_price`: see `item_row`.
fn sections_html(
    items: &[GroceryItemResponse],
    can_price: bool,
    can_edit: impl Fn(&GroceryItemResponse) -> bool,
) -> String {
    if items.is_empty() {
        return r#"<p class="muted">Aucun article pour le moment.</p>"#.to_string();
    }
    let section = |title: &str, checked: bool| {
        let rows: String = items
            .iter()
            .filter(|item| item.checked == checked)
            .map(|item| item_row(item, can_edit(item), can_price))
            .collect();
        if rows.is_empty() {
            String::new()
        } else {
            format!(
                r#"<h3 class="eyebrow">{title}</h3>
<ul class="list">{rows}</ul>
"#
            )
        }
    };
    let checked = items.iter().filter(|item| item.checked).count();
    format!(
        "{}{}{}",
        section("À acheter", false),
        section("Dans le panier", true),
        reception_html(checked)
    )
}

/// What the stock is short of and the list does not hold yet (#526): the
/// stock's `low_stock` articles whose name is on no line of the list,
/// checked or not, compared trimmed and case-folded. In the stock's order,
/// at most `SUGGESTION_CAP`.
fn suggested<'a>(
    stock: &'a [StockItemResponse],
    items: &[GroceryItemResponse],
) -> Vec<&'a StockItemResponse> {
    let fold = |name: &str| name.trim().to_lowercase();
    let listed: Vec<String> = items.iter().map(|item| fold(&item.name)).collect();
    stock
        .iter()
        .filter(|s| s.low_stock && !listed.contains(&fold(&s.name)))
        .take(SUGGESTION_CAP)
        .collect()
}

/// The PC's suggestions card (#526). Each one is put on the list by a single
/// press: a form to `/grocery-list/add` carrying the name alone. Its button
/// is named « Mettre … sur la liste » and not « Ajouter … », which the add
/// form's own button is called. `None` is a stock that could not be read,
/// told apart from a stock that lacks nothing.
fn suggestions_html(suggestions: Option<&[&StockItemResponse]>) -> String {
    let body = match suggestions {
        None => r#"<p class="muted">Le stock n'a pas pu être lu.</p>"#.to_string(),
        Some([]) => {
            r#"<p class="muted">Rien ne manque au stock que la liste n'ait déjà.</p>"#.to_string()
        }
        Some(suggestions) => {
            let rows: String = suggestions
                .iter()
                .map(|s| {
                    let name = html_escape(&s.name);
                    let left = format_quantity(s.quantity, &s.unit);
                    format!(
                        r#"<li class="list-row"><span class="what"><strong>{name}</strong><span class="muted">Stock bas : {left}</span></span><form method="post" action="/grocery-list/add"><input type="hidden" name="name" value="{name}"/><button type="submit" class="secondary" aria-label="Mettre {name} sur la liste">{plus}</button></form></li>"#,
                        left = html_escape(&left),
                        plus = icon(Icon::Plus),
                    )
                })
                .collect();
            format!(r#"<ul class="list">{rows}</ul>"#)
        }
    };
    format!(
        r#"<section class="card"><h2>Suggestions</h2>
{body}</section>"#
    )
}

/// The PC's reminder of the recipes « À réaliser » (#526, the selections of
/// #470), by name, with a way to the recipes. `None` is a selection that
/// could not be read, told apart from none planned.
fn planned_html(selections: Option<&[RecipeSelectionResponse]>) -> String {
    let body = match selections {
        None => r#"<p class="muted">Les recettes prévues n'ont pas pu être lues.</p>"#.to_string(),
        Some([]) => r#"<p class="muted">Aucune recette prévue.</p>"#.to_string(),
        Some(selections) => {
            let names: Vec<String> = selections
                .iter()
                .map(|s| html_escape(&s.recipe_name))
                .collect();
            format!("<p>{}</p>", names.join(", "))
        }
    };
    format!(
        r#"<section class="card"><div class="split"><h2>Recettes prévues</h2><a class="more" href="/recipes#a-realiser">Voir les recettes</a></div>
{body}</section>"#
    )
}

/// The whole stock, for the suggestions; `None` when it cannot be read.
async fn stock_items(
    state: &AppState,
    gid: Uuid,
    cookie: Option<&str>,
) -> Option<Vec<StockItemResponse>> {
    match api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/stock-items"),
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<StockItemList>(resp.body)
                .map(|l| l.items)
                .ok()
        }
        Ok(_) | Err(_) => None,
    }
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    LenientQuery(query): LenientQuery<ListQuery>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/grocery-list").await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = grocery_cookie(&headers);

    // The list, the recipes « À réaliser » and the stock, read side by side:
    // the last two only feed the PC's cards and the generate button's label,
    // and either failing leaves the list served.
    let items_path = format!("/groups/{}/grocery-items", fam.gid);
    let (items, planned, stock) = tokio::join!(
        api_request_auth(
            &state,
            reqwest::Method::GET,
            &items_path,
            cookie.as_deref(),
            None,
        ),
        planned_selections(&state, fam.gid, cookie.as_deref()),
        stock_items(&state, fam.gid, cookie.as_deref()),
    );
    let items: Vec<GroceryItemResponse> = match items {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<GroceryItemList>(resp.body)
                .map(|l| l.items)
                .unwrap_or_default()
        }
        // A non-member (403, unreachable once family is resolved) or any other
        // status: render an empty list rather than leaking JSON.
        Ok(_) => Vec::new(),
        Err(_) => return service_unavailable_page().into_response(),
    };
    let planned = planned.ok();
    let suggestions = stock.as_deref().map(|stock| suggested(stock, &items));

    let notice = notice_html(query.notice.as_deref(), query.added, query.received);
    let error = error_html(query.error.as_deref());
    let checked = items.iter().filter(|item| item.checked).count();

    let body = format!(
        r#"<div class="page-header">
<h1>Liste de courses</h1>
{generate}
</div>
<p class="muted">Une seule liste partagée par famille. La génération reprend les ingrédients manquants des recettes à réaliser, pour leur nombre de personnes, et les stocks sous leur seuil. Elle est sans risque à relancer : les articles déjà présents ne sont pas dupliqués.</p>
{notice}{error}
<div class="groceries">
<section class="card">
{progress}
<form method="post" action="/grocery-list/add" class="quick-add">
<label>Article <input type="text" name="name" required placeholder="ex. Lait"/></label>
<label>Quantité <input type="number" name="quantity" step="any" min="0" placeholder="Optionnel"/></label>
<label>Unité <input type="text" name="unit" placeholder="Optionnel (ex. L, kg)"/></label>
<button type="submit">{plus} Ajouter</button>
</form>
{list}</section>
<aside class="pc">
{suggestions}
{planned}
</aside>
</div>"#,
        generate = generate_html(planned_count(planned.as_deref())),
        progress = progress_html(checked, items.len()),
        plus = icon(Icon::Plus),
        list = sections_html(&items, can_access_budget(&fam.role), |item| can_modify(
            &fam.role,
            item.created_by == me.user_id
        )),
        suggestions = suggestions_html(suggestions.as_deref()),
        planned = planned_html(planned.as_deref()),
    );
    Html(shell_with_header(
        Width::Full,
        "Liste de courses",
        &fam.header,
        &body,
    ))
    .into_response()
}

// -- mutations --------------------------------------------------------------

/// The inline manual-add form fields. `quantity` arrives as a string (empty =
/// no quantity) and is parsed in the handler.
#[derive(serde::Deserialize)]
pub struct AddForm {
    name: String,
    #[serde(default)]
    quantity: String,
    #[serde(default)]
    unit: String,
}

/// Parses a form quantity string to an optional non-negative `f64`: empty →
/// `None` (no quantity, unlike Stocks where it defaults to `0`), else a parsed
/// `f64` wrapped in `Some`. `Err(())` on an unparseable value.
pub(crate) fn parse_optional_quantity(s: &str) -> Result<Option<f64>, ()> {
    let t = s.trim();
    if t.is_empty() {
        Ok(None)
    } else {
        t.parse::<f64>().map(Some).map_err(|_| ())
    }
}

pub async fn add(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<AddForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/grocery-list").await else {
        return Redirect::to("/groups/new").into_response();
    };

    let redirect =
        |code: &str| Redirect::to(&format!("/grocery-list?error={code}")).into_response();

    let Ok(quantity) = parse_optional_quantity(&form.quantity) else {
        return redirect("quantity_must_be_non_negative");
    };
    // Pre-validate (name non-empty, quantity non-negative) before the round
    // trip; the backend 400 is the defensive fallback below.
    if let Err(e) = validate_item_form(&form.name, quantity) {
        return redirect(e.code());
    }

    let unit = {
        let u = form.unit.trim();
        (!u.is_empty()).then(|| u.to_string())
    };
    let req = CreateGroceryItemRequest {
        name: form.name.trim().to_string(),
        quantity,
        unit,
    };

    let cookie = grocery_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/grocery-items", fam.gid),
        cookie.as_deref(),
        Some(serde_json::to_value(&req).unwrap()),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            Redirect::to("/grocery-list?notice=item_added").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            let code = resp
                .body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unavailable");
            redirect(code)
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => redirect("forbidden"),
        Ok(_) | Err(_) => redirect("unavailable"),
    }
}

pub async fn generate(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/grocery-list").await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = grocery_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/grocery-items/generate", fam.gid),
        cookie.as_deref(),
        None,
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            let added = serde_json::from_value::<GroceryItemList>(resp.body)
                .map(|l| l.items.len())
                .unwrap_or(0);
            format!("/grocery-list?notice=generated&added={added}")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            "/grocery-list?error=forbidden".to_string()
        }
        Ok(_) | Err(_) => "/grocery-list?error=unavailable".to_string(),
    };
    Redirect::to(&target).into_response()
}

#[derive(serde::Deserialize)]
pub struct CheckForm {
    /// The *target* checked state (the row sends the toggle of the current
    /// value). Parsed leniently: anything but "true" is treated as unchecking.
    #[serde(default)]
    checked: String,
}

pub async fn check(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(item_id): Path<Uuid>,
    Form(form): Form<CheckForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/grocery-list").await else {
        return Redirect::to("/groups/new").into_response();
    };
    let checked = form.checked.trim() == "true";
    let body = CheckGroceryItemRequest { checked };

    let cookie = grocery_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/grocery-items/{}/check", fam.gid, item_id),
        cookie.as_deref(),
        Some(serde_json::to_value(&body).unwrap()),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            let notice = if checked {
                "item_checked"
            } else {
                "item_unchecked"
            };
            Redirect::to(&format!("/grocery-list?notice={notice}")).into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            item_not_found_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            Redirect::to("/grocery-list?error=forbidden").into_response()
        }
        Ok(_) | Err(_) => Redirect::to("/grocery-list?error=unavailable").into_response(),
    }
}

/// The `?error=` code a refused price is relayed with: a 400's own code
/// (an amount the pre-validation let through), the 409 of an item whose
/// expense carries other items too (#633) — which only the budget can
/// correct —, `forbidden` on a 403, and `unavailable` for the rest.
fn price_error_code(status: reqwest::StatusCode, body: &serde_json::Value) -> String {
    let code = body.get("error").and_then(|e| e.as_str());
    match status {
        reqwest::StatusCode::BAD_REQUEST => code.unwrap_or("unavailable").to_string(),
        reqwest::StatusCode::CONFLICT if code == Some("grocery_item_priced_with_others") => {
            "grocery_item_priced_with_others".to_string()
        }
        reqwest::StatusCode::FORBIDDEN => "forbidden".to_string(),
        _ => "unavailable".to_string(),
    }
}

/// The inline price-on-checkout form (F7). `amount` arrives as a euro string
/// and is parsed in the handler.
#[derive(serde::Deserialize)]
pub struct PriceForm {
    #[serde(default)]
    amount: String,
}

/// Sets a price on a checked grocery item (Budget price-on-checkout, F7). An
/// owner or an admin may set a price (#531: a standard member's is the
/// backend's 404, the item-not-found page). A re-tap re-tarifies the expense
/// the item is linked to rather than double-counting; when that expense
/// links other items too (#633), the API refuses with a 409
/// `grocery_item_priced_with_others` and the list says to correct it from
/// the budget (`price_error_code`). Relays to
/// `POST /groups/:id/grocery-items/:item_id/price`.
pub async fn price(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(item_id): Path<Uuid>,
    Form(form): Form<PriceForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/grocery-list").await else {
        return Redirect::to("/groups/new").into_response();
    };

    let redirect =
        |code: &str| Redirect::to(&format!("/grocery-list?error={code}")).into_response();

    // Parse + pre-validate the amount (finite, non-negative, at most
    // `AMOUNT_MAX_EUROS`). The price form
    // has no name field, so only the amount arm of `validate_entry_form` can
    // fire — a placeholder name keeps the name arm satisfied.
    let Ok(amount) = form.amount.trim().parse::<f64>() else {
        return redirect("amount_must_be_non_negative");
    };
    if let Err(e) = validate_entry_form("prix", amount) {
        return redirect(e.code());
    }

    let body = SetGroceryItemPriceRequest {
        amount,
        spent_at: None,
    };

    let cookie = grocery_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/grocery-items/{}/price", fam.gid, item_id),
        cookie.as_deref(),
        Some(serde_json::to_value(&body).unwrap()),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            Redirect::to("/grocery-list?notice=price_set").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            item_not_found_page().into_response()
        }
        Ok(resp) => redirect(&price_error_code(resp.status, &resp.body)),
        Err(_) => redirect("unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list_query(raw: &str) -> ListQuery {
        crate::lenient_query::from_query_str(raw)
            .expect("a malformed parameter must not reject the page")
    }

    // -- the price relay's refusals (#633's leftover, closed by #533) --------

    #[test]
    fn an_item_priced_with_others_is_its_own_error_not_an_outage() {
        let code = price_error_code(
            reqwest::StatusCode::CONFLICT,
            &serde_json::json!({ "error": "grocery_item_priced_with_others" }),
        );
        assert_eq!(code, "grocery_item_priced_with_others");
        let banner = error_html(Some(&code));
        assert!(banner.contains("plusieurs articles"), "{banner}");
        assert!(banner.contains("budget"), "{banner}");
    }

    #[test]
    fn a_price_above_the_bound_is_worded() {
        let code = price_error_code(
            reqwest::StatusCode::BAD_REQUEST,
            &serde_json::json!({ "error": "amount_too_large" }),
        );
        assert_eq!(code, "amount_too_large");
        assert!(error_html(Some(&code)).contains("10 milliards"));
    }

    #[test]
    fn a_rejected_amount_keeps_the_api_code() {
        let code = price_error_code(
            reqwest::StatusCode::BAD_REQUEST,
            &serde_json::json!({ "error": "amount_must_be_non_negative" }),
        );
        assert_eq!(code, "amount_must_be_non_negative");
    }

    #[test]
    fn any_other_refusal_of_the_price_is_named_as_such() {
        let forbidden = price_error_code(reqwest::StatusCode::FORBIDDEN, &serde_json::json!({}));
        assert_eq!(forbidden, "forbidden");
        for status in [
            reqwest::StatusCode::CONFLICT,
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            assert_eq!(
                price_error_code(status, &serde_json::json!({ "error": "other" })),
                "unavailable",
                "{status}"
            );
        }
    }

    #[test]
    fn absent_added_reads_as_none() {
        // `default` beside `deserialize_with`: a missing parameter is not a
        // "missing field" 400.
        for raw in ["", "notice=item_added"] {
            assert!(list_query(raw).added.is_none(), "{raw}");
        }
    }

    #[test]
    fn malformed_added_reads_as_absent() {
        // #582: empty, unconvertible, negative.
        for raw in [
            "added=",
            "added=abc",
            "added=-1",
            "notice=generated&added=1.5",
        ] {
            assert!(list_query(raw).added.is_none(), "{raw}");
        }
        // The generate banner then reads as a run that added nothing.
        let q = list_query("notice=generated&added=abc");
        assert_eq!(
            notice_html(q.notice.as_deref(), q.added, None),
            r#"<p class="notice success">Aucun nouvel article à ajouter.</p>"#
        );
    }

    #[test]
    fn valid_added_keeps_its_effect() {
        let q = list_query("notice=generated&added=3");
        assert_eq!(q.added, Some(3));
        assert!(notice_html(q.notice.as_deref(), q.added, None).contains("3 articles ajoutés"));
    }

    #[test]
    fn the_generate_button_counts_the_recipes_to_cook() {
        let one = generate_html(Some(1));
        assert!(
            one.contains(">Générer depuis 1 recette et les stocks</button>"),
            "{one}"
        );
        let three = generate_html(Some(3));
        assert!(
            three.contains(">Générer depuis 3 recettes et les stocks</button>"),
            "{three}"
        );
        for html in [&one, &three] {
            assert!(!html.contains("/recipes#a-realiser"), "{html}");
            assert!(
                html.contains(r#"<form method="post" action="/grocery-list/generate">"#),
                "{html}"
            );
        }
    }

    #[test]
    fn without_recipes_to_cook_the_button_links_to_the_selection() {
        let html = generate_html(Some(0));
        assert!(
            html.contains(">Générer depuis les stocks</button>"),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<a class="btn secondary" href="/recipes#a-realiser">Choisir les recettes à réaliser</a>"#
            ),
            "{html}"
        );
    }

    #[test]
    fn an_unknown_count_keeps_the_generic_label() {
        let html = generate_html(None);
        assert!(
            html.contains(">Générer depuis les recettes et les stocks</button>"),
            "{html}"
        );
        assert!(!html.contains("/recipes#a-realiser"), "{html}");
    }

    /// A stand-in for apps/api answering the selections read with
    /// `status` and `body`.
    async fn fake_selections_api(status: u16, body: &'static str) -> AppState {
        use axum::routing::get;
        let app = axum::Router::new().route(
            "/groups/:gid/recipes/selections",
            get(move || async move {
                (
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    body,
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        AppState {
            http: reqwest::Client::new(),
            api_internal_base_url: format!("http://{addr}"),
            api_public_base_url: "/api".into(),
            body_read_limits: manage_our_home_http_guard::BodyReadLimits::PRODUCTION,
            upload_gate: manage_our_home_http_guard::UploadGate::new(8, 2),
            shutdown: manage_our_home_http_guard::Shutdown::new(),
        }
    }

    #[tokio::test]
    async fn an_unread_selection_count_keeps_the_generic_button() {
        // A status other than 200 is not « no recipe to cook »: the button
        // must not say « Générer depuis les stocks » nor link to the choice.
        for status in [403, 404, 500, 503] {
            let state = fake_selections_api(status, r#"{"error":"x"}"#).await;
            let selections = planned_selections(&state, Uuid::from_u128(1), Some("session=s"))
                .await
                .ok();
            let planned = planned_count(selections.as_deref());
            assert_eq!(planned, None, "{status}");
            let html = generate_html(planned);
            assert!(
                html.contains(">Générer depuis les recettes et les stocks</button>"),
                "{status}: {html}"
            );
            assert!(!html.contains("/recipes#a-realiser"), "{status}: {html}");
        }
    }

    #[tokio::test]
    async fn an_empty_selection_read_counts_no_recipe() {
        let state = fake_selections_api(200, r#"{"selections":[]}"#).await;
        let selections = planned_selections(&state, Uuid::from_u128(1), Some("session=s"))
            .await
            .ok();
        let planned = planned_count(selections.as_deref());
        assert_eq!(planned, Some(0));
        assert!(generate_html(planned).contains(">Générer depuis les stocks</button>"));
    }

    #[test]
    fn the_reception_notice_counts_the_articles_received() {
        let q = list_query("notice=received&received=1");
        assert_eq!(
            notice_html(q.notice.as_deref(), None, q.received),
            r#"<p class="notice success">1 article entré en stock.</p>"#
        );
        let q = list_query("notice=received&received=3");
        assert_eq!(
            notice_html(q.notice.as_deref(), None, q.received),
            r#"<p class="notice success">3 articles entrés en stock.</p>"#
        );
        // A count that does not convert reads as absent (#582).
        let q = list_query("notice=received&received=abc");
        assert_eq!(
            notice_html(q.notice.as_deref(), None, q.received),
            r#"<p class="notice success">Articles entrés en stock.</p>"#
        );
    }

    #[test]
    fn the_reception_link_shows_only_with_checked_items() {
        assert_eq!(reception_html(0), "");
        for n in [1, 4] {
            let html = reception_html(n);
            assert!(
                html.contains(
                    r#"<a class="btn" href="/grocery-list/reception">Réceptionner les articles cochés</a>"#
                ),
                "{html}"
            );
        }
    }

    #[test]
    fn a_repeated_parameter_keeps_its_first_occurrence() {
        let q = list_query("notice=generated&added=2&added=5&notice=item_added");
        assert_eq!(q.notice.as_deref(), Some("generated"));
        assert_eq!(q.added, Some(2));
    }

    // -- the page of the mock-ups (#526) -----------------------------------

    fn item(name: &str, checked: bool) -> GroceryItemResponse {
        GroceryItemResponse {
            id: Uuid::from_u128(name.len() as u128 + if checked { 1000 } else { 0 }),
            group_id: Uuid::from_u128(1),
            created_by: Uuid::from_u128(2),
            name: name.to_string(),
            quantity: None,
            unit: None,
            checked,
            source: "manual".to_string(),
            source_recipe_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn stock(name: &str, quantity: f64, unit: &str, low: bool) -> StockItemResponse {
        StockItemResponse {
            id: Uuid::from_u128(name.len() as u128),
            group_id: Uuid::from_u128(1),
            created_by: Uuid::from_u128(2),
            name: name.to_string(),
            category: None,
            quantity,
            unit: unit.to_string(),
            reorder_threshold: low.then_some(1.0),
            low_stock: low,
            expires_on: None,
            expiry_status: manage_our_home_shared::validation::stocks::ExpiryStatus::Unknown,
            barcode: None,
        }
    }

    fn selection(name: &str) -> RecipeSelectionResponse {
        RecipeSelectionResponse {
            id: Uuid::from_u128(name.len() as u128),
            recipe_id: Uuid::from_u128(name.len() as u128 + 1),
            recipe_name: name.to_string(),
            status: "planned".to_string(),
            servings: None,
            created_by: Uuid::from_u128(2),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn the_progress_says_how_many_are_in_the_basket_of_how_many() {
        let html = progress_html(3, 7);
        assert!(html.contains("<h2>3 sur 7 dans le panier</h2>"), "{html}");
        assert!(html.contains(">4 restants<"), "{html}");
        // The gauge repeats the words: hidden from assistive technology.
        assert!(
            html.contains(r#"<progress max="7" value="3" aria-hidden="true"></progress>"#),
            "{html}"
        );
    }

    #[test]
    fn one_or_none_left_is_singular() {
        assert!(progress_html(6, 7).contains(">1 restant<"));
        assert!(progress_html(7, 7).contains(">0 restant<"));
        assert!(progress_html(0, 1).contains("<h2>0 sur 1 dans le panier</h2>"));
    }

    #[test]
    fn an_empty_list_has_no_progress() {
        assert_eq!(progress_html(0, 0), "");
    }

    #[test]
    fn the_sections_split_on_the_checked_state_in_the_list_order() {
        let items = [item("Pain", false), item("Sel", false), item("Lait", true)];
        let html = sections_html(&items, true, |_| false);
        let buy = html
            .find(r#"<h3 class="eyebrow">À acheter</h3>"#)
            .expect("À acheter");
        let basket = html
            .find(r#"<h3 class="eyebrow">Dans le panier</h3>"#)
            .expect("Dans le panier");
        let (pain, sel, lait) = (
            html.find("Cocher Pain").unwrap(),
            html.find("Cocher Sel").unwrap(),
            html.find("Décocher Lait").unwrap(),
        );
        assert!(
            buy < pain && pain < sel && sel < basket && basket < lait,
            "{html}"
        );
    }

    #[test]
    fn a_section_with_nothing_in_it_is_left_out() {
        let html = sections_html(&[item("Pain", false)], true, |_| false);
        assert!(html.contains("À acheter"), "{html}");
        assert!(!html.contains("Dans le panier"), "{html}");
        assert!(!html.contains("/grocery-list/reception"), "{html}");

        let html = sections_html(&[item("Lait", true)], true, |_| false);
        assert!(!html.contains("À acheter"), "{html}");
        assert!(html.contains("Dans le panier"), "{html}");
        // Something is checked: the way to the reception (#472).
        assert!(html.contains(r#"href="/grocery-list/reception""#), "{html}");
    }

    #[test]
    fn an_empty_list_says_so() {
        let html = sections_html(&[], true, |_| true);
        assert!(html.contains("Aucun article pour le moment."), "{html}");
        assert!(!html.contains("<h3"), "{html}");
    }

    #[test]
    fn the_tick_box_is_the_form_button_and_works_without_javascript() {
        let it = item("Pain", false);
        let html = item_row(&it, false, true);
        assert!(
            html.contains(&format!(
                r#"<form method="post" action="/grocery-list/{}/check" data-keep-scroll>"#,
                it.id
            )),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="hidden" name="checked" value="true"/>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<button type="submit" class="tick secondary" aria-label="Cocher Pain"></button>"#
            ),
            "{html}"
        );
        // No bare checkbox any more: the box is the button.
        assert!(!html.contains("checkbox"), "{html}");
        assert!(!html.contains(r#"class="done""#), "{html}");
        assert!(!html.contains("/price"), "{html}");
    }

    #[test]
    fn a_checked_row_is_ticked_struck_through_and_priced() {
        let html = item_row(&item("Lait", true), false, true);
        assert!(
            html.contains(r#"<input type="hidden" name="checked" value="false"/>"#),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"<button type="submit" class="tick" aria-label="Décocher Lait">{}</button>"#,
                icon(Icon::Check)
            )),
            "{html}"
        );
        assert!(
            html.contains(r#"<strong class="done">Lait</strong>"#),
            "{html}"
        );
        assert!(html.contains("/price"), "{html}");
    }

    #[test]
    fn a_standard_member_s_checked_row_has_no_price_field() {
        // #531: the price is a budget entry, which a standard member does not
        // reach; an owner or an admin adds the spend later.
        let html = item_row(&item("Lait", true), false, false);
        assert!(
            html.contains(r#"<strong class="done">Lait</strong>"#),
            "{html}"
        );
        assert!(!html.contains("/price"), "{html}");
        assert!(!html.contains("Prix"), "{html}");
        let html = sections_html(&[item("Lait", true)], false, |_| false);
        assert!(!html.contains("/price"), "{html}");
        assert!(sections_html(&[item("Lait", true)], true, |_| false).contains("/price"));
    }

    #[test]
    fn the_row_names_the_quantity_the_provenance_and_the_edit_link() {
        let mut it = item("Farine", false);
        it.quantity = Some(300.0);
        it.unit = Some("g".into());
        it.source = "recipe".into();
        let html = item_row(&it, true, true);
        assert!(
            html.contains(r#"<span class="muted">300 g</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="badge">Recette</span>"#),
            "{html}"
        );
        assert!(html.contains(">Modifier</a>"), "{html}");
        assert!(!item_row(&it, false, true).contains("Modifier"));
    }

    #[test]
    fn the_suggestions_are_the_low_stock_the_list_lacks() {
        let stock = [
            stock("Beurre", 0.0, "kg", true),
            stock("Café", 1.0, "paquet", true),
            stock("Riz", 5.0, "kg", false),
            stock("Lait", 0.0, "L", true),
        ];
        // On the list already, checked or not, whatever its case.
        let items = [item(" café ", false), item("LAIT", true)];
        let names: Vec<&str> = suggested(&stock, &items)
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, ["Beurre"]);
    }

    #[test]
    fn the_suggestions_are_capped() {
        let stock: Vec<StockItemResponse> = (0..8)
            .map(|n| stock(&format!("Article {n}"), 0.0, "kg", true))
            .collect();
        assert_eq!(suggested(&stock, &[]).len(), SUGGESTION_CAP);
    }

    #[test]
    fn a_suggestion_writes_the_stock_left_as_the_stock_list_does() {
        let pates = stock("Pâtes", 2.0, "paquet", true);
        let oeufs = stock("Œufs", 1.0, "unité", true);
        let lait = stock("Lait", 0.5, "L", true);
        let html = suggestions_html(Some(&[&pates, &oeufs, &lait]));
        assert!(html.contains("Stock bas : 2 paquets"), "{html}");
        assert!(html.contains("Stock bas : 1<"), "{html}");
        assert!(html.contains("Stock bas : 0,5 L"), "{html}");
    }

    #[test]
    fn a_suggestion_puts_the_article_on_the_list_in_one_press() {
        let beurre = stock("Beurre", 0.0, "kg", true);
        let html = suggestions_html(Some(&[&beurre]));
        assert!(html.contains("<h2>Suggestions</h2>"), "{html}");
        assert!(html.contains("<strong>Beurre</strong>"), "{html}");
        assert!(html.contains("Stock bas : 0 kg"), "{html}");
        assert!(
            html.contains(r#"<form method="post" action="/grocery-list/add">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="hidden" name="name" value="Beurre"/>"#),
            "{html}"
        );
        // Not « Ajouter … »: the add form's own button is named « Ajouter ».
        assert!(
            html.contains(&format!(
                r#"<button type="submit" class="secondary" aria-label="Mettre Beurre sur la liste">{}</button>"#,
                icon(Icon::Plus)
            )),
            "{html}"
        );
    }

    #[test]
    fn no_suggestion_and_an_unread_stock_say_so_apart() {
        let none = suggestions_html(Some(&[]));
        assert!(
            none.contains("Rien ne manque au stock que la liste n'ait déjà."),
            "{none}"
        );
        let unread = suggestions_html(None);
        assert!(unread.contains("Le stock n'a pas pu être lu."), "{unread}");
    }

    #[test]
    fn the_planned_recipes_are_named_with_a_link_to_the_recipes() {
        let html = planned_html(Some(&[selection("Soupe de potiron"), selection("Quiche")]));
        assert!(html.contains("<h2>Recettes prévues</h2>"), "{html}");
        assert!(html.contains("<p>Soupe de potiron, Quiche</p>"), "{html}");
        assert!(
            html.contains(r#"<a class="more" href="/recipes#a-realiser">Voir les recettes</a>"#),
            "{html}"
        );
        let escaped = planned_html(Some(&[selection("Pâtes <b>")]));
        assert!(escaped.contains("Pâtes &lt;b&gt;"), "{escaped}");
    }

    #[test]
    fn no_planned_recipe_and_an_unread_selection_say_so_apart() {
        let none = planned_html(Some(&[]));
        assert!(none.contains("Aucune recette prévue."), "{none}");
        assert!(none.contains(r#"href="/recipes#a-realiser""#), "{none}");
        let unread = planned_html(None);
        assert!(
            unread.contains("Les recettes prévues n'ont pas pu être lues."),
            "{unread}"
        );
    }
}
