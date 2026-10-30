//! `/recipes/new` — create a recipe. Since #539 the form reads: an import
//! by link at the top (its own form, posting to `/recipes/import`); then
//! name, number of people and time; one line per ingredient (quantity,
//! unit, name, seasonal months, « Facultatif », « Retirer »); and numbered
//! steps. Everything works without JavaScript: the form is drawn with blank
//! lines to fill, a line ticked « Retirer » is left out when the form is
//! read, and « Ajouter des lignes » sends the form back with more blank
//! ones, saving nothing.
//!
//! The empty-name, ingredient, servings and time rules are pre-validated by
//! the shared `validation::recipes` (inline error, no round trip); the
//! backend's matching 400s are mapped defensively. Any member may create,
//! so 403 isn't reachable via the UI. Success (201) → PRG
//! `/recipes/:id?notice=recipe_created`.
//!
//! The import (`import.rs`, #405) renders this same form, pre-filled from
//! the page it read, with the page's address in a hidden `source_url`
//! field: the reviewed recipe is created here, like any other.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::recipes::{CreateRecipeRequest, RecipeResponse};
use manage_our_home_shared::validation::recipes::{
    join_steps, parse_duration, parse_ingredient_fields, parse_servings, validate_recipe_name,
    IngredientFields, IngredientParseError, RecipeFormError, DURATION_MAX, SERVINGS_MAX,
};

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{family_context, forbidden_page, recipes_cookie};

/// The recipe form as typed, shared by create, edit and the import.
#[derive(Debug, Default, PartialEq)]
pub struct RecipeForm {
    pub name: String,
    /// The number of people the recipe is written for (#470); blank when
    /// it does not say. Parsed by `parse_servings`.
    pub servings: String,
    /// How long it takes, in minutes (#539); blank when it does not say.
    /// Parsed by `parse_duration`.
    pub duration: String,
    /// The page an imported recipe comes from (#405); empty otherwise.
    pub source_url: String,
    /// The ingredient lines, in their order, those ticked « Retirer » left
    /// out.
    pub ingredients: Vec<IngredientFields>,
    /// The steps, in their order.
    pub steps: Vec<String>,
}

impl RecipeForm {
    /// Leaves out the ingredient lines and steps nothing was typed in.
    pub(crate) fn drop_blanks(&mut self) {
        self.ingredients.retain(|l| !l.is_blank());
        self.steps.retain(|s| !s.trim().is_empty());
    }
}

/// What the member asked for by the button they pressed.
#[derive(Debug, PartialEq)]
pub(crate) enum Intent {
    Save,
    /// « Ajouter des lignes »: the form back, with more blank lines.
    MoreLines,
}

/// Blank ingredient lines and steps a new recipe's form is drawn with.
pub(crate) const NEW_LINES: usize = 5;
pub(crate) const NEW_STEPS: usize = 3;
/// Blank ones added under those already filled: on the edit form, on a form
/// sent back with an error, and by « Ajouter des lignes ».
pub(crate) const MORE_LINES: usize = 3;
pub(crate) const MORE_STEPS: usize = 2;

