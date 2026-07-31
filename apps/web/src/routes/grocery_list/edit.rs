//! `/grocery-list/:id` — a grocery item's edit screen (name / quantity /
//! unit) with a delete action. Same permission bar as delete (`can_modify`):
//! a non-permitted user never sees the form (GET → forbidden page), and the
//! backend 403 is mapped to `?error=forbidden` on the list defensively. A
//! blank quantity/unit field *clears* it (sent as `Some(None)` per the
//! backend's double-`Option` PATCH contract); the form always sends every
//! field. Success (200) → PRG `/grocery-list?notice=item_updated`.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::grocery_list::{GroceryItemResponse, UpdateGroceryItemRequest};
use manage_our_home_shared::validation::grocery_list::{fmt_num, validate_item_form};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::list::parse_optional_quantity;
use super::{
    can_modify, family_context, forbidden_page, grocery_cookie, item_not_found_page,
    service_unavailable_page,
};

/// The edit form fields. `quantity` arrives as a string (empty = clear) and is
/// parsed in the handler.
#[derive(serde::Deserialize, Default)]
pub struct EditForm {
    pub name: String,
    #[serde(default)]
    pub quantity: String,
    #[serde(default)]
    pub unit: String,
}

/// French copy for a grocery-item edit error code.
fn error_message(code: &str) -> &'static str {
    match code {
        "name_required" => "Le nom est obligatoire.",
        "quantity_must_be_non_negative" => "La quantité ne peut pas être négative.",
        "unavailable" => "Service momentanément indisponible, merci de réessayer.",
        _ => "Une erreur est survenue, merci de réessayer.",
    }
}

fn page(header: &str, id: Uuid, name: &str, form: &EditForm, error: Option<&str>) -> String {
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let body = format!(
        r#"<h1>Modifier — {name_esc}</h1>
{error_html}
<form method="post" action="/grocery-list/{id}/edit">
<label>Nom <input type="text" name="name" required value="{name_val}"/></label>
<label>Quantité <input type="number" name="quantity" step="any" min="0" value="{quantity}" placeholder="Optionnel — laisser vide pour aucune quantité"/></label>
<label>Unité <input type="text" name="unit" value="{unit}" placeholder="Optionnel (ex. L, kg)"/></label>
<button type="submit">Enregistrer</button>
</form>
<form method="post" action="/grocery-list/{id}/delete" class="actions">
<button type="submit" class="secondary danger">Supprimer</button>
</form>
<div class="links"><a href="/grocery-list">Retour à la liste de courses</a></div>"#,
        name_esc = html_escape(name),
        name_val = html_escape(&form.name),
        quantity = html_escape(&form.quantity),
        unit = html_escape(&form.unit),
    );
    shell_with_header(Width::Form, "Modifier l'article", header, &body)
}

/// Fetches an item, returning it or an early `Response` (404/unavailable).
async fn fetch_item(
    state: &AppState,
    gid: Uuid,
    item_id: Uuid,
    cookie: Option<&str>,
) -> Result<GroceryItemResponse, Response> {
    match api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/grocery-items/{item_id}"),
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<GroceryItemResponse>(resp.body)
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
    let Some(fam) =
        family_context(&state, &headers, &me, &format!("/grocery-list/{item_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = grocery_cookie(&headers);
    let item = match fetch_item(&state, fam.gid, item_id, cookie.as_deref()).await {
        Ok(i) => i,
        Err(resp) => return resp,
    };

    if !can_modify(&fam.role, item.created_by == me.user_id) {
        return forbidden_page().into_response();
    }

    let form = EditForm {
        name: item.name.clone(),
        quantity: item.quantity.map(fmt_num).unwrap_or_default(),
        unit: item.unit.clone().unwrap_or_default(),
    };
    Html(page(&fam.header, item_id, &item.name, &form, None)).into_response()
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(item_id): Path<Uuid>,
    Form(form): Form<EditForm>,
) -> Response {
    let Some(fam) =
        family_context(&state, &headers, &me, &format!("/grocery-list/{item_id}")).await
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

    let Ok(quantity) = parse_optional_quantity(&form.quantity) else {
        return render_error("quantity_must_be_non_negative");
    };
    if let Err(e) = validate_item_form(&form.name, quantity) {
        return render_error(e.code());
    }

    // The edit form always sends every field: name as `Some(..)`, and a blank
    // quantity/unit clears it (`Some(None)`), else sets it (`Some(Some(..))`).
    let unit = {
        let u = form.unit.trim();
        Some((!u.is_empty()).then(|| u.to_string()))
    };
    let body = UpdateGroceryItemRequest {
        name: Some(form.name.trim().to_string()),
        quantity: Some(quantity),
        unit,
    };

    let cookie = grocery_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::PATCH,
        &format!("/groups/{}/grocery-items/{}", fam.gid, item_id),
        cookie.as_deref(),
        Some(serde_json::to_value(&body).unwrap()),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            Redirect::to("/grocery-list?notice=item_updated").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            Redirect::to(&format!("/grocery-list/{item_id}?error=forbidden")).into_response()
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

pub async fn delete(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(item_id): Path<Uuid>,
) -> Response {
    let Some(fam) =
        family_context(&state, &headers, &me, &format!("/grocery-list/{item_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = grocery_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &format!("/groups/{}/grocery-items/{}", fam.gid, item_id),
        cookie.as_deref(),
        None,
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            Redirect::to("/grocery-list?notice=item_deleted").into_response()
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
