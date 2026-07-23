//! `/budget` — the family's grocery-derived spend: a **Résumé mensuel** section
//! (cumulated per month, computed server-side) above the entry list. Any member
//! reads it; per-row a `Modifier` link renders only for entries the caller may
//! modify. Manual entry lives on `/budget/new`; the price-on-checkout path is on
//! the grocery list. PRG banners after create/update/delete and after a
//! price-set on the grocery list. See `docs/front-epic-7-budget.md`.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use manage_our_home_shared::dto::budget::{BudgetEntryList, BudgetEntryResponse, BudgetSummary};
use manage_our_home_shared::validation::budget::{can_modify, format_euros, format_period};

use crate::app::{html_escape, shell};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{budget_cookie, family_context, service_unavailable_page};

#[derive(serde::Deserialize)]
pub struct ListQuery {
    notice: Option<String>,
    error: Option<String>,
}

fn notice_html(notice: Option<&str>) -> String {
    let text = match notice {
        Some("entry_created") => "Dépense ajoutée.",
        Some("entry_updated") => "Dépense mise à jour.",
        Some("entry_deleted") => "Dépense supprimée.",
        _ => return String::new(),
    };
    format!(r#"<p class="notice success">{}</p>"#, html_escape(text))
}

fn error_html(error: Option<&str>) -> String {
    let text = match error {
        Some("name_required") => "Le nom est obligatoire.",
        Some("amount_must_be_non_negative") => "Le montant ne peut pas être négatif.",
        Some("forbidden") => "Vous n'avez pas les droits nécessaires pour cette action.",
        Some("unavailable") => "Service momentanément indisponible, merci de réessayer.",
        _ => return String::new(),
    };
    format!(r#"<p class="notice error">{}</p>"#, html_escape(text))
}

/// Renders the monthly-summary section from the `periods` list (newest first).
fn summary_html(summary: &BudgetSummary) -> String {
    if summary.periods.is_empty() {
        return r#"<p class="muted">Aucune dépense enregistrée pour le moment.</p>"#.to_string();
    }
    let rows = summary
        .periods
        .iter()
        .map(|p| {
            format!(
                r#"<li style="display:flex;justify-content:space-between;gap:0.75rem;padding:0.4rem 0;border-bottom:1px solid var(--border);"><span>{period}</span><strong>{total}</strong></li>"#,
                period = html_escape(&format_period(p.period)),
                total = html_escape(&format_euros(p.total)),
            )
        })
        .collect::<String>();
    format!(r#"<ul style="list-style:none;padding:0;margin:0.5rem 0 0 0;">{rows}</ul>"#)
}

/// Renders one budget entry: date, name, amount, an optional `Courses` badge
/// (set when the entry came from the price-on-checkout path), and — for
/// permitted users — an edit link.
fn entry_row(entry: &BudgetEntryResponse, can_edit: bool) -> String {
    let date = entry.spent_at.format("%d/%m/%Y").to_string();
    let badge_html = if entry.grocery_item_id.is_some() {
        r#" <span style="font-size:0.75rem;padding:0.1rem 0.4rem;border-radius:3px;background:var(--border);">Courses</span>"#
    } else {
        ""
    };
    let edit_link = if can_edit {
        format!(
            r#"<a class="button secondary" href="/budget/{id}">Modifier</a>"#,
            id = entry.id
        )
    } else {
        String::new()
    };
    format!(
        r#"<li style="display:flex;justify-content:space-between;align-items:center;gap:0.75rem;padding:0.6rem 0;border-bottom:1px solid var(--border);">
<span><span class="muted">{date}</span> <strong>{name}</strong>{badge_html} <span class="muted">— {amount}</span></span>
{edit_link}
</li>"#,
        date = html_escape(&date),
        name = html_escape(&entry.name),
        amount = html_escape(&format_euros(entry.amount)),
    )
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/budget").await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = budget_cookie(&headers);

    // Monthly summary (computed server-side). A transport error takes down the
    // whole page; any non-200 renders an empty summary rather than leaking JSON.
    let summary: BudgetSummary = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/budget-entries/summary", fam.gid),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<BudgetSummary>(resp.body).unwrap_or(BudgetSummary {
                periods: Vec::new(),
            })
        }
        Ok(_) => BudgetSummary {
            periods: Vec::new(),
        },
        Err(_) => return service_unavailable_page().into_response(),
    };

    let entries: Vec<BudgetEntryResponse> = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/budget-entries", fam.gid),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<BudgetEntryList>(resp.body)
                .map(|l| l.entries)
                .unwrap_or_default()
        }
        Ok(_) => Vec::new(),
        Err(_) => return service_unavailable_page().into_response(),
    };

    let notice = notice_html(query.notice.as_deref());
    let error = error_html(query.error.as_deref());
    let summary_section = summary_html(&summary);

    let entries_section = if entries.is_empty() {
        r#"<p class="muted">Aucune dépense pour le moment.</p>"#.to_string()
    } else {
        let rows = entries
            .iter()
            .map(|e| entry_row(e, can_modify(&fam.role, e.created_by == me.user_id)))
            .collect::<String>();
        format!(r#"<ul style="list-style:none;padding:0;margin:1rem 0 0 0;">{rows}</ul>"#)
    };

    let body = format!(
        r#"{header}
<div style="display:flex;justify-content:space-between;align-items:center;gap:0.75rem;flex-wrap:wrap;">
<h1 style="margin:0;">Budget</h1>
<a class="button" href="/budget/new">Ajouter une dépense</a>
</div>
<p class="muted">Les dépenses liées aux courses de la famille : saisie manuelle ou prix renseigné sur un article coché de la liste de courses.</p>
{notice}{error}
<h2 style="margin-top:1.5rem;">Résumé mensuel</h2>
<div id="monthly-summary">{summary_section}</div>
<h2 style="margin-top:1.5rem;">Dépenses</h2>
{entries_section}"#,
        header = fam.header,
    );
    Html(shell("Budget", &body)).into_response()
}
