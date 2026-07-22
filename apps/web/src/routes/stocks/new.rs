//! `/stocks/new` — manually add a stock item (name, category, quantity, unit,
//! reorder threshold). The empty-name / empty-unit / negative-quantity /
//! negative-threshold rules are pre-validated by the shared
//! `validate_item_form` (inline error, no round trip); the backend's matching
//! 400s are mapped defensively. Any member may create, so 403 isn't reachable
//! via the UI. Success (201) → PRG `/stocks?notice=item_created`.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::stocks::CreateStockItemRequest;
use manage_our_home_shared::validation::stocks::{validate_item_form, ItemFormError};

use crate::app::{html_escape, shell};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{family_context, forbidden_page, stocks_cookie};

/// The shared item form fields, used by both create and edit. `quantity` /
/// `reorder_threshold` arrive as strings and are parsed in the handler.
#[derive(serde::Deserialize, Default)]
pub struct ItemForm {
    pub name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub quantity: String,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub reorder_threshold: String,
}

/// French copy for a stock-item form error code.
pub(crate) fn error_message(code: &str) -> &'static str {
    match code {
        "name_required" => "Le nom est obligatoire.",
        "unit_required" => "L'unité est obligatoire.",
        "quantity_must_be_non_negative" => "La quantité ne peut pas être négative.",
        "reorder_threshold_must_be_non_negative" => "Le seuil de réappro ne peut pas être négatif.",
        "unavailable" => "Service momentanément indisponible, merci de réessayer.",
        _ => "Une erreur est survenue, merci de réessayer.",
    }
}

/// Maps a shared `ItemFormError` to the backend's matching error code, so the
/// inline message and a defensively-mapped 400 read identically.
pub(crate) fn form_error_code(err: ItemFormError) -> &'static str {
    match err {
        ItemFormError::NameRequired => "name_required",
        ItemFormError::UnitRequired => "unit_required",
        ItemFormError::QuantityNegative => "quantity_must_be_non_negative",
        ItemFormError::ThresholdNegative => "reorder_threshold_must_be_non_negative",
    }
}

/// Parses a form quantity string to a non-negative `f64`, defaulting an empty
/// input to `0.0`. Returns `None` only on an unparseable value.
pub(crate) fn parse_quantity(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        Some(0.0)
    } else {
        t.parse::<f64>().ok()
    }
}

/// Parses an optional threshold: empty string → `None` (no threshold), else a
/// parsed `f64` wrapped in `Some`. `Err(())` on an unparseable value.
pub(crate) fn parse_threshold(s: &str) -> Result<Option<f64>, ()> {
    let t = s.trim();
    if t.is_empty() {
        Ok(None)
    } else {
        t.parse::<f64>().map(Some).map_err(|_| ())
    }
}

/// Renders the item form fields (shared markup, pre-filled). Used by create
/// (empty defaults) and edit (existing values).
pub(crate) fn form_fields(
    name: &str,
    category: &str,
    quantity: &str,
    unit: &str,
    reorder_threshold: &str,
) -> String {
    format!(
        r#"<label>Nom <input type="text" name="name" required value="{name}"/></label>
<label>Catégorie <input type="text" name="category" value="{category}" placeholder="Optionnel (ex. Cellier, Frigo)"/></label>
<label>Quantité <input type="number" name="quantity" step="any" min="0" value="{quantity}"/></label>
<label>Unité <input type="text" name="unit" required value="{unit}" placeholder="ex. kg, L, unité"/></label>
<label>Seuil de réappro
<input type="number" name="reorder_threshold" step="any" min="0" value="{reorder_threshold}" placeholder="Optionnel — laisser vide pour aucun seuil"/>
<span class="muted">En dessous ou à ce niveau, l'article est signalé « stock bas ». Partagé au niveau de la famille.</span>
</label>"#,
        name = html_escape(name),
        category = html_escape(category),
        quantity = html_escape(quantity),
        unit = html_escape(unit),
        reorder_threshold = html_escape(reorder_threshold),
    )
}

fn page(header: &str, form: &ItemForm, error: Option<&str>) -> String {
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
<h1>Nouvel article</h1>
{error_html}
<form method="post" action="/stocks/new">
{fields}
<button type="submit">Ajouter l'article</button>
</form>
<div class="links"><a href="/stocks">Retour aux stocks</a></div>"#,
    );
    shell("Nouvel article", &body)
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/stocks/new").await else {
        return Redirect::to("/groups/new").into_response();
    };
    // Default a fresh form to quantity 0.
    let form = ItemForm {
        quantity: "0".to_string(),
        ..Default::default()
    };
    Html(page(&fam.header, &form, None)).into_response()
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ItemForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/stocks/new").await else {
        return Redirect::to("/groups/new").into_response();
    };

    let render_error =
        |code: &str| Html(page(&fam.header, &form, Some(error_message(code)))).into_response();

    let Some(quantity) = parse_quantity(&form.quantity) else {
        return render_error("quantity_must_be_non_negative");
    };
    let Ok(reorder_threshold) = parse_threshold(&form.reorder_threshold) else {
        return render_error("reorder_threshold_must_be_non_negative");
    };

    if let Err(e) = validate_item_form(&form.name, &form.unit, quantity, reorder_threshold) {
        return render_error(form_error_code(e));
    }

    let category = {
        let c = form.category.trim();
        (!c.is_empty()).then(|| c.to_string())
    };
    let req = CreateStockItemRequest {
        name: form.name.trim().to_string(),
        category,
        quantity,
        unit: form.unit.trim().to_string(),
        reorder_threshold,
    };

    let cookie = stocks_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/stock-items", fam.gid),
        cookie.as_deref(),
        Some(serde_json::to_value(&req).unwrap()),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            Redirect::to("/stocks?notice=item_created").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            // The shared pre-validation should have caught these; map the
            // backend's exact code defensively if one slips through.
            let code = resp
                .body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unavailable");
            render_error(code)
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            forbidden_page().into_response()
        }
        Ok(_) | Err(_) => render_error("unavailable"),
    }
}
