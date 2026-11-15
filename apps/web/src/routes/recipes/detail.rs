//! `/recipes/:id` — recipe detail: ingredients (with optional/seasonal
//! markers), its time and numbered steps (#539), the last-cooked state, plus the log-a-meal action
//! and the delete action; full edit lives in `edit.rs`. The « À réaliser » /
//! « Retirer » forms (#470) plan the recipe for the next grocery list, open
//! to any member like the selection API; a planned recipe's day and meal
//! (#812) are set, changed and removed there too, by any member as well.
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
    IngredientResponse, LogMealRequest, MealHistoryList, RecipeResponse, RecipeSelectionResponse,
};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use manage_our_home_shared::validation::recipes::{duration_label, servings_label, steps_of};

use super::missing::{after_stock_change, incomplete_banner, incomplete_count};
use super::{
    can_modify, deselect_form, family_context, forbidden_page, outcome_links, planned_badge,
    planned_with_stock, recipe_not_found_page, recipes_cookie, select_form,
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

pub(super) fn notice_text(code: &str) -> Option<&'static str> {
    match code {
        "recipe_created" => Some("Recette créée."),
        "recipe_updated" => Some("Recette mise à jour."),
        "meal_logged" => Some("Repas enregistré."),
        "recipe_selected" => Some("Recette à réaliser pour les prochaines courses."),
        "recipe_deselected" => Some("Recette retirée des recettes à réaliser."),
        "recipe_done" => Some(super::outcome::DONE_NOTICE),
        "recipe_abandoned" => Some(super::outcome::ABANDONED_NOTICE),
        "plan_saved" => Some("Jour et repas enregistrés."),
        "plan_cleared" => Some("Date retirée : la recette reste à réaliser."),
        _ => None,
    }
}

