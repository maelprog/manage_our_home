//! `/budget/new` — manually add a price entry (name, amount in euros, optional
//! date). Budget stays grocery-derived, so this form deliberately carries no
//! category/payee and never sends a `grocery_item_id` (that link is only made
//! by the price-on-checkout path on the grocery list). The empty-name /
//! negative-amount rules are pre-validated by the shared `validate_entry_form`
//! (inline error, no round trip); the backend's matching 400s are mapped
//! defensively. Any member may create, so 403 isn't reachable via the UI.
//! Success (201) → PRG `/budget?notice=entry_created`.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use chrono::NaiveDate;
use manage_our_home_shared::dto::budget::CreateBudgetEntryRequest;
use manage_our_home_shared::validation::budget::validate_entry_form;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{budget_cookie, family_context, forbidden_page};

/// The shared entry form fields, used by both create and edit. `amount` and
/// `spent_at` arrive as strings and are parsed in the handler.
#[derive(serde::Deserialize, Default)]
pub struct EntryForm {
    pub name: String,
    #[serde(default)]
    pub amount: String,
    #[serde(default)]
    pub spent_at: String,
}

/// French copy for a budget-entry form error code.
pub(crate) fn error_message(code: &str) -> &'static str {
    match code {
        "name_required" => "Le nom est obligatoire.",
        "amount_must_be_non_negative" => "Le montant ne peut pas être négatif.",
        "grocery_item_already_priced" => "Cet article a déjà un prix enregistré.",
        "unavailable" => "Service momentanément indisponible, merci de réessayer.",
        _ => "Une erreur est survenue, merci de réessayer.",
    }
}

/// Parses a required euro amount string to an `f64`. An empty or unparseable
/// value maps to `Err(())`, which the handler surfaces as
/// `amount_must_be_non_negative` (the same code the backend's `to_cents`
/// rejects a bad amount with). The `>= 0`/finite check lives in the shared
/// `validate_entry_form`.
pub(crate) fn parse_amount(s: &str) -> Result<f64, ()> {
    s.trim().parse::<f64>().map_err(|_| ())
}

/// Parses an optional date string (`YYYY-MM-DD` from an `<input type=date>`):
/// empty → `None` (backend defaults to today), a valid date → `Some`, anything
/// unparseable → `None` (let the backend default rather than 400 on a value the
/// date picker shouldn't have produced).
pub(crate) fn parse_spent_at(s: &str) -> Option<NaiveDate> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        NaiveDate::parse_from_str(t, "%Y-%m-%d").ok()
    }
}

/// Renders the entry form fields (shared markup, pre-filled). Used by create
/// (today's date, empty name/amount) and edit (existing values).
pub(crate) fn form_fields(name: &str, amount: &str, spent_at: &str) -> String {
    format!(
        r#"<label>Nom <input type="text" name="name" required value="{name}" placeholder="ex. Courses Super U"/></label>
<label>Montant (€) <input type="number" name="amount" step="0.01" min="0" required value="{amount}"/></label>
<label>Date <input type="date" name="spent_at" value="{spent_at}"/></label>"#,
        name = html_escape(name),
        amount = html_escape(amount),
        spent_at = html_escape(spent_at),
    )
}

fn page(header: &str, form: &EntryForm, error: Option<&str>) -> String {
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let fields = form_fields(&form.name, &form.amount, &form.spent_at);
    let body = format!(
        r#"<h1>Nouvelle dépense</h1>
{error_html}
<form method="post" action="/budget/new">
{fields}
<button type="submit">Ajouter la dépense</button>
</form>
<div class="links"><a href="/budget">Retour au budget</a></div>"#,
    );
    shell_with_header(Width::Form, "Nouvelle dépense", header, &body)
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/budget/new").await else {
        return Redirect::to("/groups/new").into_response();
    };
    // Pre-fill the date with today for convenience (still optional server-side).
    let form = EntryForm {
        spent_at: chrono::Utc::now()
            .date_naive()
            .format("%Y-%m-%d")
            .to_string(),
        ..Default::default()
    };
    Html(page(&fam.header, &form, None)).into_response()
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<EntryForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/budget/new").await else {
        return Redirect::to("/groups/new").into_response();
    };

    let render_error =
        |code: &str| Html(page(&fam.header, &form, Some(error_message(code)))).into_response();

    let Ok(amount) = parse_amount(&form.amount) else {
        return render_error("amount_must_be_non_negative");
    };
    if let Err(e) = validate_entry_form(&form.name, amount) {
        return render_error(e.code());
    }

    let req = CreateBudgetEntryRequest {
        name: form.name.trim().to_string(),
        amount,
        spent_at: parse_spent_at(&form.spent_at),
        grocery_item_id: None,
    };

    let cookie = budget_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/budget-entries", fam.gid),
        cookie.as_deref(),
        Some(serde_json::to_value(&req).unwrap()),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            Redirect::to("/budget?notice=entry_created").into_response()
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
