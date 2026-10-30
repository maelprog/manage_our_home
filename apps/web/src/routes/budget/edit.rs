//! `/budget/:id` — a budget entry's edit screen with a delete action: the
//! form of `/budget/new` (#533), pre-filled — amount, name, category, date,
//! and the grocery items it links to, which stay offered checked or not.
//! Same permission bar as delete (`can_modify`): a non-permitted user never
//! sees the form (GET → forbidden page), and the backend 403 is mapped to
//! `?error=forbidden` on the entry screen defensively. `spent_at` is
//! `NOT NULL`, so the form always sends it and a round trip preserves the
//! date. The category and the grocery items are sent only when their pills
//! were drawn (`CATEGORIES_PRESENT_FIELD`, `ITEMS_PRESENT_FIELD`): a picker
//! that could not load leaves the entry's category and links as they are.
//! Success (200) → PRG `/budget?notice=entry_updated`.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use manage_our_home_shared::dto::budget::{BudgetEntryResponse, UpdateBudgetEntryRequest};
use manage_our_home_shared::validation::budget::{amount_input_value, validate_entry_form};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::routes::agenda::new::is_form_urlencoded;
use crate::state::{api_request_auth, AppState};

use super::new::{
    error_message, fetch_choices, form_fields, known_code, parse_amount, parse_spent_at, Choices,
    EntryForm,
};
use super::{
    budget_cookie, can_modify, entry_not_found_page, family_context, forbidden_page,
    service_unavailable_page, uuids_from_raw_form,
};

/// The PATCH body of a submitted edit form. The form always sends every
/// field. `spent_at` has no "clear" state (NOT NULL in the DB): a valid date
/// sets it, an empty/unparseable value leaves it as-is (`None` — omitted from
/// the wire). The category and the links go only with the marker of the pills
/// that offered them: pills left undrawn (the API did not answer) must not
/// clear what the entry has.
fn update_body(form: &EntryForm, amount: f64, items: &[Uuid]) -> UpdateBudgetEntryRequest {
    UpdateBudgetEntryRequest {
        name: Some(form.name.trim().to_string()),
        amount: Some(amount),
        spent_at: parse_spent_at(&form.spent_at),
        category_id: form.category(),
        grocery_item_ids: form.grocery_items_present.as_ref().map(|_| items.to_vec()),
    }
}

#[allow(clippy::too_many_arguments)]
fn page(
    header: &str,
    id: Uuid,
    name: &str,
    form: &EntryForm,
    choices: &Choices,
    category: Option<Uuid>,
    items: &[Uuid],
    error: Option<&str>,
) -> String {
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let fields = form_fields(form, choices, category, items);
    let body = format!(
        r#"<h1>Modifier — {name_esc}</h1>
{error_html}
<form method="post" action="/budget/{id}/edit">
{fields}
<button type="submit">Enregistrer</button>
</form>
<form method="post" action="/budget/{id}/delete" class="actions">
<button type="submit" class="secondary danger">Supprimer</button>
</form>
<div class="links"><a href="/budget">Retour au budget</a></div>"#,
        name_esc = html_escape(name),
    );
    shell_with_header(Width::Form, "Modifier la dépense", header, &body)
}

/// Fetches an entry, returning it or an early boxed `Response` (404/unavailable).
async fn fetch_entry(
    state: &AppState,
    gid: Uuid,
    entry_id: Uuid,
    cookie: Option<&str>,
) -> Result<BudgetEntryResponse, Box<Response>> {
    match api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/budget-entries/{entry_id}"),
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<BudgetEntryResponse>(resp.body)
                .map_err(|_| Box::new(service_unavailable_page().into_response()))
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            Err(Box::new(entry_not_found_page().into_response()))
        }
        _ => Err(Box::new(service_unavailable_page().into_response())),
    }
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(entry_id): Path<Uuid>,
) -> Response {
    let fam = match family_context(&state, &headers, &me, &format!("/budget/{entry_id}")).await {
        Ok(fam) => fam,
        Err(answer) => return *answer,
    };
    let cookie = budget_cookie(&headers);
    let entry = match fetch_entry(&state, fam.gid, entry_id, cookie.as_deref()).await {
        Ok(e) => e,
        Err(resp) => return *resp,
    };

    if !can_modify(&fam.role, entry.created_by == me.user_id) {
        return forbidden_page().into_response();
    }

    let choices = fetch_choices(&state, fam.gid, cookie.as_deref(), Some(&entry)).await;
    Html(entry_page(&fam.header, &entry, &choices, None)).into_response()
}

