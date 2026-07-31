//! `/recipes/new` — create a recipe (name, instructions, ingredient list as
//! a one-per-line textarea). The empty-name and ingredient rules are
//! pre-validated by the shared `validate_recipe_name` / `parse_ingredients`
//! (inline error, no round trip); the backend's matching 400s are mapped
//! defensively. Any member may create, so 403 isn't reachable via the UI.
//! Success (201) → PRG `/recipes/:id?notice=recipe_created`.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::recipes::{CreateRecipeRequest, RecipeResponse};
use manage_our_home_shared::validation::recipes::{
    parse_ingredients, validate_recipe_name, RecipeFormError,
};

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{family_context, forbidden_page, recipes_cookie};

/// The shared recipe form fields, used by both create and edit. `ingredients`
/// arrives as the raw textarea value and is parsed in the handler.
#[derive(serde::Deserialize, Default)]
pub struct RecipeForm {
    pub name: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub ingredients: String,
}

/// French copy for a recipe form error code.
pub(crate) fn error_message(code: &str) -> &'static str {
    match code {
        "name_required" => "Le nom est obligatoire.",
        "ingredient_name_required" => "Chaque ingrédient doit avoir un nom.",
        "ingredient_quantity_invalid" => "La quantité d'un ingrédient est invalide.",
        "ingredient_quantity_must_be_non_negative" => {
            "La quantité d'un ingrédient ne peut pas être négative."
        }
        "invalid_seasonal_month" => "Les mois de saison doivent être compris entre 1 et 12.",
        "unavailable" => "Service momentanément indisponible, merci de réessayer.",
        _ => "Une erreur est survenue, merci de réessayer.",
    }
}

/// Maps the shared name-form error to the backend's matching code.
pub(crate) fn name_error_code(err: RecipeFormError) -> &'static str {
    match err {
        RecipeFormError::NameRequired => "name_required",
    }
}

/// Renders the recipe form fields (shared markup, pre-filled). Used by create
/// (empty defaults) and edit (existing values, ingredients pre-filled via
/// `format_ingredients`).
pub(crate) fn form_fields(name: &str, instructions: &str, ingredients: &str) -> String {
    format!(
        r#"<label>Nom <input type="text" name="name" required value="{name}"/></label>
<label>Instructions
<textarea name="instructions" rows="5" placeholder="Optionnel — étapes de préparation">{instructions}</textarea>
</label>
<label>Ingrédients
<textarea name="ingredients" rows="6" placeholder="Un ingrédient par ligne :&#10;Farine | 2 | kg&#10;Tomate | 4 | pièce | 6,7,8&#10;Basilic |  |  |  | optionnel">{ingredients}</textarea>
<span class="muted">Un ingrédient par ligne, champs séparés par des barres verticales : <code>intitulé | quantité | unité | mois de saison | optionnel</code>. Seul l'intitulé est obligatoire ; les champs suivants sont facultatifs. Les mois de saison sont des numéros séparés par des virgules (1 à 12). Terminez la ligne par <code>optionnel</code> pour un ingrédient facultatif (exclu du calcul « en stock »).</span>
</label>"#,
        name = html_escape(name),
        instructions = html_escape(instructions),
        ingredients = html_escape(ingredients),
    )
}

fn page(header: &str, form: &RecipeForm, error: Option<&str>) -> String {
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let fields = form_fields(&form.name, &form.instructions, &form.ingredients);
    let body = format!(
        r#"<h1>Nouvelle recette</h1>
{error_html}
<form method="post" action="/recipes/new">
{fields}
<button type="submit">Créer la recette</button>
</form>
<div class="links"><a href="/recipes">Retour aux recettes</a></div>"#,
    );
    shell_with_header(Width::Form, "Nouvelle recette", header, &body)
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/recipes/new").await else {
        return Redirect::to("/groups/new").into_response();
    };
    Html(page(&fam.header, &RecipeForm::default(), None)).into_response()
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<RecipeForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/recipes/new").await else {
        return Redirect::to("/groups/new").into_response();
    };

    let render_error =
        |code: &str| Html(page(&fam.header, &form, Some(error_message(code)))).into_response();

    if let Err(e) = validate_recipe_name(&form.name) {
        return render_error(name_error_code(e));
    }
    let ingredients = match parse_ingredients(&form.ingredients) {
        Ok(list) => list,
        Err(e) => return render_error(e.code()),
    };

    let instructions = {
        let i = form.instructions.trim();
        (!i.is_empty()).then(|| i.to_string())
    };
    let req = CreateRecipeRequest {
        name: form.name.trim().to_string(),
        instructions,
        ingredients,
    };

    let cookie = recipes_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/recipes", fam.gid),
        cookie.as_deref(),
        Some(serde_json::to_value(&req).unwrap()),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            match serde_json::from_value::<RecipeResponse>(resp.body) {
                Ok(recipe) => {
                    Redirect::to(&format!("/recipes/{}?notice=recipe_created", recipe.id))
                        .into_response()
                }
                // Created but couldn't read the id back: fall back to the list.
                Err(_) => Redirect::to("/recipes?notice=recipe_created").into_response(),
            }
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
