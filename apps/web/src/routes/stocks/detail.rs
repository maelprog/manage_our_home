//! `/stocks/:id` — item detail with the low-stock and expiry states, plus the
//! quantity-adjust action and the delete action; full edit lives in `edit.rs`.
//! Under « Utilisé dans », the planned recipes that take from the article
//! and their total against the stock (#712).
//!
//! Permission bar: the quantity-adjust form renders for **any** family member
//! (shared inventory — issue #39); the edit link and delete button render only
//! for the item's creator or a member whose role meets the family's
//! `stocks_edit` threshold (`can_modify`, #866). A member below it viewing
//! another member's item still gets the adjust form, plus a
//! muted note that editing/deleting is reserved. The backend stays the
//! authority — a forged full-edit/DELETE is 403'd and mapped to
//! `?error=forbidden` here defensively.
//!
//! Error table: 404 (`get_stock_item`) unknown/foreign item → introuvable
//! page; 403 (`update`/`delete` permission bar) → `?error=forbidden`.
//!
//! The ingredient (#904, `ingredient_html`): an article no category
//! recognizes is asked which ingredient it is, and `POST
//! /stocks/:id/ingredient` searches then links its name
//! (`…/ingredient-links/{name}`, #903); a linked one shows it, with
//! « Changer » and « Retirer » (`POST /stocks/:id/ingredient/delete`).
//! Errors: 400 `unknown_tag` → `?error=unknown_ingredient`, 409 →
//! `link_limit`, anything else → `link_unavailable` / `unlink_unavailable`,
//! the term kept in `?q=`; a failed search → `search_unavailable`.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::stocks::{StockItemResponse, UpdateStockItemRequest};
use manage_our_home_shared::validation::stocks::{
    check_quantities, format_quantity, is_low_stock, ItemFormError,
};
use uuid::Uuid;

use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::validation::dietary::SEARCH_QUERY_MAX_LEN;

use crate::app::{html_escape, shell_with_header, Width};
use crate::edit_rule::{who_may, Guarded};
use crate::icons::{icon, Icon};
use crate::layout::CurrentUser;
use crate::routes::account::allergies::Found;
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
    /// `change`: « Changer » was followed (#904), the ingredient's card is
    /// shown in place of its line.
    ingredient: Option<String>,
    /// The ingredient's field, kept across a failed link (#904).
    q: Option<String>,
}

/// The notice of `code`. A link names its ingredient, read back from the
/// article (#904); a removed one says whether the article still counts.
fn notice_text(code: &str, ingredient: Option<&ItemIngredient>) -> Option<String> {
    let text = match code {
        "item_updated" => "Article mis à jour.",
        "quantity_adjusted" => "Quantité mise à jour.",
        // A scanned article no category recognizes lands here (#904).
        "item_created" => "Article ajouté au stock.",
        "ingredient_linked" => {
            return Some(match ingredient.and_then(|i| i.link.as_ref()) {
                Some(link) => format!("Article rattaché à « {} ».", link.label),
                None => "Article rattaché.".to_string(),
            })
        }
        "ingredient_unlinked" if ingredient.is_some_and(|i| i.recognized) => "Rattachement retiré.",
        "ingredient_unlinked" => {
            "Rattachement retiré. Cet article ne compte plus pour aucune recette."
        }
        _ => return None,
    };
    Some(text.to_string())
}

fn error_text(code: &str) -> Option<&'static str> {
    match code {
        "forbidden" => Some("Vous n'avez pas les droits nécessaires pour cette action."),
        "unavailable" => Some("Service momentanément indisponible, merci de réessayer."),
        "quantity_too_large" => Some(super::QUANTITY_TOO_LARGE_MESSAGE),
        "link_unavailable" => {
            Some("Impossible de rattacher l'article pour le moment. Réessayez dans un instant.")
        }
        "unknown_ingredient" => Some("Cet ingrédient n'existe pas. Choisissez-en un autre."),
        "link_limit" => {
            Some("La famille a atteint le nombre maximal d'articles rattachés à un ingrédient.")
        }
        "unlink_unavailable" => {
            Some("Impossible de retirer le rattachement pour le moment. Réessayez dans un instant.")
        }
        "search_unavailable" => Some(
            "La recherche d'ingrédient est indisponible pour le moment. Réessayez dans un instant.",
        ),
        _ => None,
    }
}

