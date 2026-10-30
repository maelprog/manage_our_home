//! `/recipes/:id/edit` — full edit of a recipe, in the form of
//! `new.rs` (#539): its lines and steps pre-filled, blank ones under them. Same permission bar as delete (`can_modify`): a
//! non-permitted user never sees the form (GET → forbidden page), and the
//! backend 403 is mapped to `?error=forbidden` on the detail page
//! defensively. No step at all *clears* the instructions (sent as `Some(None)`
//! per the backend's double-`Option` PATCH contract); the edit form always
//! sends the full ingredient list, replacing the recipe's ingredients.
//! Success (200) → PRG `/recipes/:id?notice=recipe_updated`.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::recipes::{RecipeResponse, UpdateRecipeRequest};
use manage_our_home_shared::validation::recipes::{ingredient_fields, steps_of};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::new::{check, error_message, form_buttons, form_fields, read_form, Intent, RecipeForm};
use super::{
    can_modify, family_context, forbidden_page, recipe_not_found_page, recipes_cookie,
    service_unavailable_page,
};

fn page(header: &str, id: Uuid, name: &str, form: &RecipeForm, error: Option<&str>) -> String {
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let fields = form_fields(form);
    let buttons = form_buttons("Enregistrer");
    let body = format!(
        r#"<h1>Modifier — {name_esc}</h1>
{error_html}
<form method="post" action="/recipes/{id}/edit">
{fields}
{buttons}
</form>
<div class="links"><a href="/recipes/{id}">Retour au détail</a></div>"#,
        name_esc = html_escape(name),
    );
    shell_with_header(Width::Form, "Modifier la recette", header, &body)
}

/// Fetches a recipe, returning it or an early boxed `Response` (404/unavailable).
async fn fetch_recipe(
    state: &AppState,
    gid: Uuid,
    recipe_id: Uuid,
    cookie: Option<&str>,
) -> Result<RecipeResponse, Box<Response>> {
    match api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/recipes/{recipe_id}"),
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<RecipeResponse>(resp.body)
                .map_err(|_| Box::new(service_unavailable_page().into_response()))
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            Err(Box::new(recipe_not_found_page().into_response()))
        }
        _ => Err(Box::new(service_unavailable_page().into_response())),
    }
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
) -> Response {
    let Some(fam) =
        family_context(&state, &headers, &me, &format!("/recipes/{recipe_id}/edit")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);
    let recipe = match fetch_recipe(&state, fam.gid, recipe_id, cookie.as_deref()).await {
        Ok(r) => r,
        Err(resp) => return *resp,
    };

    if !can_modify(&fam.role, recipe.created_by == me.user_id) {
        return forbidden_page().into_response();
    }

    let form = RecipeForm {
        name: recipe.name.clone(),
        servings: recipe.servings.map(|s| s.to_string()).unwrap_or_default(),
        duration: recipe
            .duration_minutes
            .map(|d| d.to_string())
            .unwrap_or_default(),
        source_url: String::new(),
        ingredients: ingredient_fields(&recipe.ingredients),
        steps: steps_of(recipe.instructions.as_deref().unwrap_or_default()),
    };
    Html(page(&fam.header, recipe_id, &recipe.name, &form, None)).into_response()
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let Some(fam) =
        family_context(&state, &headers, &me, &format!("/recipes/{recipe_id}/edit")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };

    let (mut form, intent) = read_form(&pairs);
    if intent == Intent::MoreLines {
        return Html(page(&fam.header, recipe_id, &form.name, &form, None)).into_response();
    }
    let checked = check(&form);
    form.drop_blanks();
    let render_error = |message: &str| {
        Html(page(
            &fam.header,
            recipe_id,
            &form.name,
            &form,
            Some(message),
        ))
        .into_response()
    };
    let checked = match checked {
        Ok(c) => c,
        Err(message) => return render_error(&message),
    };

    // The edit form always sends every field, so each is `Some(...)`: a
    // blank number of people, time, or no step at all clears it.
    let body = UpdateRecipeRequest {
        name: Some(form.name.trim().to_string()),
        instructions: Some(checked.instructions),
        ingredients: Some(checked.ingredients),
        servings: Some(checked.servings),
        duration_minutes: Some(checked.duration_minutes),
    };

    let cookie = recipes_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::PATCH,
        &format!("/groups/{}/recipes/{}", fam.gid, recipe_id),
        cookie.as_deref(),
        Some(serde_json::to_value(&body).unwrap()),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            Redirect::to(&format!("/recipes/{recipe_id}?notice=recipe_updated")).into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            Redirect::to(&format!("/recipes/{recipe_id}?error=forbidden")).into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            recipe_not_found_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            let code = resp
                .body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unavailable");
            render_error(error_message(code))
        }
        Ok(_) | Err(_) => render_error(error_message("unavailable")),
    }
}
