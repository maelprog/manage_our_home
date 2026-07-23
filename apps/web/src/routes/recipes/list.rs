//! `/recipes` — the active family's recipes in two sections: a ranked
//! **Suggestions** view (from the backend's rule-based scorer) and the full
//! alphabetical recipe list. The raw `score` is never shown; each suggestion
//! surfaces the human signals it's built from — a stock summary, a
//! "déjà cuisiné récemment" badge, and its missing required ingredients
//! under the grocery-list marker (the cross-epic generate action is F6, out
//! of scope — see `docs/front-epic-5-recipes.md`). PRG banners after
//! create/delete.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use manage_our_home_shared::dto::recipes::{
    MissingIngredient, RecipeList, RecipeResponse, RecipeSuggestion, SuggestionList,
};
use manage_our_home_shared::validation::recipes::stock_summary;

use crate::app::{html_escape, shell};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{family_context, recipes_cookie, service_unavailable_page};

#[derive(serde::Deserialize)]
pub struct ListQuery {
    notice: Option<String>,
}

fn notice_text(code: &str) -> Option<&'static str> {
    match code {
        "recipe_created" => Some("Recette créée."),
        "recipe_deleted" => Some("Recette supprimée."),
        _ => None,
    }
}

/// Renders a suggestion's missing required ingredients, or nothing when it's
/// fully stocked. The marker states these feed the grocery list (F6).
fn missing_html(missing: &[MissingIngredient]) -> String {
    if missing.is_empty() {
        return String::new();
    }
    let items = missing
        .iter()
        .map(|m| {
            let qty = match (m.quantity, m.unit.as_deref()) {
                (Some(q), Some(u)) => format!(
                    " ({} {})",
                    crate::routes::recipes::detail::fmt_num(q),
                    html_escape(u)
                ),
                (Some(q), None) => format!(" ({})", crate::routes::recipes::detail::fmt_num(q)),
                (None, Some(u)) => format!(" ({})", html_escape(u)),
                (None, None) => String::new(),
            };
            format!("<li>{}{}</li>", html_escape(&m.name), qty)
        })
        .collect::<String>();
    format!(
        r#"<div class="muted" style="margin-top:0.4rem;">Ingrédients manquants — à ajouter à la liste de courses :</div>
<ul style="margin:0.2rem 0 0 0;">{items}</ul>"#,
    )
}

fn suggestion_html(s: &RecipeSuggestion) -> String {
    let summary = html_escape(&stock_summary(
        s.matched_ingredients,
        s.total_required_ingredients,
    ));
    let recent = if s.recently_eaten {
        let when = s
            .last_eaten_on
            .map(|d| format!(" (le {})", d.format("%d/%m/%Y")))
            .unwrap_or_default();
        format!(
            r#" <span style="font-size:0.8rem;padding:0.1rem 0.4rem;border-radius:3px;background:var(--border);">Déjà cuisiné récemment{when}</span>"#,
        )
    } else {
        String::new()
    };
    let missing = missing_html(&s.missing_ingredients);
    format!(
        r#"<li style="padding:0.6rem 0;border-bottom:1px solid var(--border);">
<a href="/recipes/{id}"><strong>{name}</strong></a>{recent}
<div class="muted">{summary}</div>
{missing}
</li>"#,
        id = s.recipe_id,
        name = html_escape(&s.name),
    )
}

fn recipe_row_html(r: &RecipeResponse) -> String {
    let count = r.ingredients.len();
    let count_label = match count {
        0 => "aucun ingrédient".to_string(),
        1 => "1 ingrédient".to_string(),
        n => format!("{n} ingrédients"),
    };
    format!(
        r#"<li style="display:flex;justify-content:space-between;align-items:center;gap:0.75rem;padding:0.6rem 0;border-bottom:1px solid var(--border);">
<a href="/recipes/{id}"><strong>{name}</strong></a>
<span class="muted">{count_label}</span>
</li>"#,
        id = r.id,
        name = html_escape(&r.name),
    )
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/recipes").await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);

    // Suggestions (ranked). A non-200 (e.g. 403, unreachable once family is
    // resolved) renders an empty section rather than leaking JSON; a
    // transport error is the only hard failure.
    let suggestions: Vec<RecipeSuggestion> = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/recipes/suggestions", fam.gid),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<SuggestionList>(resp.body)
                .map(|l| l.suggestions)
                .unwrap_or_default()
        }
        Ok(_) => Vec::new(),
        Err(_) => return service_unavailable_page().into_response(),
    };

    let recipes: Vec<RecipeResponse> = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/recipes", fam.gid),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<RecipeList>(resp.body)
                .map(|l| l.recipes)
                .unwrap_or_default()
        }
        Ok(_) => Vec::new(),
        Err(_) => return service_unavailable_page().into_response(),
    };

    let notice = query
        .notice
        .as_deref()
        .and_then(notice_text)
        .map(|n| format!(r#"<p class="notice success">{}</p>"#, html_escape(n)))
        .unwrap_or_default();

    let suggestions_section = if suggestions.is_empty() {
        String::new()
    } else {
        let rows = suggestions.iter().map(suggestion_html).collect::<String>();
        format!(
            r#"<h2>Suggestions</h2>
<p class="muted">Classées selon ce que vous avez en stock, la variété (repas récents) et la saison.</p>
<ul style="list-style:none;padding:0;margin:0 0 1.5rem 0;">{rows}</ul>"#,
        )
    };

    let list_section = if recipes.is_empty() {
        r#"<p class="muted">Aucune recette pour le moment.</p>"#.to_string()
    } else {
        let rows = recipes.iter().map(recipe_row_html).collect::<String>();
        format!(r#"<ul style="list-style:none;padding:0;margin:0;">{rows}</ul>"#)
    };

    let body = format!(
        r#"{header}
<div style="display:flex;justify-content:space-between;align-items:center;gap:0.75rem;flex-wrap:wrap;">
<h1 style="margin:0;">Recettes</h1>
<a class="button" href="/recipes/new">Nouvelle recette</a>
</div>
{notice}
{suggestions_section}
<h2>Toutes les recettes</h2>
{list_section}"#,
        header = fam.header,
    );
    Html(shell("Recettes", &body)).into_response()
}
