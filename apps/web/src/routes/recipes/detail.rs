//! `/recipes/:id` — recipe detail: ingredients (with optional/seasonal
//! markers), instructions, the last-cooked state, plus the log-a-meal action
//! and the delete action; full edit lives in `edit.rs`. The « À réaliser » /
//! « Retirer » forms (#470) plan the recipe for the next grocery list, open
//! to any member like the selection API.
//!
//! Permission bar: the log-a-meal form renders for **any** family member
//! (feeding the shared variety history); the edit link and delete button
//! render only for the recipe's creator or a group admin/owner
//! (`can_modify`). A standard member viewing another member's recipe still
//! gets the log form, plus a muted note that editing/deleting is reserved.
//! The backend stays the authority — a forged edit/DELETE/log is 403'd and
//! mapped defensively here.
//!
//! Error table: 404 (`get_recipe`/`delete_recipe`/`log_meal`) unknown/foreign
//! recipe → introuvable page; 403 (`delete`/`log` permission bar) →
//! `?error=forbidden` / forbidden page.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use chrono::NaiveDate;
use manage_our_home_shared::dto::recipes::{
    IngredientResponse, LogMealRequest, MealHistoryList, RecipeResponse,
};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use manage_our_home_shared::validation::recipes::servings_label;

use super::{
    can_modify, deselect_form, family_context, forbidden_page, outcome_links, planned_badge,
    planned_selections, recipe_not_found_page, recipes_cookie, select_form,
    service_unavailable_page,
};

/// Formats an `f64` quantity for display without a trailing `.0` on whole
/// numbers (`2.0` → `"2"`). Shared with the list page's missing-ingredient
/// rendering.
pub(crate) fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// The site an imported recipe comes from (#405), as the page shows it:
/// the host of an `http`/`https` URL, without a leading `www.`.
pub(crate) fn source_domain(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str().filter(|h| !h.is_empty())?;
    Some(host.strip_prefix("www.").unwrap_or(host).to_string())
}

/// The link to the page a recipe was imported from, named by its site.
/// Nothing for a recipe typed by hand, or for an address that is not a web
/// page (the API refuses one; this is not where a `javascript:` link gets
/// a second chance).
fn source_html(source_url: Option<&str>) -> String {
    let Some((url, domain)) = source_url.and_then(|u| Some((u.trim(), source_domain(u)?))) else {
        return String::new();
    };
    format!(
        r#"<p class="muted">Source : <a href="{url}" rel="noopener noreferrer nofollow">{domain}</a></p>"#,
        url = html_escape(url),
        domain = html_escape(&domain),
    )
}

#[derive(serde::Deserialize)]
pub struct DetailQuery {
    notice: Option<String>,
    error: Option<String>,
}

fn notice_text(code: &str) -> Option<&'static str> {
    match code {
        "recipe_created" => Some("Recette créée."),
        "recipe_updated" => Some("Recette mise à jour."),
        "meal_logged" => Some("Repas enregistré."),
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
        "forbidden" => Some("Vous n'avez pas les droits nécessaires pour cette action."),
        "unavailable" => Some("Service momentanément indisponible, merci de réessayer."),
        "invalid_servings" => Some("Le nombre de personnes doit être un nombre entier de 1 à 50."),
        _ => None,
    }
}

/// One ingredient → a list item with its quantity/unit and optional/seasonal
/// markers.
fn ingredient_html(ing: &IngredientResponse) -> String {
    let qty = match (ing.quantity, ing.unit.as_deref()) {
        (Some(q), Some(u)) => format!(" — {} {}", fmt_num(q), html_escape(u)),
        (Some(q), None) => format!(" — {}", fmt_num(q)),
        (None, Some(u)) => format!(" — {}", html_escape(u)),
        (None, None) => String::new(),
    };
    let optional = if ing.is_optional {
        r#" <span class="muted">· optionnel</span>"#.to_string()
    } else {
        String::new()
    };
    let seasonal = ing
        .seasonal_months
        .as_ref()
        .filter(|m| !m.is_empty())
        .map(|m| {
            let months = m.iter().map(i32::to_string).collect::<Vec<_>>().join(", ");
            format!(
                r#" <span class="muted">· saison : {}</span>"#,
                html_escape(&months)
            )
        })
        .unwrap_or_default();
    format!(
        "<li><strong>{name}</strong>{qty}{optional}{seasonal}</li>",
        name = html_escape(&ing.name),
    )
}

