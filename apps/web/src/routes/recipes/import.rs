//! `/recipes/import` (#405) — import a recipe from the address of a web
//! page. GET shows the address field; POST asks the API
//! (`POST /groups/:gid/recipes/import`) for the recipe the page publishes,
//! then renders the create form of `/recipes/new` pre-filled with it, for
//! the member to review: nothing is saved before that form is submitted.
//! On a refusal, the same page comes back with the address kept and the
//! reason in plain words.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::recipes::{ImportRecipeRequest, RecipeDraft};

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::detail::fmt_num;
use super::new::RecipeForm;
use super::{family_context, forbidden_page, recipes_cookie};

#[derive(serde::Deserialize, Default)]
pub struct ImportForm {
    #[serde(default)]
    pub url: String,
}

/// French copy for an import refusal: the API's 422 codes, `too_many`
/// for its 429, and `unavailable`.
pub(crate) fn error_text(code: &str) -> &'static str {
    match code {
        "url_invalid" => "Cette adresse n'est pas une adresse web valide.",
        "url_scheme_refused" => "Seules les adresses en http:// ou https:// sont acceptées.",
        "url_destination_refused" => {
            "Cette adresse mène à un réseau local ou privé : elle ne peut pas être importée."
        }
        "source_timeout" => "Le site n'a pas répondu à temps. Réessayez dans un moment.",
        "source_unreachable" => {
            "La page n'a pas pu être lue : le site est injoignable ou a répondu par une erreur."
        }
        "source_not_html" => "Cette adresse ne mène pas à une page web.",
        "source_too_large" => "Cette page est trop lourde pour être importée.",
        "no_recipe_found" => {
            "Aucune recette n'a été trouvée sur cette page : le site ne la publie pas dans un format lisible (schema.org). Saisissez-la à la main."
        }
        "too_many" => "Trop d'imports en peu de temps. Réessayez dans quelques minutes.",
        _ => "Service momentanément indisponible, merci de réessayer.",
    }
}