/// What `GET /groups/:id/stock-items/:item_id/ingredient` says of an
/// article (#904): whether it has a category, and its name's link.
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct ItemIngredient {
    pub(crate) recognized: bool,
    pub(crate) link: Option<ItemLink>,
}

/// The family's link for an article's name (#903): its category's French
/// name, and who may delete it with admins and the owner.
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct ItemLink {
    pub(crate) label: String,
    pub(crate) created_by: Uuid,
}

#[derive(serde::Deserialize)]
struct SearchResults {
    results: Vec<Found>,
}

/// What the ingredient's card shows beyond the article's state (#904).
#[derive(Default)]
pub(crate) struct Ask<'a> {
    /// « Changer » was followed, or a search ran: the card, even for a
    /// linked article.
    change: bool,
    /// What the field holds.
    query: &'a str,
    /// What `POST /taxonomy/search` found for it, once it ran.
    found: Option<&'a [Found]>,
    /// The last attempt failed: the button says « Réessayer ».
    retry: bool,
}

/// The article, or the page that says why there is none.
async fn fetch_item(
    state: &AppState,
    gid: Uuid,
    item_id: Uuid,
    cookie: Option<&str>,
) -> Result<StockItemResponse, Html<String>> {
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
                .map_err(|_| service_unavailable_page())
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => Err(item_not_found_page()),
        _ => Err(service_unavailable_page()),
    }
}

/// The article's ingredient (#904); `None` when the API does not say,
/// and the page then says nothing of it.
pub(crate) async fn fetch_ingredient(
    state: &AppState,
    gid: Uuid,
    item_id: Uuid,
    cookie: Option<&str>,
) -> Option<ItemIngredient> {
    match api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/stock-items/{item_id}/ingredient"),
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value(resp.body).ok()
        }
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
    let error = query.error.as_deref();
    let ask = Ask {
        change: query.ingredient.as_deref() == Some("change"),
        query: query.q.as_deref().unwrap_or_default(),
        found: None,
        retry: matches!(error, Some("link_unavailable" | "search_unavailable")),
    };
    show(
        &state,
        &headers,
        &me,
        item_id,
        query.notice.as_deref(),
        error,
        &ask,
    )
    .await
}

/// The article's page, with its ingredient's card as `ask` has it.
async fn show(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    item_id: Uuid,
    notice: Option<&str>,
    error: Option<&str>,
    ask: &Ask<'_>,
) -> Response {
    let Some(fam) = family_context(state, headers, me, &format!("/stocks/{item_id}")).await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = stocks_cookie(headers);
    let item = match fetch_item(state, fam.gid, item_id, cookie.as_deref()).await {
        Ok(item) => item,
        Err(page) => return page.into_response(),
    };
    let ingredient = fetch_ingredient(state, fam.gid, item_id, cookie.as_deref()).await;

    let can_edit = can_modify(&fam.role, &fam.edit_min_role, item.created_by == me.user_id);
    // The bar of `DELETE …/ingredient-links/{name}`: its author, an admin
    // or the owner — not the family's `stocks_edit` threshold.
    let can_unlink = ingredient
        .as_ref()
        .and_then(|i| i.link.as_ref())
        .is_some_and(|l| can_modify(&fam.role, "admin", l.created_by == me.user_id));
    let notice = notice.and_then(|code| notice_text(code, ingredient.as_ref()));
    let error = error.and_then(error_text);
    let card = ingredient_html(&item, ingredient.as_ref(), ask, can_unlink);
    Html(page(
        &fam.header,
        &item,
        can_edit,
        &fam.edit_min_role,
        notice.as_deref(),
        error,
        &card,
    ))
    .into_response()
}

