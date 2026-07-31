//! `/recipes/:id` — recipe detail: ingredients (with optional/seasonal
//! markers), instructions, the last-cooked state, plus the log-a-meal action
//! and the delete action; full edit lives in `edit.rs`.
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

use super::{
    can_modify, family_context, forbidden_page, recipe_not_found_page, recipes_cookie,
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
        _ => None,
    }
}

fn error_text(code: &str) -> Option<&'static str> {
    match code {
        "forbidden" => Some("Vous n'avez pas les droits nécessaires pour cette action."),
        "unavailable" => Some("Service momentanément indisponible, merci de réessayer."),
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
    let can_edit = can_modify(&fam.role, recipe.created_by == me.user_id);
    let notice = query.notice.as_deref().and_then(notice_text);
    let error = query.error.as_deref().and_then(error_text);
    Html(page(&fam.header, &recipe, last, can_edit, notice, error)).into_response()
}

fn page(
    header: &str,
    recipe: &RecipeResponse,
    last_eaten: Option<NaiveDate>,
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
    // the date defaults to today and can be backdated.
    let today = chrono::Utc::now()
        .date_naive()
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

    let body = format!(
        r#"<h1>{name}</h1>
{notice_html}{error_html}
<h2>Ingrédients</h2>
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