/// Fetches this recipe's most recent `eaten_on`, if any, from the family
/// meal history (a wide `days_back` so "jamais cuisiné" is meaningful).
async fn last_eaten(
    state: &AppState,
    gid: Uuid,
    recipe_id: Uuid,
    cookie: Option<&str>,
) -> Option<NaiveDate> {
    let resp = api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/recipes/meal-history?days_back=3650"),
        cookie,
        None,
    )
    .await
    .ok()?;
    if resp.status != reqwest::StatusCode::OK {
        return None;
    }
    let list = serde_json::from_value::<MealHistoryList>(resp.body).ok()?;
    list.entries
        .into_iter()
        .filter(|e| e.recipe_id == recipe_id)
        .map(|e| e.eaten_on)
        .max()
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
    Query(query): Query<DetailQuery>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, &format!("/recipes/{recipe_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);

    let recipe = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/recipes/{}", fam.gid, recipe_id),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match serde_json::from_value::<RecipeResponse>(resp.body) {
                Ok(r) => r,
                Err(_) => return service_unavailable_page().into_response(),
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return recipe_not_found_page().into_response()
        }
        _ => return service_unavailable_page().into_response(),
    };

    let last = last_eaten(&state, fam.gid, recipe_id, cookie.as_deref()).await;
    let planned = planned_selections(&state, fam.gid, cookie.as_deref())
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|s| s.recipe_id == recipe_id)
        .map(|s| s.servings);
    let can_edit = can_modify(&fam.role, recipe.created_by == me.user_id);
    let notice = query.notice.as_deref().and_then(notice_text);
    let error = query.error.as_deref().and_then(error_text);
    Html(page(
        &fam.header,
        &recipe,
        last,
        planned,
        can_edit,
        notice,
        error,
    ))
    .into_response()
}

/// The recipe's place in the next shopping (#470): « À réaliser » with the
/// number of people when it is not planned, the badge and « Retirer » when
/// it is. `planned` is the selection's number of people, `Some(None)` for a
/// selection that does not say.
fn selection_html(id: Uuid, base: Option<i32>, planned: Option<Option<i32>>) -> String {
    match planned {
        Some(servings) => format!(
            r#"<div class="notice split">
<span>{badge} Cette recette est prise en compte par la <a href="/grocery-list">liste de courses</a>.</span>
<span class="actions">{outcome}
{deselect}</span>
</div>"#,
            badge = planned_badge(servings),
            outcome = outcome_links(id, "detail"),
            deselect = deselect_form(id, "detail"),
        ),
        None => select_form(id, base, "detail", "card inline"),
    }
}