pub(super) fn error_text(code: &str) -> Option<&'static str> {
    match code {
        "selection_gone" => Some(super::outcome::SELECTION_GONE),
        "forbidden" => Some("Vous n'avez pas les droits nécessaires pour cette action."),
        "unavailable" => Some("Service momentanément indisponible, merci de réessayer."),
        "invalid_servings" => Some("Le nombre de personnes doit être un nombre entier de 1 à 50."),
        "meal_without_day" => Some("Choisissez un jour pour préciser le repas."),
        "invalid_planned_on" => Some("Le jour doit tomber à 366 jours au plus d'aujourd'hui."),
        "invalid_meal_slot" => Some("Le repas se choisit entre midi et soir."),
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
    let all_planned = planned_with_stock(&state, fam.gid, cookie.as_deref())
        .await
        .unwrap_or_default();
    let planned = all_planned
        .iter()
        .find(|p| p.selection.recipe_id == recipe_id)
        .map(|p| &p.selection);
    // Marked done, the recipe took from the stock (#560): the planned ones
    // it left incomplete are told, with the way to « Prévues ».
    let incomplete = if after_stock_change(query.notice.as_deref()) {
        incomplete_banner(incomplete_count(&all_planned))
    } else {
        String::new()
    };
    let can_edit = can_modify(&fam.role, recipe.created_by == me.user_id);
    let notice_html = query
        .notice
        .as_deref()
        .and_then(notice_text)
        .map(|n| format!(r#"<p class="notice success">{}</p>"#, html_escape(n)))
        .unwrap_or_default();
    let error_html = query
        .error
        .as_deref()
        .and_then(error_text)
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    Html(page(
        &fam.header,
        &recipe,
        last,
        planned,
        is_plan_error(query.error.as_deref()),
        can_edit,
        &format!("{notice_html}{incomplete}{error_html}"),
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

/// When a dated recipe is to be cooked (#812), as its « Quand ? » block
/// says it: "jeudi 19 novembre, au dîner".
fn plan_sentence(planned_on: NaiveDate, meal_slot: Option<&str>) -> String {
    let meal = match meal_slot {
        Some("lunch") => ", au déjeuner",
        Some("dinner") => ", au dîner",
        _ => "",
    };
    format!("{}{meal}", crate::routes::home::long_date_fr(planned_on))
}

/// The errors of the « Quand ? » form (#812), after which the page opens
/// it again.
fn is_plan_error(code: Option<&str>) -> bool {
    matches!(
        code,
        Some("meal_without_day" | "invalid_planned_on" | "invalid_meal_slot")
    )
}

/// The day and the meal of a planned recipe (#812), the selection being
/// `planned`; nothing for a recipe that is not. Undated, a « Choisir un
/// jour » disclosure, closed unless `open` (after an error of its form);
/// dated, the « Quand ? » block, with « Retirer la date ». The day is one of
/// the 366 days either side of `today` the API accepts.
pub(super) fn plan_html(
    id: Uuid,
    selection: Option<&RecipeSelectionResponse>,
    open: bool,
    today: NaiveDate,
    back: &str,
) -> String {
    let Some(selection) = selection else {
        return String::new();
    };
    let horizon = chrono::Duration::days(366);
    let meal = selection.meal_slot.as_deref().unwrap_or_default();
    let radios: String = [("", "Non précisé"), ("lunch", "Midi"), ("dinner", "Soir")]
        .into_iter()
        .map(|(value, label)| {
            let checked = if value == meal { " checked" } else { "" };
            format!(
                r#"<label class="field inline pick"><input type="radio" name="meal_slot" value="{value}"{checked}/>{label}</label>"#
            )
        })
        .collect();
    let (state, clear) = match selection.planned_on {
        Some(day) => (
            format!(
                "Prévue le <strong>{}</strong>.",
                plan_sentence(day, Some(meal))
            ),
            format!(
                r#"<button type="submit" class="secondary" formaction="/recipes/{id}/planning/clear" formnovalidate>Retirer la date</button>"#
            ),
        ),
        None => ("Pas encore de jour choisi.".to_string(), String::new()),
    };
    let form = format!(
        r#"<form method="post" action="/recipes/{id}/planning" class="card inline when-form">
<input type="hidden" name="back" value="{back}"/>
<p class="when-state">{state}</p>
<label>Jour (facultatif)
<input type="date" name="planned_on" value="{value}" min="{min}" max="{max}"/></label>
<fieldset class="picks"><legend>Repas (facultatif)</legend>
{radios}
</fieldset>
<button type="submit" data-busy-label="Enregistrement…">Enregistrer</button>
{clear}
</form>"#,
        value = selection
            .planned_on
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default(),
        min = (today - horizon).format("%Y-%m-%d"),
        max = (today + horizon).format("%Y-%m-%d"),
        back = html_escape(back),
    );
    if selection.planned_on.is_some() {
        form
    } else {
        let open = if open { " open" } else { "" };
        format!(
            r#"<div class="actions"><details{open}><summary>Choisir un jour</summary>{form}</details></div>"#
        )
    }
}

/// The recipe's page. `banners` are the ones of the URL's `?notice=` and
/// `?error=`, and the planned recipes left incomplete in between (#560).
/// `planned` is the recipe's planned selection; `plan_open` opens its
/// « Choisir un jour » again after an error of that form (#812).
fn page(
    header: &str,
    recipe: &RecipeResponse,
    last_eaten: Option<NaiveDate>,
    planned: Option<&RecipeSelectionResponse>,
    plan_open: bool,
    can_edit: bool,
    banners: &str,
) -> String {
    let id = recipe.id;

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

    let instructions_html = steps_html(recipe.instructions.as_deref());

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
    let today_paris = crate::routes::agenda::today_paris();
    let today = today_paris.format("%Y-%m-%d").to_string();
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
    let selection_html = selection_html(id, recipe.servings, planned.map(|s| s.servings));
    let plan_html = plan_html(id, planned, plan_open, today_paris, "detail");
    let time_html = time_html(recipe.duration_minutes);
    let body = format!(
        r#"<h1>{name}</h1>
{source_html}
{time_html}
{banners}
{selection_html}
{plan_html}
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

/// The method as the form wrote it (#539): one numbered step per line.
fn steps_html(instructions: Option<&str>) -> String {
    let steps = steps_of(instructions.unwrap_or_default());
    if steps.is_empty() {
        return String::new();
    }
    let items: String = steps
        .iter()
        .map(|s| format!("<li>{}</li>", html_escape(s)))
        .collect();
    format!("<h2>Étapes</h2><ol>{items}</ol>")
}

/// How long the recipe takes (#539), when it says.
fn time_html(minutes: Option<i32>) -> String {
    minutes
        .map(|m| format!(r#"<p class="muted">Temps : {}.</p>"#, duration_label(m)))
        .unwrap_or_default()
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

    #[test]
    fn the_steps_are_numbered_one_per_line() {
        assert_eq!(
            steps_html(Some("Mélanger <bien>.\n\nCuire.")),
            "<h2>Étapes</h2><ol><li>Mélanger &lt;bien&gt;.</li><li>Cuire.</li></ol>"
        );
        // A recipe imported before #539 stored its steps numbered.
        assert_eq!(
            steps_html(Some("1. Mélanger.\n2. Cuire.")),
            "<h2>Étapes</h2><ol><li>Mélanger.</li><li>Cuire.</li></ol>"
        );
        assert_eq!(steps_html(Some("  \n ")), "");
        assert_eq!(steps_html(None), "");
    }

    #[test]
    fn the_time_is_said_when_the_recipe_says_it() {
        assert_eq!(
            time_html(Some(90)),
            r#"<p class="muted">Temps : 1 h 30.</p>"#
        );
        assert_eq!(time_html(None), "");
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn planned(planned_on: Option<NaiveDate>, meal_slot: Option<&str>) -> RecipeSelectionResponse {
        RecipeSelectionResponse {
            id: Uuid::from_u128(2),
            recipe_id: Uuid::from_u128(1),
            recipe_name: "Gratin".into(),
            status: "planned".into(),
            servings: Some(4),
            planned_on,
            meal_slot: meal_slot.map(str::to_string),
            created_by: Uuid::from_u128(3),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    const TODAY: (i32, u32, u32) = (2026, 11, 15);

    fn today() -> NaiveDate {
        day(TODAY.0, TODAY.1, TODAY.2)
    }

    #[test]
    fn the_plan_sentence_names_the_weekday_the_day_and_the_meal() {
        assert_eq!(plan_sentence(day(2026, 11, 19), None), "jeudi 19 novembre");
        assert_eq!(
            plan_sentence(day(2026, 11, 19), Some("dinner")),
            "jeudi 19 novembre, au dîner"
        );
        assert_eq!(
            plan_sentence(day(2026, 12, 1), Some("lunch")),
            "mardi 1er décembre, au déjeuner"
        );
    }

    #[test]
    fn an_unplanned_recipe_has_no_day_to_choose() {
        assert_eq!(
            plan_html(Uuid::from_u128(1), None, false, today(), "detail"),
            ""
        );
    }

    #[test]
    fn an_undated_planned_recipe_offers_a_closed_choisir_un_jour() {
        let id = Uuid::from_u128(1);
        let html = plan_html(id, Some(&planned(None, None)), false, today(), "detail");
        assert!(
            html.starts_with(r#"<div class="actions"><details><summary>Choisir un jour</summary>"#),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"<form method="post" action="/recipes/{id}/planning" class="card inline when-form">"#
            )),
            "{html}"
        );
        assert!(html.contains(r#"name="back" value="detail""#), "{html}");
        assert!(
            html.contains(r#"<p class="when-state">Pas encore de jour choisi.</p>"#),
            "{html}"
        );
        // The 366 days either side of today, as the API takes them.
        assert!(
            html.contains(
                r#"<input type="date" name="planned_on" value="" min="2025-11-14" max="2027-11-16"/>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="radio" name="meal_slot" value="" checked/>Non précisé"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="radio" name="meal_slot" value="lunch"/>Midi"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="radio" name="meal_slot" value="dinner"/>Soir"#),
            "{html}"
        );
        assert!(
            html.contains("<legend>Repas (facultatif)</legend>"),
            "{html}"
        );
        assert!(!html.contains("Retirer la date"), "{html}");
        // Opened again after an error of its form.
        let html = plan_html(id, Some(&planned(None, None)), true, today(), "detail");
        assert!(html.contains("<details open><summary>"), "{html}");
    }

    #[test]
    fn a_dated_planned_recipe_shows_quand_and_retirer_la_date() {
        let id = Uuid::from_u128(1);
        let html = plan_html(
            id,
            Some(&planned(Some(day(2026, 11, 19)), None)),
            false,
            today(),
            "detail",
        );
        assert!(!html.contains("<details"), "{html}");
        assert!(
            html.contains(
                r#"<p class="when-state">Prévue le <strong>jeudi 19 novembre</strong>.</p>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"name="planned_on" value="2026-11-19""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="radio" name="meal_slot" value="" checked/>Non précisé"#),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"<button type="submit" class="secondary" formaction="/recipes/{id}/planning/clear" formnovalidate>Retirer la date</button>"#
            )),
            "{html}"
        );
    }

    #[test]
    fn a_dated_planned_recipe_with_its_meal_has_it_ticked() {
        let id = Uuid::from_u128(1);
        let html = plan_html(
            id,
            Some(&planned(Some(day(2026, 11, 19)), Some("dinner"))),
            false,
            today(),
            "detail",
        );
        assert!(
            html.contains("Prévue le <strong>jeudi 19 novembre, au dîner</strong>."),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="radio" name="meal_slot" value="dinner" checked/>Soir"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="radio" name="meal_slot" value=""/>Non précisé"#),
            "{html}"
        );
        assert!(html.contains("Retirer la date</button>"), "{html}");
    }

    #[test]
    fn the_plan_banners_are_said_and_its_errors_reopen_the_form() {
        assert_eq!(
            notice_text("plan_saved"),
            Some("Jour et repas enregistrés.")
        );
        assert_eq!(
            notice_text("plan_cleared"),
            Some("Date retirée : la recette reste à réaliser.")
        );
        assert_eq!(
            error_text("meal_without_day"),
            Some("Choisissez un jour pour préciser le repas.")
        );
        assert!(error_text("invalid_planned_on").is_some());
        assert!(error_text("invalid_meal_slot").is_some());
        assert!(is_plan_error(Some("meal_without_day")));
        assert!(is_plan_error(Some("invalid_planned_on")));
        assert!(is_plan_error(Some("invalid_meal_slot")));
        assert!(!is_plan_error(Some("unavailable")));
        assert!(!is_plan_error(None));
    }
}