/// The ingredient of an article (#904, maquette validée le 2026-11-19),
/// between its dates and « Utilisé dans ». An article no category
/// recognizes gets a card: « Cet article ne compte pour aucune recette.
/// Lequel est-ce ? », a field, and without JavaScript a post that comes
/// back with the results as radios — `app::ENHANCE_SCRIPT` reads them to
/// suggest as one types (`data-ing-search`). A linked one gets a line,
/// « Ingrédient : … », with « Changer » (the card again) and, for who may
/// delete the link, « Retirer ». Nothing for an article that has a
/// category of its own, nor when the API does not answer.
pub(crate) fn ingredient_html(
    item: &StockItemResponse,
    ingredient: Option<&ItemIngredient>,
    ask: &Ask,
    can_unlink: bool,
) -> String {
    let Some(ingredient) = ingredient else {
        return String::new();
    };
    let id = item.id;
    let searching = ask.change || ask.found.is_some() || ask.retry;
    match &ingredient.link {
        Some(link) if !searching => return linked_html(item, link, can_unlink),
        None if ingredient.recognized => return String::new(),
        _ => {}
    }
    let lead = match &ingredient.link {
        Some(link) => format!(
            "<strong>Changer d'ingrédient.</strong> Actuellement : {}.",
            html_escape(&link.label)
        ),
        None => "<strong>Cet article ne compte pour aucune recette.</strong> Lequel est-ce ?"
            .to_string(),
    };
    let save = if ingredient.link.is_some() {
        "Enregistrer"
    } else {
        "Rattacher"
    };
    let action = format!("/stocks/{id}/ingredient");
    let query = html_escape(ask.query);
    let found = ask.found.filter(|found| !found.is_empty());
    // The help is for the field to fill; once results are listed, their
    // legend says what was found.
    let (described, help) = match found {
        Some(_) => ("", ""),
        None => (
            r#" aria-describedby="ing-h""#,
            r#"<p class="help" id="ing-h">Écrivez le nom de l'ingrédient : les recettes qui l'utilisent compteront cet article.</p>"#,
        ),
    };
    let (button, picks) = match found {
        Some(found) => (
            r#"<button type="submit" class="secondary">Chercher</button>"#.to_string(),
            picks_html(&action, ask.query, found, save),
        ),
        None => (
            format!(
                r#"<button type="submit">{}</button>"#,
                if ask.retry { "Réessayer" } else { save }
            ),
            String::new(),
        ),
    };
    let status = match ask.found {
        Some([]) => format!(
            "Aucun ingrédient ne correspond à « {query} ». Essayez un mot plus court ou au singulier."
        ),
        _ => String::new(),
    };
    let cancel = if ingredient.link.is_some() {
        format!(r#"<p class="cancel"><a class="more" href="/stocks/{id}">Annuler</a></p>"#)
    } else {
        String::new()
    };
    format!(
        r#"<section class="card ing" aria-labelledby="ing-t"><h2 id="ing-t" class="sr-only">Rattacher à un ingrédient</h2>
<p class="lead">{lead}</p>
<form method="post" action="{action}" role="search" data-ing-search><div class="ing-field"><label for="ing-q">Ingrédient</label><div class="ing-box">{icon}<input id="ing-q" name="q" type="search" value="{query}" maxlength="{SEARCH_QUERY_MAX_LEN}" autocomplete="off" placeholder="lait, farine, tomate…"{described}/></div></div><input type="hidden" name="ingredient" value=""/>{button}{help}</form>
{picks}<p class="status" role="status">{status}</p>{cancel}</section>"#,
        icon = icon(Icon::Search),
    )
}

/// The results of a search without JavaScript: one radio per category,
/// the first checked, for the form that links the one chosen.
fn picks_html(action: &str, query: &str, found: &[Found], save: &str) -> String {
    let query = html_escape(query);
    let legend = match found.len() {
        1 => format!("1 ingrédient correspond à « {query} »"),
        n => format!("{n} ingrédients correspondent à « {query} »"),
    };
    let rows: String = found
        .iter()
        .enumerate()
        .map(|(i, f)| {
            format!(
                r#"<label class="list-row pick"><input type="radio" name="ingredient" value="{tag}"{checked}/>{label}</label>"#,
                tag = html_escape(&f.tag),
                checked = if i == 0 { " checked" } else { "" },
                label = html_escape(&f.label),
            )
        })
        .collect();
    format!(
        r#"<form class="ing-picks" method="post" action="{action}"><input type="hidden" name="q" value="{query}"/><fieldset class="plain" data-ing-picks><legend>{legend}</legend>{rows}</fieldset><button type="submit">{save}</button></form>"#
    )
}

/// A linked article's line: its ingredient, « Changer », and « Retirer »
/// for who may delete the link.
fn linked_html(item: &StockItemResponse, link: &ItemLink, can_unlink: bool) -> String {
    let id = item.id;
    let name = html_escape(&item.name);
    let label = html_escape(&link.label);
    let unlink = if can_unlink {
        format!(
            r#"<form method="post" action="/stocks/{id}/ingredient/delete"><button type="submit" class="secondary danger" aria-label="Retirer le rattachement de {name} à l'ingrédient {label}">Retirer</button></form>"#
        )
    } else {
        String::new()
    };
    format!(
        r#"<div class="split ing-row"><p><strong>Ingrédient :</strong> {label}</p><div class="actions"><a class="more" href="/stocks/{id}?ingredient=change" aria-label="Changer l'ingrédient de {name}">Changer</a>{unlink}</div></div>"#
    )
}

/// Where a link lands: the article, with its notice, or with the error
/// and the term the field held.
pub(crate) fn linked_target(item_id: Uuid, outcome: Result<(), (&str, &str)>) -> String {
    match outcome {
        Ok(()) => format!("/stocks/{item_id}?notice=ingredient_linked"),
        Err((code, "")) => format!("/stocks/{item_id}?error={code}"),
        Err((code, query)) => format!(
            "/stocks/{item_id}?{}",
            serde_urlencoded::to_string([("error", code), ("q", query)]).unwrap_or_default()
        ),
    }
}

/// `…/ingredient-links/{name}`: the article's name as one path segment,
/// every reserved character percent-encoded — a space as `%20`, since a
/// `+` in a path is a `+`.
pub(crate) fn link_path(gid: Uuid, name: &str) -> String {
    let encoded = serde_urlencoded::to_string([("", name)]).unwrap_or_default();
    // `=` then the value; the form encoding writes a space `+` and a `+`
    // `%2B`, so every `+` left is a space.
    let segment = encoded.trim_start_matches('=').replace('+', "%20");
    format!("/groups/{gid}/ingredient-links/{segment}")
}

#[derive(serde::Deserialize)]
pub struct LinkForm {
    #[serde(default)]
    q: String,
    /// The category chosen; empty, the form searches `q`.
    #[serde(default)]
    ingredient: String,
}

/// `POST /stocks/:id/ingredient` (#904): with a category chosen, links the
/// article's name to it (`PUT …/ingredient-links/{name}`, any member) and
/// lands on the article; without, searches the term
/// (`POST /taxonomy/search`) and shows the article with what it found.
/// A `POST`, so that the term stays out of the address, as on the
/// allergies' page.
pub async fn link(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(item_id): Path<Uuid>,
    Form(form): Form<LinkForm>,
) -> Response {
    let tag = form.ingredient.trim();
    let query = form.q.trim();
    if tag.is_empty() {
        return search(&state, &headers, &me, item_id, query).await;
    }
    let Some(fam) = family_context(&state, &headers, &me, &format!("/stocks/{item_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = stocks_cookie(&headers);
    let item = match fetch_item(&state, fam.gid, item_id, cookie.as_deref()).await {
        Ok(item) => item,
        Err(page) => return page.into_response(),
    };
    let result = api_request_auth(
        &state,
        reqwest::Method::PUT,
        &link_path(fam.gid, &item.name),
        cookie.as_deref(),
        Some(serde_json::json!({ "tag": tag })),
    )
    .await;
    let outcome = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => Ok(()),
        Ok(resp)
            if resp.status == reqwest::StatusCode::BAD_REQUEST
                && resp.body.get("error").and_then(|e| e.as_str()) == Some("unknown_tag") =>
        {
            Err("unknown_ingredient")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::CONFLICT => Err("link_limit"),
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => Err("forbidden"),
        _ => Err("link_unavailable"),
    };
    Redirect::to(&linked_target(
        item_id,
        outcome.map_err(|code| (code, query)),
    ))
    .into_response()
}

/// The article with the categories `query` finds, or with an error that
/// keeps the term.
async fn search(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    item_id: Uuid,
    query: &str,
) -> Response {
    let mut ask = Ask {
        change: true,
        query,
        ..Ask::default()
    };
    if query.is_empty() {
        return show(state, headers, me, item_id, None, None, &ask).await;
    }
    let cookie = stocks_cookie(headers);
    let result = api_request_auth(
        state,
        reqwest::Method::POST,
        "/taxonomy/search",
        cookie.as_deref(),
        Some(serde_json::json!({ "query": query })),
    )
    .await;
    let found = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<SearchResults>(resp.body).ok()
        }
        _ => None,
    };
    match found {
        Some(found) => {
            ask.found = Some(&found.results);
            show(state, headers, me, item_id, None, None, &ask).await
        }
        None => {
            ask.retry = true;
            let error = Some("search_unavailable");
            show(state, headers, me, item_id, None, error, &ask).await
        }
    }
}

/// `POST /stocks/:id/ingredient/delete` (#904): removes the link of the
/// article's name (`DELETE …/ingredient-links/{name}`: its author, an
/// admin or the owner), without confirmation — linking again undoes it.
/// A link already gone lands as removed.
pub async fn unlink(
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
    let item = match fetch_item(&state, fam.gid, item_id, cookie.as_deref()).await {
        Ok(item) => item,
        Err(page) => return page.into_response(),
    };
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &link_path(fam.gid, &item.name),
        cookie.as_deref(),
        None,
    )
    .await;
    let query = match result {
        Ok(resp)
            if resp.status == reqwest::StatusCode::NO_CONTENT
                || resp.status == reqwest::StatusCode::NOT_FOUND =>
        {
            "notice=ingredient_unlinked"
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => "error=forbidden",
        _ => "error=unlink_unavailable",
    };
    Redirect::to(&format!("/stocks/{item_id}?{query}")).into_response()
}

fn page(
    header: &str,
    item: &StockItemResponse,
    can_edit: bool,
    edit_min_role: &str,
    notice: Option<&str>,
    error: Option<&str>,
    ingredient: &str,
) -> String {
    let id = item.id;
    let notice_html = notice
        .map(|n| {
            format!(
                r#"<p class="notice success" role="status">{}</p>"#,
                html_escape(n)
            )
        })
        .unwrap_or_default();
    let error_html = error
        .map(|e| {
            format!(
                r#"<p class="notice error" role="alert">{}</p>"#,
                html_escape(e)
            )
        })
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
        format!(
            r#"<p class="muted">{}</p>"#,
            who_may(edit_min_role, Guarded::StockItem)
        )
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
{ingredient}
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
    fn an_admin_under_an_owner_threshold_is_told_the_owner_alone_edits() {
        // #962: the line follows the group's `stocks_edit` threshold, not a
        // fixed « créateur ou administrateur ».
        let can_edit = can_modify("admin", "owner", false);
        assert!(!can_edit);
        let html = page(
            "",
            &item(1.0, "pot", None),
            can_edit,
            "owner",
            None,
            None,
            "",
        );
        assert!(
            html.contains(r#"<p class="muted">Seul le propriétaire ou le créateur peut modifier ou supprimer cet article.</p>"#),
            "{html}"
        );
        assert!(!html.contains("/delete"), "{html}");
    }

    #[test]
    fn the_quantity_and_the_threshold_read_as_the_list_writes_them() {
        let html = page(
            "",
            &item(6.0, "unité", Some(1.5)),
            false,
            "admin",
            None,
            None,
            "",
        );
        assert!(html.contains("<strong>Quantité :</strong> 6</p>"), "{html}");
        assert!(
            html.contains("<strong>Seuil de réappro :</strong> 1,5</p>"),
            "{html}"
        );
        let html = page(
            "",
            &item(2.0, "paquet", Some(1.0)),
            false,
            "admin",
            None,
            None,
            "",
        );
        assert!(
            html.contains("<strong>Quantité :</strong> 2 paquets</p>"),
            "{html}"
        );
        assert!(
            html.contains("<strong>Seuil de réappro :</strong> 1 paquet</p>"),
            "{html}"
        );
    }

    // -- the ingredient (#904) ------------------------------------------------

    fn creams(created_by: Uuid) -> ItemIngredient {
        ItemIngredient {
            recognized: true,
            link: Some(ItemLink {
                label: "Crèmes épaisses".to_string(),
                created_by,
            }),
        }
    }

    fn unknown() -> ItemIngredient {
        ItemIngredient {
            recognized: false,
            link: None,
        }
    }

    fn found(names: &[(&str, &str)]) -> Vec<Found> {
        names
            .iter()
            .map(|(tag, label)| Found {
                tag: tag.to_string(),
                label: label.to_string(),
            })
            .collect()
    }

    fn card(ing: Option<&ItemIngredient>, ask: &Ask, can_unlink: bool) -> String {
        ingredient_html(&item(1.0, "pot", None), ing, ask, can_unlink)
    }

    #[test]
    fn without_the_api_s_answer_nothing_is_said() {
        assert_eq!(card(None, &Ask::default(), true), "");
    }

    #[test]
    fn an_article_with_a_category_and_no_link_shows_nothing() {
        let lait = ItemIngredient {
            recognized: true,
            link: None,
        };
        assert_eq!(card(Some(&lait), &Ask::default(), true), "");
    }

    #[test]
    fn an_article_without_a_category_is_asked_which_ingredient_it_is() {
        let html = card(Some(&unknown()), &Ask::default(), false);
        assert!(
            html.starts_with(r#"<section class="card ing" aria-labelledby="ing-t">"#),
            "{html}"
        );
        assert!(
            html.contains(
                "<strong>Cet article ne compte pour aucune recette.</strong> Lequel est-ce ?"
            ),
            "{html}"
        );
        let id = id();
        assert!(
            html.contains(&format!(
                r#"<form method="post" action="/stocks/{id}/ingredient" role="search" data-ing-search>"#
            )),
            "{html}"
        );
        assert!(
            html.contains(r#"<label for="ing-q">Ingrédient</label>"#),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"<input id="ing-q" name="q" type="search" value="" maxlength="{SEARCH_QUERY_MAX_LEN}" autocomplete="off" placeholder="lait, farine, tomate…" aria-describedby="ing-h"/>"#
            )),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="hidden" name="ingredient" value=""/>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<button type="submit">Rattacher</button>"#),
            "{html}"
        );
        assert!(html.contains(r#"<p class="help" id="ing-h">"#), "{html}");
        assert!(
            html.contains(r#"<p class="status" role="status"></p>"#),
            "{html}"
        );
        assert!(!html.contains("Annuler"), "{html}");
    }

    #[test]
    fn a_linked_article_shows_its_ingredient_with_changer() {
        let html = card(Some(&creams(Uuid::nil())), &Ask::default(), false);
        assert!(!html.contains("<section"), "{html}");
        assert!(
            html.contains("<p><strong>Ingrédient :</strong> Crèmes épaisses</p>"),
            "{html}"
        );
        let id = id();
        assert!(
            html.contains(&format!(
                r#"<a class="more" href="/stocks/{id}?ingredient=change" aria-label="Changer l'ingrédient de Pâtes">Changer</a>"#
            )),
            "{html}"
        );
        // « Retirer » for who may delete the link only.
        assert!(!html.contains("Retirer"), "{html}");
        let html = card(Some(&creams(Uuid::nil())), &Ask::default(), true);
        assert!(
            html.contains(&format!(
                r#"<form method="post" action="/stocks/{id}/ingredient/delete"><button type="submit" class="secondary danger" aria-label="Retirer le rattachement de Pâtes à l'ingrédient Crèmes épaisses">Retirer</button></form>"#
            )),
            "{html}"
        );
    }

    #[test]
    fn changer_recalls_the_link_saves_and_cancels() {
        let ask = Ask {
            change: true,
            ..Ask::default()
        };
        let html = card(Some(&creams(Uuid::nil())), &ask, true);
        assert!(
            html.contains("<strong>Changer d'ingrédient.</strong> Actuellement : Crèmes épaisses."),
            "{html}"
        );
        assert!(
            html.contains(r#"<button type="submit">Enregistrer</button>"#),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"<p class="cancel"><a class="more" href="/stocks/{}">Annuler</a></p>"#,
                id()
            )),
            "{html}"
        );
        assert!(!html.contains("Retirer"), "{html}");
    }

    #[test]
    fn the_results_are_radios_the_first_checked() {
        let results = found(&[("en:milks", "Lait"), ("en:coconut-milks", "Lait de coco")]);
        let ask = Ask {
            query: "lai",
            found: Some(&results),
            ..Ask::default()
        };
        let html = card(Some(&unknown()), &ask, false);
        assert!(html.contains(r#"value="lai""#), "{html}");
        assert!(
            html.contains(r#"<button type="submit" class="secondary">Chercher</button>"#),
            "{html}"
        );
        let id = id();
        assert!(
            html.contains(&format!(
                r#"<form class="ing-picks" method="post" action="/stocks/{id}/ingredient"><input type="hidden" name="q" value="lai"/><fieldset class="plain" data-ing-picks><legend>2 ingrédients correspondent à « lai »</legend><label class="list-row pick"><input type="radio" name="ingredient" value="en:milks" checked/>Lait</label><label class="list-row pick"><input type="radio" name="ingredient" value="en:coconut-milks"/>Lait de coco</label></fieldset><button type="submit">Rattacher</button></form>"#
            )),
            "{html}"
        );
        // The help is for an empty field; the legend says the count.
        assert!(!html.contains("ing-h"), "{html}");
    }

    #[test]
    fn one_result_is_said_in_the_singular() {
        let results = found(&[("en:milks", "Lait")]);
        let ask = Ask {
            query: "lait",
            found: Some(&results),
            ..Ask::default()
        };
        let html = card(Some(&unknown()), &ask, false);
        assert!(
            html.contains("<legend>1 ingrédient correspond à « lait »</legend>"),
            "{html}"
        );
    }

    #[test]
    fn no_result_is_said_in_the_status_not_as_an_error() {
        let ask = Ask {
            query: "xyzzy",
            found: Some(&[]),
            ..Ask::default()
        };
        let html = card(Some(&unknown()), &ask, false);
        assert!(
            html.contains(r#"<p class="status" role="status">Aucun ingrédient ne correspond à « xyzzy ». Essayez un mot plus court ou au singulier.</p>"#),
            "{html}"
        );
        assert!(!html.contains("error"), "{html}");
        assert!(!html.contains("data-ing-picks"), "{html}");
    }

    #[test]
    fn after_an_error_the_term_is_kept_and_the_button_retries() {
        let ask = Ask {
            query: r#"lait "entier" <bio>"#,
            retry: true,
            ..Ask::default()
        };
        let html = card(Some(&unknown()), &ask, false);
        assert!(
            html.contains(r#"value="lait &quot;entier&quot; &lt;bio&gt;""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<button type="submit">Réessayer</button>"#),
            "{html}"
        );
    }

    #[test]
    fn the_notices_name_the_ingredient_and_what_unlinking_leaves() {
        assert_eq!(
            notice_text("ingredient_linked", Some(&creams(Uuid::nil()))).as_deref(),
            Some("Article rattaché à « Crèmes épaisses ».")
        );
        assert_eq!(
            notice_text("ingredient_unlinked", Some(&unknown())).as_deref(),
            Some("Rattachement retiré. Cet article ne compte plus pour aucune recette.")
        );
        let lait = ItemIngredient {
            recognized: true,
            link: None,
        };
        assert_eq!(
            notice_text("ingredient_unlinked", Some(&lait)).as_deref(),
            Some("Rattachement retiré.")
        );
        assert_eq!(
            notice_text("item_created", None).as_deref(),
            Some("Article ajouté au stock.")
        );
        assert_eq!(notice_text("anything-else", None), None);
    }

    #[test]
    fn every_ingredient_error_has_a_message() {
        for code in [
            "link_unavailable",
            "unknown_ingredient",
            "link_limit",
            "unlink_unavailable",
            "search_unavailable",
        ] {
            assert!(error_text(code).is_some(), "{code}");
        }
    }

    #[test]
    fn a_link_lands_on_the_article_its_term_kept_on_an_error() {
        let id = id();
        assert_eq!(
            linked_target(id, Ok(())),
            format!("/stocks/{id}?notice=ingredient_linked")
        );
        assert_eq!(
            linked_target(id, Err(("link_unavailable", "pâtes & riz"))),
            format!("/stocks/{id}?error=link_unavailable&q=p%C3%A2tes+%26+riz")
        );
        assert_eq!(
            linked_target(id, Err(("link_limit", ""))),
            format!("/stocks/{id}?error=link_limit")
        );
    }

    #[test]
    fn the_link_s_name_is_one_encoded_path_segment() {
        let gid = Uuid::nil();
        assert_eq!(
            link_path(gid, "Mon p'tit plaisir"),
            format!("/groups/{gid}/ingredient-links/Mon%20p%27tit%20plaisir")
        );
        assert_eq!(
            link_path(gid, "Pâtes & riz/2+1 ?#"),
            format!("/groups/{gid}/ingredient-links/P%C3%A2tes%20%26%20riz%2F2%2B1%20%3F%23")
        );
    }

    #[test]
    fn the_card_sits_between_the_dates_and_the_planned_recipes() {
        let mut pates = item(3.0, "paquet", None);
        pates.planned_usage = Some(vec![usage(1, "Bolo", 1.0, "paquet")]);
        let html = page("", &pates, false, "admin", None, None, "<CARD>");
        let dates = html.find("Aucune date de péremption.").expect(&html);
        let card = html.find("<CARD>").expect(&html);
        let planned = html.find("Utilisé dans").expect(&html);
        assert!(dates < card && card < planned, "{html}");
    }

    #[test]
    fn the_notices_are_announced() {
        let html = page(
            "",
            &item(1.0, "pot", None),
            false,
            "admin",
            Some("Ok."),
            Some("Non."),
            "",
        );
        assert!(
            html.contains(r#"<p class="notice success" role="status">Ok.</p>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<p class="notice error" role="alert">Non.</p>"#),
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
            let html = page("", &pates, false, "admin", None, None, "");
            assert!(!html.contains("Utilisé dans"), "{html}");
            assert!(!html.contains("Total prévu"), "{html}");
        }
    }

    #[test]
    fn one_planned_recipe_is_linked_with_its_quantity_and_the_total() {
        let mut pates = item(3.0, "paquet", None);
        pates.planned_usage = Some(vec![usage(1, "Pâtes <bolo>", 1.0, "paquet")]);
        let html = page("", &pates, false, "admin", None, None, "");
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
        let html = page("", &pates, false, "admin", None, None, "");
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
        let html = page("", &pates, false, "admin", None, None, "");
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