fn page(
    header: &str,
    recipe: &RecipeResponse,
    last_eaten: Option<NaiveDate>,
    planned: Option<Option<i32>>,
    can_edit: bool,
    notice: Option<&str>,
    error: Option<&str>,
) -> String {
    let id = recipe.id;
    let notice_html = notice
        .map(|n| format!(r#"<p class="notice success">{}</p>"#, html_escape(n)))
        .unwrap_or_default();
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();

    let ingredients_html = if recipe.ingredients.is_empty() {
        r#"<p class="muted">Aucun ingrédient renseigné.</p>"#.to_string()
    } else {
        let items = recipe
            .ingredients
            .iter()
            .map(ingredient_html)
            .collect::<String>();
        format!("<ul>{items}</ul>")
    };

    let instructions_html = match recipe.instructions.as_deref().map(str::trim) {
        Some(i) if !i.is_empty() => format!(
            "<h2>Instructions</h2><p class=\"multiline\">{}</p>",
            html_escape(i)
        ),
        _ => String::new(),
    };

    let last_eaten_html = match last_eaten {
        Some(d) => format!(
            r#"<p class="muted">Dernier repas cuisiné : le {}.</p>"#,
            d.format("%d/%m/%Y")
        ),
        None => r#"<p class="muted">Ce plat n'a pas encore été cuisiné.</p>"#.to_string(),
    };

    // Log-a-meal is open to any member (feeds the shared variety history);
    // the date defaults to today in Paris, the day the pages judge dates
    // in, and can be backdated. A field left empty is dated by apps/api,
    // on the same Paris day (#608).
    let today = crate::routes::agenda::today_paris()
        .format("%Y-%m-%d")
        .to_string();
    let log_form = format!(
        r#"<form method="post" action="/recipes/{id}/log" class="card inline">
<label>Date du repas
<input type="date" name="eaten_on" value="{today}"/></label>
<button type="submit">Logger ce repas</button>
</form>"#,
    );

    let edit_delete_html = if can_edit {
        format!(
            r#"<div class="actions">
<a class="btn secondary" href="/recipes/{id}/edit">Modifier la recette</a>
<form method="post" action="/recipes/{id}/delete">
<button type="submit" class="secondary danger">Supprimer</button>
</form>
</div>"#
        )
    } else {
        r#"<p class="muted">Seul le créateur ou un administrateur peut modifier ou supprimer cette recette.</p>"#.to_string()
    };

    let source_html = source_html(recipe.source_url.as_deref());
    let servings_html = recipe
        .servings
        .map(|s| format!(r#"<p class="muted">Pour {}.</p>"#, servings_label(s)))
        .unwrap_or_default();
    let selection_html = selection_html(id, recipe.servings, planned);
    let body = format!(
        r#"<h1>{name}</h1>
{source_html}
{notice_html}{error_html}
{selection_html}
<h2>Ingrédients</h2>
{servings_html}
{ingredients_html}
{instructions_html}
{last_eaten_html}
{log_form}
{edit_delete_html}
<div class="links"><a href="/recipes">Retour aux recettes</a></div>"#,
        name = html_escape(&recipe.name),
    );
    shell_with_header(Width::Read, &recipe.name, header, &body)
}

// -- mutations --------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct LogForm {
    #[serde(default)]
    eaten_on: String,
}

pub async fn log(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
    Form(form): Form<LogForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, &format!("/recipes/{recipe_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let detail = format!("/recipes/{recipe_id}");

    // Empty date → omit it (backend defaults to today); an unparseable value
    // is treated as "today" too rather than erroring the family out.
    let eaten_on = NaiveDate::parse_from_str(form.eaten_on.trim(), "%Y-%m-%d").ok();
    let body = LogMealRequest { eaten_on };

    let cookie = recipes_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/recipes/{}/meal-history", fam.gid, recipe_id),
        cookie.as_deref(),
        Some(serde_json::to_value(&body).unwrap()),
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            format!("{detail}?notice=meal_logged")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return recipe_not_found_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{detail}?error=forbidden")
        }
        Ok(_) | Err(_) => format!("{detail}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}

pub async fn delete(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, &format!("/recipes/{recipe_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &format!("/groups/{}/recipes/{}", fam.gid, recipe_id),
        cookie.as_deref(),
        None,
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            Redirect::to("/recipes?notice=recipe_deleted").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            forbidden_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            recipe_not_found_page().into_response()
        }
        Ok(_) | Err(_) => service_unavailable_page().into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_domain_is_the_host_without_www() {
        assert_eq!(
            source_domain("https://www.marmiton.org/recettes/recette_crepes_27121.aspx"),
            Some("marmiton.org".into())
        );
        assert_eq!(
            source_domain("http://cuisine.journaldesfemmes.fr/recette/1"),
            Some("cuisine.journaldesfemmes.fr".into())
        );
        assert_eq!(
            source_domain("https://750g.com:8443/r"),
            Some("750g.com".into())
        );
        assert_eq!(source_domain("javascript:alert(1)"), None);
        assert_eq!(source_domain("pas une url"), None);
    }

    #[test]
    fn source_link_is_rel_noopener_noreferrer_nofollow() {
        let html = source_html(Some("https://www.750g.com/bento-r78248.htm?a=1&b=2"));
        assert!(
            html.contains(
                r#"<a href="https://www.750g.com/bento-r78248.htm?a=1&amp;b=2" rel="noopener noreferrer nofollow">750g.com</a>"#
            ),
            "{html}"
        );
    }

    #[test]
    fn no_source_link_without_a_web_address() {
        assert_eq!(source_html(None), "");
        assert_eq!(source_html(Some("javascript:alert(1)")), "");
        assert_eq!(source_html(Some("data:text/html,x")), "");
    }

    #[test]
    fn an_unplanned_recipe_offers_a_realiser_for_its_base() {
        let id = Uuid::from_u128(1);
        let html = selection_html(id, Some(4), None);
        assert!(
            html.contains(&format!(
                r#"action="/recipes/{id}/select" class="card inline""#
            )),
            "{html}"
        );
        assert!(html.contains(r#"name="back" value="detail""#), "{html}");
        assert!(html.contains(r#"value="4""#), "{html}");
    }

    #[test]
    fn a_planned_recipe_shows_its_people_and_retirer() {
        let id = Uuid::from_u128(1);
        let html = selection_html(id, Some(4), Some(Some(6)));
        assert!(html.contains("À réaliser · 6 personnes"), "{html}");
        assert!(html.contains(r#"href="/grocery-list""#), "{html}");
        assert!(
            html.contains(&format!(r#"action="/recipes/{id}/deselect""#)),
            "{html}"
        );
        // Faite and Abandonner (#474), back to the recipe.
        assert!(
            html.contains(&format!(r#"href="/recipes/{id}/done?back=detail""#)),
            "{html}"
        );
        assert!(
            html.contains(&format!(r#"href="/recipes/{id}/abandon?back=detail""#)),
            "{html}"
        );
        let html = selection_html(id, None, Some(None));
        assert!(
            html.contains(r#"<span class="badge">À réaliser</span>"#),
            "{html}"
        );
    }
}
