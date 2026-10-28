//! `/recipes` — the active family's recipes in two sections: a ranked
//! **Suggestions** view (from the backend's rule-based scorer) and the full
//! alphabetical recipe list. The raw `score` is never shown; each suggestion
//! surfaces the human signals it's built from — a stock summary, a
//! "déjà cuisiné récemment" badge, and its missing required ingredients
//! under the grocery-list marker (the cross-epic generate action is F6, out
//! of scope — see `docs/front-epic-5-recipes.md`). PRG banners after
//! create/delete.
//!
//! Since #470 the page opens on the recipes « À réaliser », the ones the
//! grocery list is generated from, and each row of the full list carries
//! the « À réaliser » / « Retirer » form.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use manage_our_home_shared::dto::recipes::{
    MissingIngredient, RecipeList, RecipeResponse, RecipeSelectionResponse, RecipeSuggestion,
    SuggestionList,
};
use manage_our_home_shared::validation::recipes::{servings_label, stock_summary};

use crate::app::{html_escape, kitchen_segment, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::routes::stocks::new::OFF_ATTRIBUTION;
use crate::state::{api_request_auth, AppState};

use super::{
    deselect_form, family_context, outcome_links, planned_badge, planned_selections,
    recipes_cookie, select_form, service_unavailable_page,
};

#[derive(serde::Deserialize)]
pub struct ListQuery {
    notice: Option<String>,
    error: Option<String>,
}

fn notice_text(code: &str) -> Option<&'static str> {
    match code {
        "recipe_created" => Some("Recette créée."),
        "recipe_deleted" => Some("Recette supprimée."),
        "recipe_selected" => Some("Recette à réaliser pour les prochaines courses."),
        "recipe_deselected" => Some("Recette retirée des recettes à réaliser."),
        "recipe_done" => Some(super::outcome::DONE_NOTICE),
        "recipe_abandoned" => Some(super::outcome::ABANDONED_NOTICE),
        _ => None,
    }
}

fn error_text(code: &str) -> Option<&'static str> {
    match code {
        "selection_gone" => Some(super::outcome::SELECTION_GONE),
        "invalid_servings" => Some("Le nombre de personnes doit être un nombre entier de 1 à 50."),
        "forbidden" => Some("Vous n'avez pas les droits nécessaires pour cette action."),
        "unavailable" => Some("Service momentanément indisponible, merci de réessayer."),
        _ => None,
    }
}