/// Reads the posted form: plain fields by name, ingredient lines and steps
/// by their number (`ing.3`, `qty.3`, …, `step.2`). A line ticked
/// `remove.N` is left out; a key whose number does not read is ignored.
pub(crate) fn read_form(pairs: &[(String, String)]) -> (RecipeForm, Intent) {
    let mut form = RecipeForm::default();
    let mut intent = Intent::Save;
    let mut lines: BTreeMap<usize, IngredientFields> = BTreeMap::new();
    let mut removed: Vec<usize> = Vec::new();
    let mut steps: BTreeMap<usize, String> = BTreeMap::new();
    for (key, value) in pairs {
        match key.as_str() {
            "name" => form.name = value.clone(),
            "servings" => form.servings = value.clone(),
            "duration" => form.duration = value.clone(),
            "source_url" => form.source_url = value.clone(),
            "more" => intent = Intent::MoreLines,
            _ => {
                let Some((field, n)) = key.split_once('.') else {
                    continue;
                };
                let Ok(n) = n.parse::<usize>() else {
                    continue;
                };
                match field {
                    "step" => {
                        steps.insert(n, value.clone());
                    }
                    "remove" => removed.push(n),
                    _ => {
                        let line = lines.entry(n).or_default();
                        match field {
                            "qty" => line.quantity = value.clone(),
                            "unit" => line.unit = value.clone(),
                            "ing" => line.name = value.clone(),
                            "months" => line.months = value.clone(),
                            "optional" => line.optional = true,
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    form.ingredients = lines
        .into_iter()
        .filter(|(n, _)| !removed.contains(n))
        .map(|(_, line)| line)
        .collect();
    form.steps = steps.into_values().collect();
    (form, intent)
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
        "invalid_source_url" => "L'adresse de la page d'origine n'est pas valide.",
        "invalid_servings" => "Le nombre de personnes doit être un nombre entier de 1 à 50.",
        "invalid_duration" => {
            "Le temps doit être un nombre entier de minutes, de 1 à 1440 (24 heures)."
        }
        "unavailable" => "Service momentanément indisponible, merci de réessayer.",
        _ => "Une erreur est survenue, merci de réessayer.",
    }
}

/// The message of an ingredient line in error, which names the line.
pub(crate) fn ingredient_error(err: IngredientParseError) -> String {
    let line = match err {
        IngredientParseError::NameRequired { line }
        | IngredientParseError::QuantityInvalid { line }
        | IngredientParseError::QuantityNegative { line }
        | IngredientParseError::MonthInvalid { line } => line,
    };
    let message = error_message(err.code());
    // « Chaque ingrédient… » → « Ingrédient 2 : chaque ingrédient… ».
    let mut chars = message.chars();
    let lowered = chars
        .next()
        .map(|c| c.to_lowercase().chain(chars).collect::<String>())
        .unwrap_or_default();
    format!("Ingrédient {line} : {lowered}")
}

/// Maps the shared name-form error to the backend's matching code.
pub(crate) fn name_error_code(err: RecipeFormError) -> &'static str {
    match err {
        RecipeFormError::NameRequired => "name_required",
    }
}

/// The form's fields (shared markup, pre-filled), with `blank_lines` empty
/// ingredient lines and `blank_steps` empty steps after those of `form`.
/// The blank lines and steps under those of `form`: a form with none
/// gets [`NEW_LINES`] / [`NEW_STEPS`], one that has some [`MORE_LINES`] /
/// [`MORE_STEPS`] more.
pub(crate) fn form_fields(form: &RecipeForm) -> String {
    let lines = if form.ingredients.is_empty() {
        NEW_LINES
    } else {
        MORE_LINES
    };
    let steps = if form.steps.is_empty() {
        NEW_STEPS
    } else {
        MORE_STEPS
    };
    fields_with(form, lines, steps)
}

/// One ingredient line, number `n`: each field's label is said with the
/// line's number (`.sr-only`), so that a screen reader tells line 2's
/// quantity from line 3's.
fn ingredient_line(n: usize, line: &IngredientFields) -> String {
    let of_line = format!(r#"<span class="sr-only"> (ligne {n})</span>"#);
    let checked = if line.optional { " checked" } else { "" };
    format!(
        r#"<li class="line">
<label>Quantité{of_line} <input type="text" inputmode="decimal" name="qty.{n}" value="{quantity}"/></label>
<label>Unité{of_line} <input type="text" name="unit.{n}" value="{unit}"/></label>
<label class="what">Ingrédient{of_line} <input type="text" name="ing.{n}" value="{name}"/></label>
<label>Saison{of_line} <input type="text" inputmode="numeric" name="months.{n}" value="{months}" placeholder="6,7,8"/></label>
<label class="field inline"><input type="checkbox" name="optional.{n}" value="1"{checked}/> Facultatif{of_line}</label>
<label class="field inline"><input type="checkbox" name="remove.{n}" value="1"/> Retirer{of_line}</label>
</li>"#,
        quantity = html_escape(&line.quantity),
        unit = html_escape(&line.unit),
        name = html_escape(&line.name),
        months = html_escape(&line.months),
    )
}

fn fields_with(form: &RecipeForm, blank_lines: usize, blank_steps: usize) -> String {
    let blank = IngredientFields::default();
    let lines: String = form
        .ingredients
        .iter()
        .chain(std::iter::repeat_n(&blank, blank_lines))
        .enumerate()
        .map(|(i, line)| ingredient_line(i + 1, line))
        .collect();
    let steps: String = form
        .steps
        .iter()
        .map(String::as_str)
        .chain(std::iter::repeat_n("", blank_steps))
        .enumerate()
        .map(|(i, step)| {
            format!(
                r#"<li><label>Étape {n} <textarea name="step.{n}" rows="2">{step}</textarea></label></li>"#,
                n = i + 1,
                step = html_escape(step),
            )
        })
        .collect();
    format!(
        r#"<label>Nom <input type="text" name="name" required value="{name}"/></label>
<div class="pair">
<label>Personnes
<input type="number" name="servings" min="1" max="{SERVINGS_MAX}" step="1" value="{servings}" placeholder="Optionnel"/>
</label>
<label>Temps (minutes)
<input type="number" name="duration" min="1" max="{DURATION_MAX}" step="1" value="{duration}" placeholder="Optionnel"/>
</label>
</div>
<p class="muted">Personnes : pour combien les quantités sont écrites. La liste de courses les recalcule pour les personnes indiquées en mettant la recette « À réaliser ».</p>
<fieldset class="card">
<legend>Ingrédients</legend>
<p class="muted">Seul l'ingrédient est obligatoire. Saison : les mois, en numéros séparés par des virgules (1 à 12). Un ingrédient facultatif n'est pas compté pour savoir si la recette se fait avec le stock. Une ligne laissée vide est ignorée.</p>
<ol class="lines">{lines}</ol>
</fieldset>
<fieldset class="card">
<legend>Étapes</legend>
<ol class="lines">{steps}</ol>
</fieldset>"#,
        name = html_escape(&form.name),
        servings = html_escape(&form.servings),
        duration = html_escape(&form.duration),
    )
}

/// The buttons under the form: the one that saves comes first, as the
/// Enter key presses the first submit button of a form.
pub(crate) fn form_buttons(save: &str) -> String {
    format!(
        r#"<div class="actions">
<button type="submit">{save}</button>
<button type="submit" name="more" value="1" class="secondary" formnovalidate>Ajouter des lignes</button>
</div>"#
    )
}

/// The checks the form makes before calling the API, in the backend's
/// order; `Err` is the message to show.
pub(crate) struct Checked {
    pub ingredients: Vec<manage_our_home_shared::dto::recipes::IngredientInput>,
    pub instructions: Option<String>,
    pub servings: Option<i32>,
    pub duration_minutes: Option<i32>,
}

pub(crate) fn check(form: &RecipeForm) -> Result<Checked, String> {
    if let Err(e) = validate_recipe_name(&form.name) {
        return Err(error_message(name_error_code(e)).to_string());
    }
    let ingredients = parse_ingredient_fields(&form.ingredients).map_err(ingredient_error)?;
    let servings = parse_servings(&form.servings).map_err(|c| error_message(c).to_string())?;
    let duration_minutes =
        parse_duration(&form.duration).map_err(|c| error_message(c).to_string())?;
    Ok(Checked {
        ingredients,
        instructions: join_steps(&form.steps),
        servings,
        duration_minutes,
    })
}

/// The create page. Without a `source_url`, the import by link comes first
/// (its own form); with one, the form is an imported draft (#405): a notice
/// names the page and asks for a review, and the address rides along in a
/// hidden field.
pub(crate) fn page(header: &str, form: &RecipeForm, error: Option<&str>) -> String {
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let source_url = form.source_url.trim();
    let (import_html, imported_html) = if source_url.is_empty() {
        (
            r#"<form method="post" action="/recipes/import" class="card inline">
<label>Importer depuis un lien <input type="url" name="url" required placeholder="https://"/></label>
<button type="submit" class="secondary">Importer</button>
</form>"#
                .to_string(),
            String::new(),
        )
    } else {
        let from = super::detail::source_domain(source_url)
            .map(|domain| format!(" depuis {}", html_escape(&domain)))
            .unwrap_or_default();
        (
            String::new(),
            format!(
                r#"<p class="notice">Recette importée{from}. Relisez-la et corrigez-la : rien n'est enregistré avant « Créer la recette ».</p>
<input type="hidden" name="source_url" value="{url}"/>"#,
                url = html_escape(source_url),
            ),
        )
    };
    let fields = form_fields(form);
    let buttons = form_buttons("Créer la recette");
    let body = format!(
        r#"<h1>Nouvelle recette</h1>
{error_html}
{import_html}
<form method="post" action="/recipes/new">
{imported_html}
{fields}
{buttons}
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
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/recipes/new").await else {
        return Redirect::to("/groups/new").into_response();
    };
    let (mut form, intent) = read_form(&pairs);
    if intent == Intent::MoreLines {
        return Html(page(&fam.header, &form, None)).into_response();
    }
    let checked = check(&form);
    // Sent back, the form keeps what was typed: its blank lines go, and
    // fresh ones come under the others.
    form.drop_blanks();
    let render_error =
        |message: &str| Html(page(&fam.header, &form, Some(message))).into_response();
    let checked = match checked {
        Ok(c) => c,
        Err(message) => return render_error(&message),
    };
    let source_url = {
        let s = form.source_url.trim();
        (!s.is_empty()).then(|| s.to_string())
    };
    let req = CreateRecipeRequest {
        name: form.name.trim().to_string(),
        instructions: checked.instructions,
        ingredients: checked.ingredients,
        source_url,
        servings: checked.servings,
        duration_minutes: checked.duration_minutes,
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
            render_error(error_message(code))
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            forbidden_page().into_response()
        }
        Ok(_) | Err(_) => render_error(error_message("unavailable")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn line(
        quantity: &str,
        unit: &str,
        name: &str,
        months: &str,
        optional: bool,
    ) -> IngredientFields {
        IngredientFields {
            quantity: quantity.into(),
            unit: unit.into(),
            name: name.into(),
            months: months.into(),
            optional,
        }
    }

    #[test]
    fn the_form_reads_its_fields_lines_and_steps_in_order() {
        let (form, intent) = read_form(&pairs(&[
            ("name", "Crêpes"),
            ("servings", "4"),
            ("duration", "30"),
            ("source_url", ""),
            ("qty.2", "1"),
            ("unit.2", "L"),
            ("ing.2", "Lait"),
            ("months.2", ""),
            ("qty.1", "250"),
            ("unit.1", "g"),
            ("ing.1", "Farine"),
            ("months.1", "1,2"),
            ("optional.3", "1"),
            ("ing.3", "Rhum"),
            ("step.2", "Cuire."),
            ("step.1", "Mélanger."),
        ]));
        assert_eq!(intent, Intent::Save);
        assert_eq!(form.name, "Crêpes");
        assert_eq!(form.servings, "4");
        assert_eq!(form.duration, "30");
        assert_eq!(
            form.ingredients,
            vec![
                line("250", "g", "Farine", "1,2", false),
                line("1", "L", "Lait", "", false),
                line("", "", "Rhum", "", true),
            ]
        );
        assert_eq!(form.steps, ["Mélanger.", "Cuire."]);
    }

    #[test]
    fn a_line_ticked_retirer_is_left_out() {
        let (form, _) = read_form(&pairs(&[
            ("ing.1", "Farine"),
            ("ing.2", "Lait"),
            ("remove.2", "1"),
            ("ing.3", "Sucre"),
        ]));
        let names: Vec<_> = form.ingredients.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Farine", "Sucre"]);
    }

    #[test]
    fn a_number_that_does_not_read_is_ignored() {
        let (form, _) = read_form(&pairs(&[
            ("ing.x", "Farine"),
            ("ing.", "Lait"),
            ("ing.1", "Sucre"),
            ("step.-1", "Rien."),
        ]));
        assert_eq!(form.ingredients, vec![line("", "", "Sucre", "", false)]);
        assert!(form.steps.is_empty());
    }

    #[test]
    fn ajouter_des_lignes_saves_nothing() {
        let (_, intent) = read_form(&pairs(&[("name", "Crêpes"), ("more", "1")]));
        assert_eq!(intent, Intent::MoreLines);
    }

    #[test]
    fn the_fields_hold_the_lines_typed_then_blank_ones() {
        let form = RecipeForm {
            name: "Crêpes <maison>".into(),
            servings: "4".into(),
            duration: "30".into(),
            ingredients: vec![line("250", "g", "Farine", "1,2", true)],
            steps: vec!["Mélanger.".into()],
            ..RecipeForm::default()
        };
        let html = fields_with(&form, 2, 1);
        assert!(
            html.contains(r#"name="name" required value="Crêpes &lt;maison&gt;""#),
            "{html}"
        );
        assert!(
            html.contains(r#"name="servings" min="1" max="50""#),
            "{html}"
        );
        assert!(html.contains(r#"value="4""#), "{html}");
        assert!(
            html.contains(r#"name="duration" min="1" max="1440" step="1" value="30""#),
            "{html}"
        );
        // Line 1 as typed, seasonal months and « Facultatif » included.
        assert!(html.contains(r#"name="qty.1" value="250""#), "{html}");
        assert!(html.contains(r#"name="unit.1" value="g""#), "{html}");
        assert!(html.contains(r#"name="ing.1" value="Farine""#), "{html}");
        assert!(html.contains(r#"name="months.1" value="1,2""#), "{html}");
        assert!(
            html.contains(r#"name="optional.1" value="1" checked"#),
            "{html}"
        );
        assert!(html.contains(r#"name="remove.1" value="1""#), "{html}");
        // Then two blank lines, numbered on.
        assert!(html.contains(r#"name="ing.3" value="""#), "{html}");
        assert!(!html.contains(r#"name="ing.4""#), "{html}");
        assert!(html.contains(r#"name="optional.3" value="1"/>"#), "{html}");
        // Each field of a line is named with its number, for the ear.
        assert!(
            html.contains(r#"Ingrédient<span class="sr-only"> (ligne 2)</span>"#),
            "{html}"
        );
        // Numbered steps, the typed one first.
        assert!(html.contains("Étape 1"), "{html}");
        assert!(
            html.contains(r#"<textarea name="step.1" rows="2">Mélanger.</textarea>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<textarea name="step.2" rows="2"></textarea>"#),
            "{html}"
        );
        assert!(!html.contains(r#"name="step.3""#), "{html}");
    }

    #[test]
    fn a_new_recipe_opens_on_the_import_by_link_then_the_form() {
        let html = page("", &RecipeForm::default(), None);
        let import = html
            .find(r#"<form method="post" action="/recipes/import""#)
            .expect("import form");
        let create = html
            .find(r#"<form method="post" action="/recipes/new""#)
            .expect("create form");
        assert!(import < create, "{html}");
        assert!(html.contains(r#"name="url""#), "{html}");
        // Drawn with its blank lines and steps.
        assert!(
            html.contains(&format!(r#"name="ing.{NEW_LINES}""#)),
            "{html}"
        );
        assert!(
            !html.contains(&format!(r#"name="ing.{}""#, NEW_LINES + 1)),
            "{html}"
        );
        assert!(
            html.contains(&format!(r#"name="step.{NEW_STEPS}""#)),
            "{html}"
        );
        // « Créer la recette » comes before « Ajouter des lignes »: the
        // Enter key presses the first one.
        let create_btn = html.find(">Créer la recette</button>").unwrap();
        let more_btn = html.find(">Ajouter des lignes</button>").unwrap();
        assert!(create_btn < more_btn, "{html}");
        assert!(html.contains(r#"name="more" value="1""#), "{html}");
    }

    #[test]
    fn an_imported_draft_carries_its_source_and_no_import_form() {
        let form = RecipeForm {
            name: "Crêpes".into(),
            source_url: "https://www.marmiton.org/recettes/crepes.aspx".into(),
            ingredients: vec![line("250", "g", "Farine", "", false)],
            ..RecipeForm::default()
        };
        let html = page("", &form, None);
        assert!(
            html.contains("Recette importée depuis marmiton.org"),
            "{html}"
        );
        assert!(
            html.contains(
                r#"name="source_url" value="https://www.marmiton.org/recettes/crepes.aspx""#
            ),
            "{html}"
        );
        assert!(!html.contains(r#"action="/recipes/import""#), "{html}");
        // Its line, then blank ones to add more.
        assert!(html.contains(r#"name="ing.1" value="Farine""#), "{html}");
        assert!(
            html.contains(&format!(r#"name="ing.{}""#, 1 + MORE_LINES)),
            "{html}"
        );
    }

    #[test]
    fn sent_back_the_form_gains_blank_lines_under_those_typed() {
        let form = RecipeForm {
            ingredients: vec![line("", "", "Farine", "", false); 5],
            steps: vec!["Un.".into(); 3],
            ..RecipeForm::default()
        };
        let html = page("", &form, Some("Erreur."));
        assert!(
            html.contains(r#"<p class="notice error">Erreur.</p>"#),
            "{html}"
        );
        assert!(
            html.contains(&format!(r#"name="ing.{}""#, 5 + MORE_LINES)),
            "{html}"
        );
        assert!(
            !html.contains(&format!(r#"name="ing.{}""#, 6 + MORE_LINES)),
            "{html}"
        );
        assert!(
            html.contains(&format!(r#"name="step.{}""#, 3 + MORE_STEPS)),
            "{html}"
        );
    }

    #[test]
    fn an_ingredient_in_error_names_its_line() {
        assert_eq!(
            ingredient_error(IngredientParseError::NameRequired { line: 2 }),
            "Ingrédient 2 : chaque ingrédient doit avoir un nom."
        );
        assert_eq!(
            ingredient_error(IngredientParseError::MonthInvalid { line: 1 }),
            "Ingrédient 1 : les mois de saison doivent être compris entre 1 et 12."
        );
    }

    #[test]
    fn the_checks_turn_the_form_into_the_request() {
        let form = RecipeForm {
            name: "Crêpes".into(),
            servings: "4".into(),
            duration: "90".into(),
            ingredients: vec![
                line("0,5", "L", "Lait", "", false),
                line("", "", "", "", false),
            ],
            steps: vec!["Mélanger.".into(), "".into(), "Cuire.".into()],
            ..RecipeForm::default()
        };
        let checked = check(&form).unwrap();
        assert_eq!(checked.ingredients.len(), 1);
        assert_eq!(checked.ingredients[0].quantity, Some(0.5));
        assert_eq!(checked.instructions.as_deref(), Some("Mélanger.\nCuire."));
        assert_eq!(checked.servings, Some(4));
        assert_eq!(checked.duration_minutes, Some(90));
        let late = RecipeForm {
            name: "Crêpes".into(),
            duration: "1441".into(),
            ..RecipeForm::default()
        };
        assert_eq!(
            check(&late).err().as_deref(),
            Some(error_message("invalid_duration"))
        );
        assert_eq!(
            check(&RecipeForm::default()).err().as_deref(),
            Some("Le nom est obligatoire.")
        );
    }
}
