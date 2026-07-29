//! `/grocery-list` — the family's one shared list. Unchecked items first,
//! then checked ones (backend order `checked, name`), each rendered as a
//! no-JS-safe check-off form (`docs/front-epic-6-grocery-list.md`). The page
//! carries the inline manual-add form and the idempotent generate button;
//! per-row a "Modifier" link renders only for items the caller may modify.
//! PRG banners after add/generate/check/update/delete.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::budget::SetGroceryItemPriceRequest;
use manage_our_home_shared::dto::grocery_list::{
    CheckGroceryItemRequest, CreateGroceryItemRequest, GroceryItemList, GroceryItemResponse,
};
use manage_our_home_shared::validation::budget::validate_entry_form;
use manage_our_home_shared::validation::grocery_list::{
    can_modify, quantity_label, source_label, validate_item_form,
};
use uuid::Uuid;

use crate::app::{html_escape, shell};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{family_context, grocery_cookie, item_not_found_page, service_unavailable_page};

#[derive(serde::Deserialize)]
pub struct ListQuery {
    notice: Option<String>,
    error: Option<String>,
    /// Number of items the generate action created (only set with
    /// `notice=generated`).
    added: Option<usize>,
}

/// Success-banner copy. `generated` is special-cased on the count so a
/// re-run that added nothing reads honestly (idempotent).
fn notice_html(notice: Option<&str>, added: Option<usize>) -> String {
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
        _ => return String::new(),
    };
    format!(r#"<p class="notice success">{}</p>"#, html_escape(&text))
}

fn error_html(error: Option<&str>) -> String {
    let text = match error {
        Some("name_required") => "Le nom est obligatoire.",
        Some("quantity_must_be_non_negative") => "La quantité ne peut pas être négative.",
        Some("amount_must_be_non_negative") => "Le montant ne peut pas être négatif.",
        Some("forbidden") => "Vous n'avez pas les droits nécessaires pour cette action.",
        Some("unavailable") => "Service momentanément indisponible, merci de réessayer.",
        _ => return String::new(),
    };
    format!(r#"<p class="notice error">{}</p>"#, html_escape(text))
}

/// Renders one item as a check-off form plus, for permitted users, an edit
/// link. The checkbox auto-submits on change (progressive enhancement); the
/// always-present `Cocher`/`Décocher` button is the no-JS path and the E2E
/// target. The hidden `checked` field carries the *target* (toggled) state.
///
/// Once an item is checked (bought), the row also shows the Budget
/// price-on-checkout form (F7): an inline amount field posting to
/// `POST /grocery-list/:id/price`. Any member may set a price, and the backend
/// upserts on the item, so a re-tap re-tarifies rather than double-counting.
fn item_row(item: &GroceryItemResponse, can_edit: bool) -> String {
    let next = !item.checked;
    let toggle_label = if item.checked { "Décocher" } else { "Cocher" };
    let checked_attr = if item.checked { " checked" } else { "" };
    let name_style = if item.checked {
        r#" style="text-decoration:line-through;color:var(--muted);""#
    } else {
        ""
    };

    let qty = quantity_label(item.quantity, item.unit.as_deref());
    let qty_html = if qty.is_empty() {
        String::new()
    } else {
        format!(r#" <span class="muted">— {}</span>"#, html_escape(&qty))
    };
    let badge_html = source_label(&item.source)
        .map(|l| {
            format!(
                r#" <span style="font-size:0.75rem;padding:0.1rem 0.4rem;border-radius:3px;background:var(--border);">{}</span>"#,
                html_escape(l)
            )
        })
        .unwrap_or_default();

    let edit_link = if can_edit {
        format!(
            r#"<a class="button secondary" href="/grocery-list/{id}">Modifier</a>"#,
            id = item.id
        )
    } else {
        String::new()
    };

    // Price-on-checkout (F7): only on a checked item, and open to any member.
    let price_form = if item.checked {
        format!(
            r#"<form method="post" action="/grocery-list/{id}/price" style="display:flex;align-items:center;gap:0.4rem;margin:0;">
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
        r#"<li style="display:flex;justify-content:space-between;align-items:center;gap:0.75rem;padding:0.6rem 0;border-bottom:1px solid var(--border);flex-wrap:wrap;">
<form method="post" action="/grocery-list/{id}/check" style="display:flex;align-items:center;gap:0.5rem;margin:0;">
<input type="hidden" name="checked" value="{next}"/>
<input type="checkbox"{checked_attr} onchange="this.form.submit()" aria-label="{name_attr}"/>
<span{name_style}><strong>{name}</strong>{qty_html}{badge_html}</span>
<button type="submit" class="secondary">{toggle_label}</button>
</form>
<span style="display:flex;align-items:center;gap:0.5rem;margin-left:auto;">{price_form}{edit_link}</span>
</li>"#,
        id = item.id,
        name = html_escape(&item.name),
        name_attr = html_escape(&item.name),
    )
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/grocery-list").await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = grocery_cookie(&headers);

    let items: Vec<GroceryItemResponse> = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/grocery-items", fam.gid),
        cookie.as_deref(),
        None,
    )
    .await
    {
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

    let notice = notice_html(query.notice.as_deref(), query.added);
    let error = error_html(query.error.as_deref());

    let list_html = if items.is_empty() {
        r#"<p class="muted">Aucun article pour le moment.</p>"#.to_string()
    } else {
        let rows = items
            .iter()
            .map(|item| item_row(item, can_modify(&fam.role, item.created_by == me.user_id)))
            .collect::<String>();
        format!(r#"<ul style="list-style:none;padding:0;margin:1rem 0 0 0;">{rows}</ul>"#)
    };

    let body = format!(
        r#"{header}
<div style="display:flex;justify-content:space-between;align-items:center;gap:0.75rem;flex-wrap:wrap;">
<h1 style="margin:0;">Liste de courses</h1>
<form method="post" action="/grocery-list/generate" style="margin:0;">
<button type="submit">Générer depuis les recettes et les stocks</button>
</form>
</div>
<p class="muted">Une seule liste partagée par famille. La génération est sans risque à relancer : les articles déjà présents ne sont pas dupliqués.</p>
{notice}{error}
<form method="post" action="/grocery-list/add" style="border:1px solid var(--border);padding:0.75rem;margin-top:1rem;display:flex;gap:0.5rem;align-items:flex-end;flex-wrap:wrap;">
<label style="margin:0;">Article <input type="text" name="name" required placeholder="ex. Lait"/></label>
<label style="margin:0;">Quantité <input type="number" name="quantity" step="any" min="0" placeholder="Optionnel"/></label>
<label style="margin:0;">Unité <input type="text" name="unit" placeholder="Optionnel (ex. L, kg)"/></label>
<button type="submit">Ajouter</button>
</form>
{list_html}"#,
        header = fam.header,
    );
    Html(shell("Liste de courses", &body)).into_response()
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

/// The inline price-on-checkout form (F7). `amount` arrives as a euro string
/// and is parsed in the handler.
#[derive(serde::Deserialize)]
pub struct PriceForm {
    #[serde(default)]
    amount: String,
}

/// Sets a price on a checked grocery item (Budget price-on-checkout, F7). Any
/// member may set a price; the backend upserts on the item, so a re-tap
/// re-tarifies the same budget entry rather than double-counting. Relays to
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

    // Parse + pre-validate the amount (finite, non-negative). The price form
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
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            let code = resp
                .body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unavailable");
            redirect(code)
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            item_not_found_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => redirect("forbidden"),
        Ok(_) | Err(_) => redirect("unavailable"),
    }
}