/// The edit screen over the entry as stored.
fn entry_page(
    header: &str,
    entry: &BudgetEntryResponse,
    choices: &Choices,
    error: Option<&str>,
) -> String {
    let form = EntryForm {
        name: entry.name.clone(),
        amount: amount_input_value(entry.amount),
        spent_at: entry.spent_at.format("%Y-%m-%d").to_string(),
        ..Default::default()
    };
    let items: Vec<Uuid> = entry.grocery_items.iter().map(|i| i.id).collect();
    page(
        header,
        entry.id,
        &entry.name,
        &form,
        choices,
        entry.category_id,
        &items,
        error,
    )
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(entry_id): Path<Uuid>,
    raw_body: axum::body::Bytes,
) -> Response {
    if !is_form_urlencoded(&headers) {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let fam = match family_context(&state, &headers, &me, &format!("/budget/{entry_id}")).await {
        Ok(fam) => fam,
        Err(answer) => return *answer,
    };
    let cookie = budget_cookie(&headers);
    // An unreadable body is a 422 over the entry as it stands, as the
    // agenda's forms answer one (#98).
    let Ok(form) = serde_urlencoded::from_bytes::<EntryForm>(&raw_body) else {
        let entry = match fetch_entry(&state, fam.gid, entry_id, cookie.as_deref()).await {
            Ok(e) => e,
            Err(resp) => return *resp,
        };
        let choices = fetch_choices(&state, fam.gid, cookie.as_deref(), Some(&entry)).await;
        let html = entry_page(
            &fam.header,
            &entry,
            &choices,
            Some(error_message("invalid_form")),
        );
        return (StatusCode::UNPROCESSABLE_ENTITY, Html(html)).into_response();
    };
    let raw = String::from_utf8_lossy(&raw_body);
    let items = uuids_from_raw_form(&raw, "grocery_item_ids");

    let gid = fam.gid;
    let render_error = |code: &'static str| {
        let (state, form, items, header) = (&state, &form, &items, &fam.header);
        let cookie = cookie.as_deref();
        async move {
            // The entry's own links stay offered on the way back: the form
            // re-reads it rather than assuming what it linked to.
            // What the form had no pills for keeps the entry's value.
            let entry = fetch_entry(state, gid, entry_id, cookie).await.ok();
            let choices = fetch_choices(state, gid, cookie, entry.as_ref()).await;
            let category = form
                .category()
                .unwrap_or_else(|| entry.as_ref().and_then(|e| e.category_id));
            let items: Vec<Uuid> = match (&form.grocery_items_present, &entry) {
                (None, Some(entry)) => entry.grocery_items.iter().map(|i| i.id).collect(),
                _ => items.clone(),
            };
            Html(page(
                header,
                entry_id,
                &form.name,
                form,
                &choices,
                category,
                &items,
                Some(error_message(code)),
            ))
            .into_response()
        }
    };

    let Ok(amount) = parse_amount(&form.amount) else {
        return render_error("amount_must_be_non_negative").await;
    };
    if let Err(e) = validate_entry_form(&form.name, amount) {
        return render_error(e.code()).await;
    }

    let body = update_body(&form, amount, &items);

    let result = api_request_auth(
        &state,
        reqwest::Method::PATCH,
        &format!("/groups/{}/budget-entries/{}", fam.gid, entry_id),
        cookie.as_deref(),
        Some(serde_json::to_value(&body).unwrap()),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            Redirect::to("/budget?notice=entry_updated").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            Redirect::to(&format!("/budget/{entry_id}?error=forbidden")).into_response()
        }
        // The entry, or a category or an item it was given, is gone: the
        // API answers both with a 404. A missing entry shows its page; a
        // reference the form offered is said on the form.
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            match fetch_entry(&state, fam.gid, entry_id, cookie.as_deref()).await {
                Ok(_) => render_error("reference_not_found").await,
                Err(page) => *page,
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::CONFLICT => {
            render_error("grocery_item_already_priced").await
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            render_error(known_code(&resp.body)).await
        }
        Ok(_) | Err(_) => render_error("unavailable").await,
    }
}

pub async fn delete(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(entry_id): Path<Uuid>,
) -> Response {
    let fam = match family_context(&state, &headers, &me, &format!("/budget/{entry_id}")).await {
        Ok(fam) => fam,
        Err(answer) => return *answer,
    };
    let cookie = budget_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &format!("/groups/{}/budget-entries/{}", fam.gid, entry_id),
        cookie.as_deref(),
        None,
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            Redirect::to("/budget?notice=entry_deleted").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            forbidden_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            entry_not_found_page().into_response()
        }
        Ok(_) | Err(_) => service_unavailable_page().into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(raw: &str) -> EntryForm {
        serde_urlencoded::from_str(raw).expect(raw)
    }

    #[test]
    fn undrawn_pills_leave_the_category_and_the_items_alone() {
        let f = form("name=Courses&amount=12&spent_at=2026-10-30&category_id=");
        let body = update_body(&f, 12.0, &[Uuid::from_u128(7)]);
        assert_eq!(body.category_id, None);
        assert_eq!(body.grocery_item_ids, None);
        let json = serde_json::to_value(&body).unwrap();
        assert!(json.get("category_id").is_none(), "{json}");
        assert!(json.get("grocery_item_ids").is_none(), "{json}");
    }

    #[test]
    fn drawn_pills_send_the_category_and_the_items_even_empty() {
        let cat = Uuid::from_u128(3);
        let f = form(&format!(
            "name=Courses&amount=12&spent_at=&category_id={cat}\
             &categories_present=1&grocery_items_present=1"
        ));
        let body = update_body(&f, 12.0, &[Uuid::from_u128(7)]);
        assert_eq!(body.category_id, Some(Some(cat)));
        assert_eq!(body.grocery_item_ids, Some(vec![Uuid::from_u128(7)]));

        // « Sans catégorie » and no item ticked clear both, explicitly.
        let f = form(
            "name=Courses&amount=12&category_id=&categories_present=1&grocery_items_present=1",
        );
        let json = serde_json::to_value(update_body(&f, 12.0, &[])).unwrap();
        assert_eq!(json["category_id"], serde_json::Value::Null, "{json}");
        assert_eq!(json["grocery_item_ids"], serde_json::json!([]), "{json}");
    }

    #[test]
    fn the_name_is_trimmed_and_an_empty_date_is_left_out() {
        let f = form("name=++Courses++&amount=12&spent_at=");
        let body = update_body(&f, 12.0, &[]);
        assert_eq!(body.name.as_deref(), Some("Courses"));
        assert_eq!(body.amount, Some(12.0));
        assert_eq!(body.spent_at, None);
    }
}