/// The recipes « À réaliser » (#470), at the top of the page: the ones the
/// next grocery list is generated from, with their number of people. The
/// heading's `id` is what the grocery list links to when there are none.
fn planned_section(planned: &[RecipeSelectionResponse]) -> String {
    let summary = if planned.is_empty() {
        r#"<p class="muted">Aucune recette à réaliser. Le bouton « À réaliser » d'une recette la fait entrer dans la prochaine génération de la liste de courses.</p>"#.to_string()
    } else {
        let names = planned
            .iter()
            .map(|s| {
                let people = s
                    .servings
                    .map(|n| format!(" ({})", servings_label(n)))
                    .unwrap_or_default();
                format!(
                    r#"<a href="/recipes/{id}">{name}</a>{people}"#,
                    id = s.recipe_id,
                    name = html_escape(&s.recipe_name),
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let count = match planned.len() {
            1 => "1 recette prévue".to_string(),
            n => format!("{n} recettes prévues"),
        };
        format!(r#"<p>{count} pour la <a href="/grocery-list">liste de courses</a> : {names}.</p>"#)
    };
    format!(
        r#"<h2 id="a-realiser">À réaliser</h2>
{summary}"#
    )
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
        r#"<div class="muted">Ingrédients manquants — à ajouter à la liste de courses :</div>
<ul>{items}</ul>"#,
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
        format!(r#" <span class="badge">Déjà cuisiné récemment{when}</span>"#,)
    } else {
        String::new()
    };
    let missing = missing_html(&s.missing_ingredients);
    format!(
        r#"<li class="list-row stacked">
<a href="/recipes/{id}"><strong>{name}</strong></a>{recent}
<div class="muted">{summary}</div>
{missing}
</li>"#,
        id = s.recipe_id,
        name = html_escape(&s.name),
    )
}

/// One recipe of the full list. `planned` is its selection's number of
/// people when it is « À réaliser » (`Some(None)`: the selection does not
/// say), `None` when it is not.
fn recipe_row_html(r: &RecipeResponse, planned: Option<Option<i32>>) -> String {
    let count = r.ingredients.len();
    let count_label = match count {
        0 => "aucun ingrédient".to_string(),
        1 => "1 ingrédient".to_string(),
        n => format!("{n} ingrédients"),
    };
    let selection = match planned {
        Some(servings) => format!(
            r#"<span class="actions">{badge}{outcome}{deselect}</span>"#,
            badge = planned_badge(servings),
            outcome = outcome_links(r.id, "list"),
            deselect = deselect_form(r.id, "list"),
        ),
        None => select_form(r.id, r.servings, "list", "actions"),
    };
    format!(
        r#"<li class="list-row">
<a href="/recipes/{id}"><strong>{name}</strong></a>
<span class="muted">{count_label}</span>
{selection}
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

    let Ok(planned) = planned_selections(&state, fam.gid, cookie.as_deref()).await else {
        return service_unavailable_page().into_response();
    };

    let notice = query
        .notice
        .as_deref()
        .and_then(notice_text)
        .map(|n| format!(r#"<p class="notice success">{}</p>"#, html_escape(n)))
        .unwrap_or_default();
    let error = query
        .error
        .as_deref()
        .and_then(error_text)
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();

    let suggestions_section = if suggestions.is_empty() {
        String::new()
    } else {
        let rows = suggestions.iter().map(suggestion_html).collect::<String>();
        format!(
            r#"<h2>Suggestions</h2>
<p class="muted">Classées selon ce que vous avez en stock, la variété (repas récents) et la saison.</p>
<ul class="list">{rows}</ul>
{OFF_ATTRIBUTION}"#,
        )
    };

    let list_section = if recipes.is_empty() {
        r#"<p class="muted">Aucune recette pour le moment.</p>"#.to_string()
    } else {
        let rows = recipes
            .iter()
            .map(|r| {
                let selection = planned.iter().find(|s| s.recipe_id == r.id);
                recipe_row_html(r, selection.map(|s| s.servings))
            })
            .collect::<String>();
        format!(r#"<ul class="list">{rows}</ul>"#)
    };

    let body = format!(
        r#"{segment}<div class="page-header">
<h1>Recettes</h1>
<div class="actions">
<a class="btn secondary" href="/recipes/import">Importer depuis une URL</a>
<a class="btn" href="/recipes/new">Nouvelle recette</a>
</div>
</div>
{notice}{error}
{planned_section}
{suggestions_section}
<h2>Toutes les recettes</h2>
{list_section}"#,
        segment = kitchen_segment("/recipes"),
        planned_section = planned_section(&planned),
    );
    Html(shell_with_header(
        Width::Full,
        "Recettes",
        &fam.header,
        &body,
    ))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn recipe(id: u128, name: &str, servings: Option<i32>) -> RecipeResponse {
        RecipeResponse {
            id: Uuid::from_u128(id),
            group_id: Uuid::from_u128(100),
            created_by: Uuid::from_u128(200),
            name: name.into(),
            instructions: None,
            ingredients: Vec::new(),
            source_url: None,
            servings,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn selection(recipe_id: u128, name: &str, servings: Option<i32>) -> RecipeSelectionResponse {
        RecipeSelectionResponse {
            id: Uuid::from_u128(recipe_id + 1000),
            recipe_id: Uuid::from_u128(recipe_id),
            recipe_name: name.into(),
            status: "planned".into(),
            servings,
            created_by: Uuid::from_u128(200),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn a_recipe_not_planned_offers_a_realiser_with_its_base() {
        let html = recipe_row_html(&recipe(1, "Crêpes", Some(4)), None);
        let id = Uuid::from_u128(1);
        assert!(
            html.contains(&format!(r#"action="/recipes/{id}/select""#)),
            "{html}"
        );
        assert!(html.contains(r#"name="back" value="list""#), "{html}");
        assert!(html.contains(r#"value="4""#), "{html}");
        assert!(!html.contains("Retirer"), "{html}");
        assert!(!html.contains(r#"class="badge""#), "{html}");
    }

    #[test]
    fn a_planned_recipe_shows_its_badge_and_retirer() {
        let html = recipe_row_html(&recipe(1, "Crêpes", Some(4)), Some(Some(6)));
        let id = Uuid::from_u128(1);
        assert!(html.contains("À réaliser · 6 personnes"), "{html}");
        assert!(
            html.contains(&format!(r#"action="/recipes/{id}/deselect""#)),
            "{html}"
        );
        assert!(!html.contains("/select\""), "{html}");
        // Faite and Abandonner (#474), back to the list.
        assert!(
            html.contains(&format!(r#"href="/recipes/{id}/done?back=list""#)),
            "{html}"
        );
        assert!(
            html.contains(&format!(r#"href="/recipes/{id}/abandon?back=list""#)),
            "{html}"
        );
    }

    #[test]
    fn the_planned_section_names_the_recipes_and_their_people() {
        let html = planned_section(&[
            selection(1, "Crêpes", Some(6)),
            selection(2, "Soupe <du jour>", None),
        ]);
        assert!(
            html.contains(r#"<h2 id="a-realiser">À réaliser</h2>"#),
            "{html}"
        );
        assert!(html.contains("2 recettes prévues"), "{html}");
        assert!(
            html.contains(&format!(
                r#"<a href="/recipes/{}">Crêpes</a> (6 personnes)"#,
                Uuid::from_u128(1)
            )),
            "{html}"
        );
        assert!(html.contains("Soupe &lt;du jour&gt;</a>."), "{html}");
        assert!(planned_section(&[selection(1, "Crêpes", None)]).contains("1 recette prévue pour"),);
    }

    #[test]
    fn an_empty_planned_section_says_how_to_plan_one() {
        let html = planned_section(&[]);
        assert!(html.contains(r#"id="a-realiser""#), "{html}");
        assert!(html.contains("Aucune recette à réaliser."), "{html}");
    }
}
