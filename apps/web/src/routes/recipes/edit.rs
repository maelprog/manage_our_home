//! `/recipes/:id/edit` — full edit of a recipe (name/instructions/
//! ingredients). Same permission bar as delete (`can_modify`): a
//! non-permitted user never sees the form (GET → forbidden page), and the
//! backend 403 is mapped to `?error=forbidden` on the detail page
//! defensively. A blank instructions field *clears* it (sent as `Some(None)`
//! per the backend's double-`Option` PATCH contract); the edit form always
//! sends the full ingredient list, replacing the recipe's ingredients.
//! Success (200) → PRG `/recipes/:id?notice=recipe_updated`.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::recipes::{RecipeResponse, UpdateRecipeRequest};
use manage_our_home_shared::validation::recipes::{
    format_ingredients, parse_ingredients, validate_recipe_name,
};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::new::{error_message, form_fields, name_error_code, RecipeForm};
use super::{
    can_modify, family_context, forbidden_page, recipe_not_found_page, recipes_cookie,
    service_unavailable_page,
};

fn page(header: &str, id: Uuid, name: &str, form: &RecipeForm, error: Option<&str>) -> String {
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let fields = form_fields(&form.name, &form.instructions, &form.ingredients);
    let body = format!(
        r#"<h1>Modifier — {name_esc}</h1>
{error_html}
<form method="post" action="/recipes/{id}/edit">
{fields}
<button type="submit">Enregistrer</button>
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
        instructions: recipe.instructions.clone().unwrap_or_default(),
        ingredients: format_ingredients(&recipe.ingredients),
    };
    Html(page(&fam.header, recipe_id, &recipe.name, &form, None)).into_response()
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
    Form(form): Form<RecipeForm>,
) -> Response {
    let Some(fam) =
        family_context(&state, &headers, &me, &format!("/recipes/{recipe_id}/edit")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };

    let render_error = |code: &str| {
        Html(page(
            &fam.header,
            recipe_id,
            &form.name,
            &form,
            Some(error_message(code)),
        ))
        .into_response()
    };

    if let Err(e) = validate_recipe_name(&form.name) {
        return render_error(name_error_code(e));
    }
    let ingredients = match parse_ingredients(&form.ingredients) {
        Ok(list) => list,
        Err(e) => return render_error(e.code()),
    };

    // A blank instructions field clears it (`Some(None)`); the edit form
    // always sends every field, so each is `Some(...)`.
    let instructions = {
        let i = form.instructions.trim();
        Some((!i.is_empty()).then(|| i.to_string()))
    };
    let body = UpdateRecipeRequest {
        name: Some(form.name.trim().to_string()),
        instructions,
        ingredients: Some(ingredients),
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
            render_error(code)
        }
        Ok(_) | Err(_) => render_error("unavailable"),
    }
}