/// The create form pre-filled from a draft: ingredients as the textarea's
/// `intitulé | quantité | unité` lines, steps numbered one per line.
pub(crate) fn draft_form(draft: &RecipeDraft) -> RecipeForm {
    let ingredients = draft
        .ingredients
        .iter()
        .map(|i| {
            // `|` separates the textarea's fields: one in a name would
            // shift the quantity into the wrong column.
            let name = i.name.replace('|', "/");
            match (i.quantity, i.unit.as_deref()) {
                (Some(q), Some(u)) => format!("{name} | {} | {}", fmt_num(q), u.replace('|', "/")),
                (Some(q), None) => format!("{name} | {}", fmt_num(q)),
                (None, _) => name,
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let instructions = draft
        .steps
        .iter()
        .enumerate()
        .map(|(n, step)| format!("{}. {step}", n + 1))
        .collect::<Vec<_>>()
        .join("\n");
    RecipeForm {
        name: draft.name.clone(),
        instructions,
        ingredients,
        source_url: draft.source_url.clone(),
    }
}

fn page(header: &str, url: &str, error: Option<&str>) -> String {
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let body = format!(
        r#"<h1>Importer une recette</h1>
{error_html}
<p class="muted">Collez l'adresse d'une page de recette (Marmiton, 750g, un blog…). La recette lue s'ouvre dans le formulaire de création : vous la relisez et la corrigez avant de l'enregistrer.</p>
<form method="post" action="/recipes/import">
<label>Adresse de la page <input type="url" name="url" required value="{url}" placeholder="https://"/></label>
<button type="submit">Importer</button>
</form>
<div class="links"><a href="/recipes/new">Saisir une recette à la main</a><a href="/recipes">Retour aux recettes</a></div>"#,
        url = html_escape(url),
    );
    shell_with_header(Width::Form, "Importer une recette", header, &body)
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/recipes/import").await else {
        return Redirect::to("/groups/new").into_response();
    };
    Html(page(&fam.header, "", None)).into_response()
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ImportForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/recipes/import").await else {
        return Redirect::to("/groups/new").into_response();
    };
    let url = form.url.trim();
    let refused = |code: &str| Html(page(&fam.header, url, Some(error_text(code)))).into_response();

    let cookie = recipes_cookie(&headers);
    let request = ImportRecipeRequest {
        url: url.to_string(),
    };
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/recipes/import", fam.gid),
        cookie.as_deref(),
        Some(serde_json::to_value(&request).unwrap()),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match serde_json::from_value::<RecipeDraft>(resp.body) {
                Ok(draft) => {
                    Html(super::new::page(&fam.header, &draft_form(&draft), None)).into_response()
                }
                Err(_) => refused("unavailable"),
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
            let code = resp
                .body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unavailable");
            refused(code)
        }
        Ok(resp) if resp.status == reqwest::StatusCode::TOO_MANY_REQUESTS => refused("too_many"),
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            forbidden_page().into_response()
        }
        Ok(_) | Err(_) => refused("unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manage_our_home_shared::dto::recipes::DraftIngredient;
    use manage_our_home_shared::validation::recipes::parse_ingredients;

    fn ingredient(quantity: Option<f64>, unit: Option<&str>, name: &str) -> DraftIngredient {
        DraftIngredient {
            quantity,
            unit: unit.map(str::to_string),
            name: name.to_string(),
        }
    }

    fn draft() -> RecipeDraft {
        RecipeDraft {
            name: "Pâte à crêpes".into(),
            ingredients: vec![
                ingredient(Some(500.0), Some("g"), "farine"),
                ingredient(Some(0.5), None, "citron"),
                ingredient(Some(0.333), Some("c. à soupe"), "huile"),
                ingredient(None, None, "sel"),
                ingredient(None, None, "sel | poivre"),
            ],
            steps: vec!["Mélanger.".into(), "Laisser reposer 1 heure.".into()],
            source_url: "https://www.marmiton.org/recettes/crepes.aspx".into(),
        }
    }

    #[test]
    fn a_draft_fills_the_create_form() {
        let form = draft_form(&draft());
        assert_eq!(form.name, "Pâte à crêpes");
        assert_eq!(
            form.ingredients,
            "farine | 500 | g\ncitron | 0.5\nhuile | 0.333 | c. à soupe\nsel\nsel / poivre"
        );
        assert_eq!(
            form.instructions,
            "1. Mélanger.\n2. Laisser reposer 1 heure."
        );
        assert_eq!(
            form.source_url,
            "https://www.marmiton.org/recettes/crepes.aspx"
        );
    }

    #[test]
    fn the_ingredient_lines_read_back_as_the_draft_says() {
        let parsed = parse_ingredients(&draft_form(&draft()).ingredients).unwrap();
        let read: Vec<_> = parsed
            .iter()
            .map(|i| (i.name.as_str(), i.quantity, i.unit.as_deref()))
            .collect();
        assert_eq!(
            read,
            [
                ("farine", Some(500.0), Some("g")),
                ("citron", Some(0.5), None),
                ("huile", Some(0.333), Some("c. à soupe")),
                ("sel", None, None),
                ("sel / poivre", None, None),
            ]
        );
    }

    #[test]
    fn every_refusal_has_its_own_words() {
        let codes = [
            "url_invalid",
            "url_scheme_refused",
            "url_destination_refused",
            "source_timeout",
            "source_unreachable",
            "source_not_html",
            "source_too_large",
            "no_recipe_found",
            "too_many",
            "unavailable",
        ];
        let texts: Vec<&str> = codes.iter().map(|c| error_text(c)).collect();
        for (code, text) in codes.iter().zip(&texts) {
            assert!(!text.is_empty(), "{code}");
        }
        let mut distinct = texts.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), codes.len(), "{texts:?}");
        assert_eq!(error_text("something_new"), error_text("unavailable"));
    }
}
