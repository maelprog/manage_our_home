//! `/recipes/:id/edit` — full edit of a recipe, in the form of
//! `new.rs` (#539): its lines and steps pre-filled, blank ones under them. Same permission bar as delete (`can_modify`): a
//! non-permitted user never sees the form (GET → forbidden page), and the
//! backend 403 is mapped to `?error=forbidden` on the detail page
//! defensively. No step at all *clears* the instructions (sent as `Some(None)`
//! per the backend's double-`Option` PATCH contract); the edit form always
//! sends the full ingredient list, replacing the recipe's ingredients.
//! Changing « Personnes » from one number to another scales each quantity
//! left as it was saved by the same ratio (#845, `rescale_untouched`): the
//! form carries the number of people and the quantities as saved in hidden
//! fields, so a quantity typed over is told from one left alone.
//! Success (200) → PRG `/recipes/:id?notice=recipe_updated`.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::recipes::{RecipeResponse, UpdateRecipeRequest};
use manage_our_home_shared::validation::recipes::{
    ingredient_fields, parse_servings, rescale_untouched, steps_of,
};
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

    let form = form_of(&recipe);
    Html(page(&fam.header, recipe_id, &recipe.name, &form, None)).into_response()
}

/// The edit form of a recipe, filled with what it holds; the number of
/// people and the quantities are also kept as saved (#845).
fn form_of(recipe: &RecipeResponse) -> RecipeForm {
    let servings = recipe.servings.map(|s| s.to_string()).unwrap_or_default();
    RecipeForm {
        name: recipe.name.clone(),
        servings_was: Some(servings.clone()),
        servings,
        duration: recipe
            .duration_minutes
            .map(|d| d.to_string())
            .unwrap_or_default(),
        source_url: String::new(),
        ingredients: ingredient_fields(&recipe.ingredients),
        steps: steps_of(recipe.instructions.as_deref().unwrap_or_default()),
    }
}

/// The request the form sends: the create form's checks (`Err` is the
/// message to show), then the quantities left as they were scaled to the
/// new number of people (#845). The edit form always sends every field, so
/// each is `Some(...)`: a blank number of people, time, or no step at all
/// clears it.
fn update_request(form: &mut RecipeForm) -> Result<UpdateRecipeRequest, String> {
    let checked = check(form)?;
    // A number as saved that does not read (a forged form) scales nothing.
    let servings_was = form
        .servings_was
        .as_deref()
        .and_then(|s| parse_servings(s).ok().flatten());
    let mut ingredients = checked.ingredients;
    rescale_untouched(
        &form.ingredients,
        &mut ingredients,
        servings_was,
        checked.servings,
    );
    Ok(UpdateRecipeRequest {
        name: Some(form.name.trim().to_string()),
        instructions: Some(checked.instructions),
        ingredients: Some(ingredients),
        servings: Some(checked.servings),
        duration_minutes: Some(checked.duration_minutes),
    })
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
    let body = update_request(&mut form);
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
    let body = match body {
        Ok(b) => b,
        Err(message) => return render_error(&message),
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

#[cfg(test)]
mod tests {
    use super::*;
    use manage_our_home_shared::dto::recipes::IngredientResponse;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// The quantities the edit form sends for `list`.
    fn sent(list: &[(&str, &str)]) -> Vec<Option<f64>> {
        let (mut form, _) = read_form(&pairs(list));
        let body = update_request(&mut form).unwrap();
        body.ingredients
            .unwrap()
            .iter()
            .map(|i| i.quantity)
            .collect()
    }

    /// Flour 200 g and 3 eggs, saved for 4, with « Personnes » sent as
    /// `servings` and flour as `flour`.
    fn crepes(servings: &str, flour: &str) -> Vec<(&'static str, String)> {
        vec![
            ("name", "Crêpes".into()),
            ("servings", servings.into()),
            ("servings_was", "4".into()),
            ("qty.1", flour.into()),
            ("qty_was.1", "200".into()),
            ("unit.1", "g".into()),
            ("ing.1", "Farine".into()),
            ("qty.2", "3".into()),
            ("qty_was.2", "3".into()),
            ("ing.2", "Œufs".into()),
            ("qty.3", "".into()),
            ("qty_was.3", "".into()),
            ("ing.3", "Sel".into()),
        ]
    }

    fn as_pairs<'a>(list: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
        list.iter().map(|(k, v)| (*k, v.as_str())).collect()
    }

    #[test]
    fn more_people_scale_the_quantities_left_as_they_were() {
        let list = crepes("8", "200");
        assert_eq!(sent(&as_pairs(&list)), [Some(400.0), Some(6.0), None]);
    }

    #[test]
    fn a_quantity_typed_over_is_sent_as_typed() {
        let list = crepes("8", "350");
        assert_eq!(sent(&as_pairs(&list)), [Some(350.0), Some(6.0), None]);
    }

    #[test]
    fn without_a_number_of_people_before_or_after_nothing_scales() {
        let cleared = crepes("", "200");
        assert_eq!(sent(&as_pairs(&cleared)), [Some(200.0), Some(3.0), None]);
        let mut unsaid = crepes("8", "200");
        unsaid.retain(|(k, _)| *k != "servings_was");
        unsaid.push(("servings_was", String::new()));
        assert_eq!(sent(&as_pairs(&unsaid)), [Some(200.0), Some(3.0), None]);
        // A number as saved that does not read scales nothing.
        let mut forged = crepes("8", "200");
        forged.retain(|(k, _)| *k != "servings_was");
        forged.push(("servings_was", "beaucoup".into()));
        assert_eq!(sent(&as_pairs(&forged)), [Some(200.0), Some(3.0), None]);
    }

    fn recipe(servings: Option<i32>) -> RecipeResponse {
        serde_json::from_value(serde_json::json!({
            "id": Uuid::nil(),
            "group_id": Uuid::nil(),
            "name": "Crêpes",
            "instructions": null,
            "servings": servings,
            "duration_minutes": null,
            "source_url": null,
            "created_by": Uuid::nil(),
            "created_at": "2026-11-14T10:00:00Z",
            "updated_at": "2026-11-14T10:00:00Z",
            "ingredients": [],
        }))
        .unwrap()
    }

    #[test]
    fn the_edit_form_carries_what_was_saved_and_says_what_changing_people_does() {
        let mut saved = recipe(Some(4));
        saved.ingredients = vec![IngredientResponse {
            id: Uuid::nil(),
            name: "Farine".into(),
            quantity: Some(200.0),
            unit: Some("g".into()),
            is_optional: false,
            seasonal_months: None,
        }];
        let html = page("", Uuid::nil(), "Crêpes", &form_of(&saved), None);
        assert!(
            html.contains(r#"<input type="hidden" name="servings_was" value="4"/>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="hidden" name="qty_was.1" value="200"/>"#),
            "{html}"
        );
        assert!(
            html.contains(
                "Changer les personnes recalcule les quantités que vous n'avez pas modifiées."
            ),
            "{html}"
        );
        // A blank line added under it has no quantity as saved.
        assert!(!html.contains(r#"name="qty_was.2""#), "{html}");
    }

    #[test]
    fn the_create_form_says_nothing_of_it() {
        let html = super::super::new::page("", &RecipeForm::default(), None);
        assert!(!html.contains("servings_was"), "{html}");
        assert!(!html.contains("qty_was"), "{html}");
        assert!(!html.contains("Changer les personnes"), "{html}");
    }
}
