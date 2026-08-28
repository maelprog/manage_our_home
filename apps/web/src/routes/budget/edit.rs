//! `/budget/:id` — a budget entry's edit screen (name / amount / date) with a
//! delete action. Same permission bar as delete (`can_modify`): a non-permitted
//! user never sees the form (GET → forbidden page), and the backend 403 is
//! mapped to `?error=forbidden` on the entry screen defensively. The PATCH has
//! no double-`Option` — `spent_at` is `NOT NULL`, so the form always sends every
//! field and a round trip preserves the date. Success (200) → PRG
//! `/budget?notice=entry_updated`.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::budget::{BudgetEntryResponse, UpdateBudgetEntryRequest};
use manage_our_home_shared::validation::budget::{amount_input_value, validate_entry_form};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::new::{error_message, form_fields, parse_amount, parse_spent_at, EntryForm};
use super::{
    budget_cookie, can_modify, entry_not_found_page, family_context, forbidden_page,
    service_unavailable_page,
};

fn page(header: &str, id: Uuid, name: &str, form: &EntryForm, error: Option<&str>) -> String {
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let fields = form_fields(&form.name, &form.amount, &form.spent_at);
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
    let Some(fam) = family_context(&state, &headers, &me, &format!("/budget/{entry_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = budget_cookie(&headers);
    let entry = match fetch_entry(&state, fam.gid, entry_id, cookie.as_deref()).await {
        Ok(e) => e,
        Err(resp) => return *resp,
    };

    if !can_modify(&fam.role, entry.created_by == me.user_id) {
        return forbidden_page().into_response();
    }

    let form = EntryForm {
        name: entry.name.clone(),
        amount: amount_input_value(entry.amount),
        spent_at: entry.spent_at.format("%Y-%m-%d").to_string(),
    };
    Html(page(&fam.header, entry_id, &entry.name, &form, None)).into_response()
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(entry_id): Path<Uuid>,
    Form(form): Form<EntryForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, &format!("/budget/{entry_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };

    let render_error = |code: &str| {
        Html(page(
            &fam.header,
            entry_id,
            &form.name,
            &form,
            Some(error_message(code)),
        ))
        .into_response()
    };

    let Ok(amount) = parse_amount(&form.amount) else {
        return render_error("amount_must_be_non_negative");
    };
    if let Err(e) = validate_entry_form(&form.name, amount) {
        return render_error(e.code());
    }

    // The edit form always sends every field. `spent_at` has no "clear" state
    // (NOT NULL in the DB): a valid date sets it, an empty/unparseable value
    // leaves it as-is (`None` — omitted from the wire).
    let body = UpdateBudgetEntryRequest {
        name: Some(form.name.trim().to_string()),
        amount: Some(amount),
        spent_at: parse_spent_at(&form.spent_at),
    };

    let cookie = budget_cookie(&headers);
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
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            entry_not_found_page().into_response()
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
    Path(entry_id): Path<Uuid>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, &format!("/budget/{entry_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
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
