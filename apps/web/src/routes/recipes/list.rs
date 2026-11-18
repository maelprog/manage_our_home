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
//!
//! Since #539 that page is the « Toutes » tab of three, picked by `?vue=`
//! (no JavaScript): « Prévues » lists the planned recipes, each with
//! « Faite », « Abandonner » and « Retirer », and the generation of the
//! grocery list; « Possibles » the recipes whose every required ingredient
//! is in stock (`?possible=true` on the suggestions endpoint).
//!
//! Since #812 a planned recipe that has a day shows it, with its meal, in
//! « Prévues » and « Toutes » (`plan_badge`); one without shows nothing more.
//!
//! Since #846 a planned recipe of those two tabs ends its actions with the
//! field of its number of people (`servings_form`), which its badge no
//! longer repeats.
//!
//! Since #825 « Possibles » carries the box « Éviter les recettes contenant
//! les allergènes de la famille », ticked by default: unticked, the
//! suggestions are asked with `avoid_allergens=false` for that request, and
//! no recipe of the tab shows its allergy warning; ticked, each shows its
//! own (`allergy::banner`).

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::NaiveDate;
use manage_our_home_shared::dto::recipes::{
    MissingIngredient, PlannedSelection, RecipeList, RecipeResponse, RecipeSelectionResponse,
    RecipeSuggestion, SuggestionList,
};
use manage_our_home_shared::validation::recipes::{servings_label, stock_summary};

use crate::app::{html_escape, kitchen_segment, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::routes::stocks::new::OFF_ATTRIBUTION;
use crate::state::{api_request_auth, AppState};

use super::missing::{
    incomplete_banner, incomplete_count, missing_badge, missing_dialog, use_by_badge,
};
use super::start::{prefill, usual_servings};
use super::week::{back_window, slot_from_query, week_back, week_href, window_for};
use super::{
    allergy, deselect_form, family_context, outcome_links, plan_badge, planned_badge,
    planned_with_stock, recipes_cookie, select_form, select_form_on, service_unavailable_page,
    servings_form,
};

#[derive(serde::Deserialize)]
pub struct ListQuery {
    notice: Option<String>,
    error: Option<String>,
    /// The tab (#539): `prevues`, `possibles`, or the full list.
    vue: Option<String>,
    /// The slot of the planning an « Ajouter » came from (#813): its day
    /// and `midi` or `soir`; either missing or unreadable, no slot.
    #[serde(default, deserialize_with = "crate::lenient_query::lenient")]
    date: Option<NaiveDate>,
    repas: Option<String>,
    /// The allergens' box of « Possibles » (#825): its form was sent, and
    /// whether the box was ticked (`allergy::avoid_from`).
    avoid_form: Option<String>,
    avoid_allergens: Option<String>,
    /// The number of people set (`notice=servings_updated`, #846).
    servings: Option<String>,
}

/// The success banner of the URL's `?notice=`, a number of people changed
/// with the one of `?servings=` (#846).
fn notice_html(notice: Option<&str>, servings: Option<&str>) -> String {
    let Some(code) = notice else {
        return String::new();
    };
    super::servings_banner(code, servings)
        .or_else(|| {
            notice_text(code)
                .map(|n| format!(r#"<p class="notice success">{}</p>"#, html_escape(n)))
        })
        .unwrap_or_default()
}

fn notice_text(code: &str) -> Option<&'static str> {
    match code {
        "recipe_created" => Some("Recette créée."),
        "recipe_deleted" => Some("Recette supprimée."),
        "recipe_selected" => Some("Recette à réaliser pour les prochaines courses."),
        "recipe_deselected" => Some("Recette retirée des recettes à réaliser."),
        "recipe_done" => Some(super::outcome::DONE_NOTICE),
        "recipe_abandoned" => Some(super::outcome::ABANDONED_NOTICE),
        "missing_added" => Some("Éléments manquants ajoutés à la liste de courses."),
        "missing_none" => Some(
            "Aucun article ajouté : les éléments manquants sont déjà sur la liste de courses, ou plus rien ne manque.",
        ),
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

/// One suggestion; `warn` shows its allergy warning (#825).
fn suggestion_html(s: &RecipeSuggestion, warn: bool) -> String {
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
    let allergy = if warn {
        allergy::banner(&s.allergy_warning, true, false)
    } else {
        String::new()
    };
    format!(
        r#"<li class="list-row stacked">
<a href="/recipes/{id}"><strong>{name}</strong></a>{recent}
<div class="muted">{summary}</div>
{allergy}{missing}
</li>"#,
        id = s.recipe_id,
        name = html_escape(&s.name),
    )
}

/// The slot of the planning an « Ajouter » link came from (#813): its day,
/// its meal (`lunch`, `dinner`), and the `back` of the planning's window
/// that shows it.
pub(super) struct Slot {
    day: NaiveDate,
    meal: &'static str,
    back: String,
}

impl Slot {
    pub(super) fn new(day: NaiveDate, meal: &'static str, today: NaiveDate) -> Slot {
        Slot {
            day,
            meal,
            back: week_back(window_for(day, today)),
        }
    }
}

/// The banner of the list opened from a slot: which one, and the way back.
fn slot_banner(slot: Option<&Slot>) -> String {
    let Some(slot) = slot else {
        return String::new();
    };
    let meal = if slot.meal == "lunch" { "midi" } else { "soir" };
    format!(
        r#"<p class="notice">Pour le <strong>{day}, {meal}</strong> : « À réaliser » sur une recette la place dans le planning. <a href="{href}">Retour au planning</a></p>"#,
        day = crate::routes::home::long_date_fr(slot.day),
        href = week_href(back_window(&slot.back).flatten()),
    )
}

/// The mark on the page header that has `enhance.js` open the first
/// questions (#820) once per member — not when the list was opened from a
/// slot of the planning, which would lose the slot.
fn start_mark(slot: Option<&Slot>, user: uuid::Uuid) -> String {
    match slot {
        None => format!(r#" data-depart="{user}""#),
        Some(_) => String::new(),
    }
}

/// One recipe of the full list. `planned` is its selection when it is
/// « À réaliser », `None` when it is not. Opened from a slot of the
/// planning (`slot`, #813), a recipe not planned yet is planned on it by its
/// « À réaliser ». `usual` is the household's usual number of people
/// (#820), which « À réaliser » proposes before the recipe's base.
fn recipe_row_for(
    r: &RecipeResponse,
    planned: Option<&RecipeSelectionResponse>,
    slot: Option<&Slot>,
    usual: Option<i32>,
) -> String {
    let base = prefill(usual, r.servings);
    let count = r.ingredients.len();
    let count_label = match count {
        0 => "aucun ingrédient".to_string(),
        1 => "1 ingrédient".to_string(),
        n => format!("{n} ingrédients"),
    };
    let selection = match planned {
        Some(s) => format!(
            r#"<div class="actions">{badge}{plan}{outcome}{deselect}{people}</div>"#,
            badge = planned_badge(),
            plan = plan_badge(s.planned_on, s.meal_slot.as_deref()),
            outcome = outcome_links(r.id, "list"),
            deselect = deselect_form(r.id, "list"),
            people = servings_form(r.id, s.servings, "list", &r.name),
        ),
        None => match slot {
            Some(slot) => select_form_on(r.id, base, &slot.back, "actions", (slot.day, slot.meal)),
            None => select_form(r.id, base, "list", "actions"),
        },
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

/// The three groups of recipes (#539), one tab each, picked by `?vue=`
/// so that they work without JavaScript.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Tab {
    /// Every recipe, with the planned ones and the suggestions on top.
    All,
    /// The `planned` selections (#469).
    Planned,
    /// The recipes the stock covers entirely.
    Possible,
    /// The planning (#813), a page of its own: `/recipes/week`.
    Week,
}

impl Tab {
    /// The tab `?vue=` names; anything else is the full list.
    fn from_query(vue: Option<&str>) -> Tab {
        match vue {
            Some("prevues") => Tab::Planned,
            Some("possibles") => Tab::Possible,
            _ => Tab::All,
        }
    }
}

/// The row of tabs, each group's count beside its name, the current one in
/// `aria-current` — the `.filters` of the stocks and notifications.
pub(super) fn tabs_html(current: Tab, counts: Option<(usize, usize)>) -> String {
    let count = |n: usize| counts.map(|_| format!(" · {n}")).unwrap_or_default();
    let (planned, possible) = counts.unwrap_or_default();
    let (planned, possible) = (count(planned), count(possible));
    let links: String = [
        (Tab::All, "/recipes", "Toutes".to_string()),
        (
            Tab::Planned,
            "/recipes?vue=prevues",
            format!("Prévues{planned}"),
        ),
        (
            Tab::Possible,
            "/recipes?vue=possibles",
            format!("Possibles{possible}"),
        ),
        (Tab::Week, "/recipes/week", "Planning".to_string()),
    ]
    .into_iter()
    .map(|(tab, href, label)| {
        let lit = if tab == current {
            r#" aria-current="true""#
        } else {
            ""
        };
        format!(r#"<a href="{href}"{lit}>{label}</a>"#)
    })
    .collect();
    format!(r#"<nav class="filters" aria-label="Groupes de recettes">{links}</nav>"#)
}

/// « Prévues »: one row per planned recipe, for how many people, with
/// « Faite » and « Abandonner » (#474) and « Retirer » (#470); then the
/// generation of the grocery list, which takes these recipes. Since #560
/// a row says by when to cook it and what the stock lacks of it, in the
/// API's order — the dated ones first.
fn planned_tab_html(planned: &[PlannedSelection]) -> String {
    if planned.is_empty() {
        return r#"<p class="muted">Aucune recette prévue. Le bouton « À réaliser » d'une recette, dans <a href="/recipes">Toutes</a>, la fait entrer dans la prochaine liste de courses.</p>"#.to_string();
    }
    let rows: String = planned
        .iter()
        .map(|p| {
            let s = &p.selection;
            format!(
                r#"<li class="list-row">
<span><a href="/recipes/{id}"><strong>{name}</strong></a>{plan}{use_by}{missing}</span>
<div class="actions">{outcome}{deselect}{people}</div>{dialog}
</li>"#,
                id = s.recipe_id,
                name = html_escape(&s.recipe_name),
                plan = match plan_badge(s.planned_on, s.meal_slot.as_deref()) {
                    badge if badge.is_empty() => badge,
                    badge => format!(" {badge}"),
                },
                use_by = p
                    .use_by
                    .zip(p.days_left)
                    .map(|(date, days)| use_by_badge(date, days))
                    .unwrap_or_default(),
                missing = missing_badge(s.recipe_id, p.missing.len()),
                dialog = missing_dialog(p),
                outcome = outcome_links(s.recipe_id, "planned"),
                deselect = deselect_form(s.recipe_id, "planned"),
                people = servings_form(s.recipe_id, s.servings, "planned", &s.recipe_name),
            )
        })
        .collect();
    let label = match planned.len() {
        1 => "Générer les courses de cette recette".to_string(),
        n => format!("Générer les courses de ces {n} recettes"),
    };
    format!(
        r#"<ul class="list">{rows}</ul>
<div class="actions">
<form method="post" action="/grocery-list/generate">
<button type="submit">{label}</button>
</form>
</div>"#
    )
}

/// « Possibles »: the box of the family's allergens (#825), then the
/// recipes whose every required ingredient is in stock, in the
/// suggestions' order — each with its allergy warning while the box is
/// ticked (`avoid`), and the aim once under them; none unticked.
fn possible_tab_html(possible: &[RecipeSuggestion], avoid: bool) -> String {
    let form = allergy::avoid_form("/recipes", &[("vue", "possibles")], avoid, "Appliquer");
    if possible.is_empty() {
        return format!(
            r#"{form}<p class="muted">Aucune recette ne se fait avec le stock actuel. Une recette apparaît ici quand tous ses ingrédients obligatoires sont en stock.</p>"#
        );
    }
    let rows = possible
        .iter()
        .map(|s| suggestion_html(s, avoid))
        .collect::<String>();
    let aim = allergy::aim_line(
        avoid
            && possible
                .iter()
                .any(|s| allergy::says_something(&s.allergy_warning)),
    );
    format!(
        r#"{form}<ul class="list">{rows}</ul>
{aim}{OFF_ATTRIBUTION}"#
    )
}

/// « Toutes »: the planned recipes in a sentence, the ranked suggestions,
/// then every recipe with its « À réaliser » or « Retirer » form.
fn all_tab_html(
    recipes: &[RecipeResponse],
    planned: &[RecipeSelectionResponse],
    suggestions: &[RecipeSuggestion],
    slot: Option<&Slot>,
    usual: Option<i32>,
) -> String {
    let suggestions_section = if suggestions.is_empty() {
        String::new()
    } else {
        let rows = suggestions
            .iter()
            .map(|s| suggestion_html(s, false))
            .collect::<String>();
        format!(
            r#"<h2>Suggestions</h2>
<p class="muted">Classées selon ce que vous avez en stock, la variété (repas récents), la saison et les avis donnés sur les plats déjà faits (Aimé, Pas aimé).</p>
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
                recipe_row_for(r, selection, slot, usual)
            })
            .collect::<String>();
        format!(r#"<ul class="list">{rows}</ul>"#)
    };

    format!(
        r#"{planned_section}
{suggestions_section}
<h2>Toutes les recettes</h2>
{list_section}"#,
        planned_section = planned_section(planned),
    )
}

/// A `GET …/recipes/suggestions` (`path` carries the query). A non-200
/// (e.g. 403) reads as no suggestion rather than leaking JSON; `Err` on a
/// transport failure only.
pub(super) async fn fetch_suggestions(
    state: &AppState,
    path: &str,
    cookie: Option<&str>,
) -> Result<Vec<RecipeSuggestion>, ()> {
    match api_request_auth(state, reqwest::Method::GET, path, cookie, None).await {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            Ok(serde_json::from_value::<SuggestionList>(resp.body)
                .map(|l| l.suggestions)
                .unwrap_or_default())
        }
        Ok(_) => Ok(Vec::new()),
        Err(_) => Err(()),
    }
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

    let tab = Tab::from_query(query.vue.as_deref());

    // The full list, needed on every tab: its length bounds the possible
    // recipes asked for below, so that their count is exact. A non-200
    // reads as no recipe; a transport error is a hard failure.
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

    // The selections fail hard on any status but 200, which makes them
    // unknown, not empty (`planned_selections`). The possible recipes and
    // the suggestions below fail hard on a transport error only, like the
    // list; a non-200 reads as none (`fetch_suggestions`).
    let Ok(with_stock) = planned_with_stock(&state, fam.gid, cookie.as_deref()).await else {
        return service_unavailable_page().into_response();
    };
    let planned: Vec<RecipeSelectionResponse> =
        with_stock.iter().map(|p| p.selection.clone()).collect();

    // The recipes the stock covers (#539), for the « Possibles » tab and
    // its count on every tab, all of them; the family's allergens kept out
    // unless the box of « Possibles » came unticked (#825).
    let avoid = allergy::avoid_from(
        query.avoid_form.as_deref(),
        query.avoid_allergens.as_deref(),
    );
    let possible = if recipes.is_empty() {
        Vec::new()
    } else {
        let path = format!(
            "/groups/{}/recipes/suggestions?possible=true&limit={}{}",
            fam.gid,
            recipes.len(),
            if avoid { "" } else { "&avoid_allergens=false" },
        );
        match fetch_suggestions(&state, &path, cookie.as_deref()).await {
            Ok(list) => list,
            Err(()) => return service_unavailable_page().into_response(),
        }
    };

    let notice = notice_html(query.notice.as_deref(), query.servings.as_deref());
    let error = query
        .error
        .as_deref()
        .and_then(error_text)
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    // A recipe marked done took from the stock (#560): the planned ones it
    // left incomplete are told, with the way to « Prévues » — except on
    // « Prévues », whose rows say it themselves.
    let incomplete = if query.notice.as_deref() == Some("recipe_done") && tab != Tab::Planned {
        incomplete_banner(incomplete_count(&with_stock))
    } else {
        String::new()
    };

    // Opened from an empty slot of the planning (#813), on the full list.
    let slot = match tab {
        Tab::All => {
            let today = crate::routes::agenda::today_paris();
            slot_from_query(query.date, query.repas.as_deref(), today)
                .map(|(day, meal)| Slot::new(day, meal, today))
        }
        _ => None,
    };
    let slot_notice = slot_banner(slot.as_ref());

    let content = match tab {
        Tab::Planned => planned_tab_html(&with_stock),
        Tab::Possible => possible_tab_html(&possible, avoid),
        Tab::All | Tab::Week => {
            // The ranked suggestions, on this tab only.
            let path = format!("/groups/{}/recipes/suggestions", fam.gid);
            let Ok(suggestions) = fetch_suggestions(&state, &path, cookie.as_deref()).await else {
                return service_unavailable_page().into_response();
            };
            all_tab_html(
                &recipes,
                &planned,
                &suggestions,
                slot.as_ref(),
                usual_servings(&headers, fam.gid),
            )
        }
    };

    let start = start_mark(slot.as_ref(), me.user_id);
    let body = format!(
        r#"{segment}<div class="page-header"{start}>
<h1>Recettes</h1>
<div class="actions">
<a class="btn secondary" href="/recipes/import">Importer depuis une URL</a>
<a class="btn" href="/recipes/new">Nouvelle recette</a>
</div>
</div>
{notice}{error}{incomplete}
{tabs}{slot_notice}
{content}"#,
        segment = kitchen_segment("/recipes"),
        tabs = tabs_html(tab, Some((planned.len(), possible.len()))),
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

    /// A row of the full list opened without a slot.
    fn recipe_row_html(r: &RecipeResponse, planned: Option<&RecipeSelectionResponse>) -> String {
        recipe_row_for(r, planned, None, None)
    }

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
            duration_minutes: None,
            required_equipment: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            allergy_warning: Default::default(),
        }
    }

    fn with_stock(s: RecipeSelectionResponse) -> PlannedSelection {
        PlannedSelection {
            selection: s,
            missing: Vec::new(),
            use_by: None,
            days_left: None,
            allergy_warning: Default::default(),
        }
    }

    fn selection(recipe_id: u128, name: &str, servings: Option<i32>) -> RecipeSelectionResponse {
        RecipeSelectionResponse {
            id: Uuid::from_u128(recipe_id + 1000),
            recipe_id: Uuid::from_u128(recipe_id),
            recipe_name: name.into(),
            status: "planned".into(),
            servings,
            planned_on: None,
            meal_slot: None,
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
    fn a_recipe_not_planned_offers_the_households_usual_number_once_given() {
        // #820: the answer to « Combien de personnes mangent d'habitude ? »
        // comes before the recipe's own base.
        let html = recipe_row_for(&recipe(1, "Crêpes", Some(4)), None, None, Some(2));
        assert!(html.contains(r#"value="2""#), "{html}");
        assert!(!html.contains(r#"value="4""#), "{html}");
    }

    #[test]
    fn a_planned_recipe_shows_its_badge_and_retirer() {
        let html = recipe_row_html(
            &recipe(1, "Crêpes", Some(4)),
            Some(&selection(1, "Crêpes", Some(6))),
        );
        let id = Uuid::from_u128(1);
        assert!(
            html.contains(r#"<span class="badge">À réaliser</span>"#),
            "{html}"
        );
        assert!(
            html.contains(&format!(r#"action="/recipes/{id}/deselect""#)),
            "{html}"
        );
        // No « À réaliser » form: the recipe is planned already.
        assert!(!html.contains(">À réaliser</button>"), "{html}");
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
    fn a_planned_recipe_of_the_full_list_changes_its_people_in_its_field() {
        // #846: after « Retirer », the field of the selection, back to the
        // list; the badge no longer says the number.
        let html = recipe_row_html(
            &recipe(1, "Crêpes", Some(4)),
            Some(&selection(1, "Crêpes", Some(6))),
        );
        let id = Uuid::from_u128(1);
        assert!(!html.contains("6 personnes"), "{html}");
        let field = super::super::servings_form(id, Some(6), "list", "Crêpes");
        let at = html.find(&field).expect(&html);
        assert!(html.find(">Retirer<").unwrap() < at, "{html}");
        // The actions are a block: a form may not sit in a `<span>`.
        assert!(html.contains(r#"<div class="actions">"#), "{html}");
        assert!(!html.contains(r#"<span class="actions">"#), "{html}");
        let html = recipe_row_html(
            &recipe(1, "Crêpes", Some(4)),
            Some(&selection(1, "Crêpes", None)),
        );
        assert!(
            html.contains(&super::super::servings_form(id, None, "list", "Crêpes")),
            "{html}"
        );
    }

    #[test]
    fn a_planned_recipe_of_the_planned_tab_changes_its_people_in_its_field() {
        // The validated mock-up's default: « Prévues » gets the field too,
        // in place of the muted « 6 personnes ».
        let html = planned_tab_html(&[with_stock(selection(1, "Crêpes", Some(6)))]);
        let id = Uuid::from_u128(1);
        assert!(!html.contains("6 personnes"), "{html}");
        let field = super::super::servings_form(id, Some(6), "planned", "Crêpes");
        let at = html.find(&field).expect(&html);
        assert!(html.find(">Retirer<").unwrap() < at, "{html}");
        assert!(!html.contains(r#"<span class="actions">"#), "{html}");
    }

    #[test]
    fn a_servings_change_is_told_on_the_list() {
        assert_eq!(
            notice_html(Some("servings_updated"), Some("8")),
            r#"<p class="notice success" role="status">Nombre de personnes mis à jour : 8.</p>"#
        );
        assert_eq!(
            notice_html(Some("recipe_deleted"), None),
            r#"<p class="notice success">Recette supprimée.</p>"#
        );
        assert_eq!(notice_html(Some("inconnu"), None), "");
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

    fn suggestion(id: u128, name: &str, matched: usize, total: usize) -> RecipeSuggestion {
        RecipeSuggestion {
            recipe_id: Uuid::from_u128(id),
            name: name.into(),
            score: 100.0,
            matched_ingredients: matched,
            total_required_ingredients: total,
            missing_ingredients: Vec::new(),
            recently_eaten: false,
            last_eaten_on: None,
            duration_minutes: None,
            allergy_warning: Default::default(),
        }
    }

    #[test]
    fn the_tab_comes_from_the_url_and_defaults_to_all() {
        assert!(Tab::from_query(None) == Tab::All);
        assert!(Tab::from_query(Some("")) == Tab::All);
        assert!(Tab::from_query(Some("prevues")) == Tab::Planned);
        assert!(Tab::from_query(Some("possibles")) == Tab::Possible);
        // Anything else is the full list, never an error page.
        assert!(Tab::from_query(Some("PREVUES")) == Tab::All);
        assert!(Tab::from_query(Some("autre")) == Tab::All);
    }

    #[test]
    fn the_tabs_count_the_planned_and_possible_recipes_and_mark_the_current_one() {
        assert_eq!(
            tabs_html(Tab::All, Some((2, 0))),
            r#"<nav class="filters" aria-label="Groupes de recettes"><a href="/recipes" aria-current="true">Toutes</a><a href="/recipes?vue=prevues">Prévues · 2</a><a href="/recipes?vue=possibles">Possibles · 0</a><a href="/recipes/week">Planning</a></nav>"#
        );
        let html = tabs_html(Tab::Possible, Some((0, 3)));
        assert!(
            html.contains(
                r#"<a href="/recipes?vue=possibles" aria-current="true">Possibles · 3</a>"#
            ),
            "{html}"
        );
        assert_eq!(html.matches("aria-current").count(), 1, "{html}");
        let html = tabs_html(Tab::Planned, Some((1, 0)));
        assert!(
            html.contains(r#"<a href="/recipes?vue=prevues" aria-current="true">Prévues · 1</a>"#),
            "{html}"
        );
    }

    #[test]
    fn the_planning_is_the_fourth_filter_and_lit_on_its_page() {
        let html = tabs_html(Tab::Week, Some((3, 1)));
        assert!(
            html.contains(r#"<a href="/recipes/week" aria-current="true">Planning</a>"#),
            "{html}"
        );
        assert_eq!(html.matches("aria-current").count(), 1, "{html}");
        assert!(
            html.contains("Prévues · 3") && html.contains("Possibles · 1"),
            "{html}"
        );
        // Counts unknown (the planning could not be read): the names alone.
        let html = tabs_html(Tab::Week, None);
        assert!(
            html.contains(r#"<a href="/recipes?vue=prevues">Prévues</a>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<a href="/recipes?vue=possibles">Possibles</a>"#),
            "{html}"
        );
    }

    #[test]
    fn the_first_questions_open_from_the_list_but_not_from_a_slot() {
        let user = Uuid::from_u128(7);
        assert_eq!(start_mark(None, user), format!(r#" data-depart="{user}""#));
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        let slot = Slot::new(today, "dinner", today);
        assert_eq!(start_mark(Some(&slot), user), "");
    }

    #[test]
    fn a_slot_of_the_planning_names_itself_and_rides_on_a_realiser() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        let slot = Some(Slot::new(today, "dinner", today));
        let html = slot_banner(slot.as_ref());
        assert!(
            html.contains("Pour le <strong>samedi 14 novembre, soir</strong>"),
            "{html}"
        );
        assert!(
            html.contains(r#"<a href="/recipes/week">Retour au planning</a>"#),
            "{html}"
        );
        assert_eq!(slot_banner(None), "");
        // A recipe not planned: « À réaliser » carries the slot, back to
        // the window of its day.
        let html = recipe_row_for(&recipe(1, "Crêpes", Some(4)), None, slot.as_ref(), None);
        assert!(
            html.contains(r#"name="planned_on" value="2026-11-14""#),
            "{html}"
        );
        assert!(
            html.contains(r#"name="meal_slot" value="dinner""#),
            "{html}"
        );
        assert!(html.contains(r#"name="back" value="week""#), "{html}");
        let later = Some(Slot::new(
            chrono::NaiveDate::from_ymd_opt(2026, 11, 30).unwrap(),
            "lunch",
            today,
        ));
        let html = recipe_row_for(&recipe(1, "Crêpes", Some(4)), None, later.as_ref(), None);
        assert!(
            html.contains(r#"name="back" value="week@2026-11-27""#),
            "{html}"
        );
        let html = slot_banner(later.as_ref());
        assert!(
            html.contains(r#"<a href="/recipes/week?date=2026-11-27">Retour au planning</a>"#),
            "{html}"
        );
        // A planned one keeps its row: placing it is « À placer »'s.
        let html = recipe_row_for(
            &recipe(1, "Crêpes", Some(4)),
            Some(&selection(1, "Crêpes", Some(6))),
            slot.as_ref(),
            None,
        );
        assert!(!html.contains("planned_on"), "{html}");
        // Without a slot, the plain form.
        let html = recipe_row_for(&recipe(1, "Crêpes", Some(4)), None, None, None);
        assert!(!html.contains("planned_on"), "{html}");
        assert!(html.contains(r#"name="back" value="list""#), "{html}");
    }

    #[test]
    fn the_planned_tab_lists_each_recipe_with_faite_abandonner_and_retirer() {
        let html = planned_tab_html(&[
            with_stock(selection(1, "Crêpes", Some(6))),
            with_stock(selection(2, "Soupe <du jour>", None)),
        ]);
        let id = Uuid::from_u128(1);
        assert!(
            html.contains(&format!(
                r#"<a href="/recipes/{id}"><strong>Crêpes</strong></a>"#
            )),
            "{html}"
        );
        assert!(
            html.contains(r#"value="6" aria-label="Personnes, Crêpes""#),
            "{html}"
        );
        assert!(html.contains("Soupe &lt;du jour&gt;"), "{html}");
        // Each comes back to this tab.
        assert!(
            html.contains(&format!(r#"href="/recipes/{id}/done?back=planned""#)),
            "{html}"
        );
        assert!(
            html.contains(&format!(r#"href="/recipes/{id}/abandon?back=planned""#)),
            "{html}"
        );
        assert!(
            html.contains(&format!(r#"action="/recipes/{id}/deselect""#)),
            "{html}"
        );
        assert!(html.contains(r#"name="back" value="planned""#), "{html}");
        // The way to the grocery list, which takes these recipes (#470).
        assert!(
            html.contains(r#"<form method="post" action="/grocery-list/generate">"#),
            "{html}"
        );
        assert!(
            html.contains(">Générer les courses de ces 2 recettes</button>"),
            "{html}"
        );
        let one = planned_tab_html(&[with_stock(selection(1, "Crêpes", None))]);
        assert!(
            one.contains(">Générer les courses de cette recette</button>"),
            "{one}"
        );
    }

    #[test]
    fn a_complete_undated_planned_recipe_carries_no_badge_and_no_dialog() {
        let html = planned_tab_html(&[with_stock(selection(1, "Crêpes", Some(6)))]);
        assert!(!html.contains("badge"), "{html}");
        assert!(!html.contains("<dialog"), "{html}");
        assert!(!html.contains("Il manque"), "{html}");
    }

    #[test]
    fn a_planned_recipe_lacking_something_opens_its_list_and_stays_faite_able() {
        let mut p = with_stock(selection(1, "Crêpes", Some(6)));
        p.missing = vec![
            MissingIngredient {
                name: "Lait".into(),
                quantity: Some(300.0),
                unit: Some("ml".into()),
            },
            MissingIngredient {
                name: "Sel".into(),
                quantity: None,
                unit: None,
            },
        ];
        let id = Uuid::from_u128(1);
        let html = planned_tab_html(&[p]);
        assert!(html.contains(&missing_badge(id, 2)), "{html}");
        assert!(html.contains(">Il manque 2 éléments</a>"), "{html}");
        assert!(
            html.contains(&format!(r#"<dialog id="missing-{id}""#)),
            "{html}"
        );
        assert!(html.contains("<li>Lait — 300 ml</li>"), "{html}");
        // A warning: « Faite » is still there.
        assert!(
            html.contains(&format!(r#"href="/recipes/{id}/done?back=planned""#)),
            "{html}"
        );
    }

    #[test]
    fn the_dated_planned_recipes_say_by_when_in_the_order_received() {
        let date = |d| chrono::NaiveDate::from_ymd_opt(2026, 11, d).unwrap();
        let mut soon = with_stock(selection(1, "Gratin", None));
        soon.use_by = Some(date(2));
        soon.days_left = Some(1);
        let mut past = with_stock(selection(2, "Soupe", None));
        past.use_by = Some(date(1));
        past.days_left = Some(-1);
        let plain = with_stock(selection(3, "Crêpes", None));
        let html = planned_tab_html(&[soon, past, plain]);
        assert!(
            html.contains("À faire avant le 02/11 (sous 1 jour)</span>"),
            "{html}"
        );
        assert!(
            html.contains("Produit périmé depuis le 01/11</span>"),
            "{html}"
        );
        let at = |name: &str| html.find(name).unwrap();
        assert!(
            at("Gratin") < at("Soupe") && at("Soupe") < at("Crêpes"),
            "{html}"
        );
    }

    #[test]
    fn an_empty_planned_tab_offers_no_generation() {
        let html = planned_tab_html(&[]);
        assert!(html.contains("Aucune recette prévue."), "{html}");
        assert!(html.contains(r#"href="/recipes""#), "{html}");
        assert!(!html.contains("/grocery-list/generate"), "{html}");
    }

    #[test]
    fn the_possible_tab_lists_the_recipes_the_stock_covers() {
        let html = possible_tab_html(&[suggestion(1, "Pain <maison>", 2, 2)], true);
        let id = Uuid::from_u128(1);
        assert!(
            html.contains(&format!(
                r#"<a href="/recipes/{id}"><strong>Pain &lt;maison&gt;</strong></a>"#
            )),
            "{html}"
        );
        assert!(html.contains("Tous les ingrédients en stock"), "{html}");
        // The matching goes through Open Food Facts' categories (#406).
        assert!(html.contains(OFF_ATTRIBUTION), "{html}");
        let empty = possible_tab_html(&[], true);
        assert!(
            empty.contains("Aucune recette ne se fait avec le stock actuel."),
            "{empty}"
        );
        assert!(!empty.contains(OFF_ATTRIBUTION), "{empty}");
        // The box of the allergens, even with nothing listed (#825).
        assert!(empty.contains(r#"<form class="avoid""#), "{empty}");
    }

    fn warned(mut s: RecipeSuggestion, json: serde_json::Value) -> RecipeSuggestion {
        s.allergy_warning = serde_json::from_value(json).unwrap();
        s
    }

    #[test]
    fn the_possible_tab_carries_the_box_ticked_and_each_warning() {
        let possible = [
            warned(
                suggestion(1, "Crêpes", 2, 2),
                serde_json::json!({"mine": [{"tag": "en:milks", "label": "Laits"}]}),
            ),
            warned(
                suggestion(2, "Sauté", 1, 1),
                serde_json::json!({"unverifiable": ["sauce soja"]}),
            ),
            suggestion(3, "Pâtes", 1, 1),
        ];
        let html = possible_tab_html(&possible, true);
        assert!(
            html.starts_with(r#"<form class="avoid" method="get" action="/recipes"><input type="hidden" name="vue" value="possibles"/>"#),
            "{html}"
        );
        assert!(html.contains("data-submit-on-change checked/>"), "{html}");
        assert_eq!(
            html.matches(r#"<div class="allergy sm" role="status">"#)
                .count(),
            2,
            "{html}"
        );
        assert!(
            html.contains("Contient lait : vous y êtes allergique."),
            "{html}"
        );
        assert!(
            html.contains("pas pu être vérifiés pour les allergies : sauce soja."),
            "{html}"
        );
        // The aim once, under the list.
        assert_eq!(html.matches(super::allergy::AIM).count(), 1, "{html}");
        // Nothing to warn of: no aim either.
        let html = possible_tab_html(&[suggestion(3, "Pâtes", 1, 1)], true);
        assert!(!html.contains("Aide uniquement"), "{html}");
    }

    #[test]
    fn unticked_the_possible_tab_shows_no_allergy_warning_at_all() {
        // Arbitration of 2026-11-17: unticked, the recipes come back
        // without any banner, whatever the kind.
        let possible = [warned(
            suggestion(1, "Crêpes", 2, 2),
            serde_json::json!({
                "mine": [{"tag": "en:milks", "label": "Laits"}],
                "public": [{"member": "Léa", "tag": "en:eggs", "label": "Œufs"}],
                "anonymous": true,
                "unverifiable": ["sauce soja"],
            }),
        )];
        let html = possible_tab_html(&possible, false);
        assert!(html.contains("Crêpes"), "{html}");
        assert!(!html.contains(r#"class="allergy"#), "{html}");
        assert!(!html.contains("Aide uniquement"), "{html}");
        assert!(!html.contains(" checked"), "{html}");
        assert!(
            html.contains(r#"name="avoid_allergens" value="true""#),
            "{html}"
        );
    }

    #[test]
    fn the_suggestions_of_toutes_carry_no_banner() {
        let s = warned(
            suggestion(1, "Crêpes", 2, 2),
            serde_json::json!({"anonymous": true}),
        );
        let html = all_tab_html(&[], &[], &[s], None, None);
        assert!(html.contains("Crêpes"), "{html}");
        assert!(!html.contains(r#"class="allergy"#), "{html}");
    }

    #[test]
    fn an_empty_planned_section_says_how_to_plan_one() {
        let html = planned_section(&[]);
        assert!(html.contains(r#"id="a-realiser""#), "{html}");
        assert!(html.contains("Aucune recette à réaliser."), "{html}");
    }

    fn dated(
        mut s: RecipeSelectionResponse,
        planned_on: (i32, u32, u32),
        meal_slot: Option<&str>,
    ) -> RecipeSelectionResponse {
        s.planned_on = chrono::NaiveDate::from_ymd_opt(planned_on.0, planned_on.1, planned_on.2);
        s.meal_slot = meal_slot.map(str::to_string);
        s
    }

    #[test]
    fn the_full_list_shows_the_day_and_the_meal_of_a_dated_recipe() {
        let r = recipe(1, "Crêpes", Some(4));
        // Without a day, nothing more than before (#812).
        let html = recipe_row_html(&r, Some(&selection(1, "Crêpes", Some(6))));
        assert!(!html.contains("badge accent"), "{html}");
        let html = recipe_row_html(
            &r,
            Some(&dated(
                selection(1, "Crêpes", Some(6)),
                (2026, 11, 21),
                None,
            )),
        );
        assert!(
            html.contains(
                r#"<span class="badge">À réaliser</span><span class="badge accent">sam. 21/11</span>"#
            ),
            "{html}"
        );
        let html = recipe_row_html(
            &r,
            Some(&dated(
                selection(1, "Crêpes", Some(6)),
                (2026, 11, 19),
                Some("lunch"),
            )),
        );
        assert!(
            html.contains(r#"<span class="badge accent">jeu. 19/11 · midi</span>"#),
            "{html}"
        );
    }

    #[test]
    fn the_planned_tab_shows_the_day_and_the_meal_after_the_name() {
        let id = Uuid::from_u128(1);
        let html = planned_tab_html(&[with_stock(dated(
            selection(1, "Crêpes", Some(6)),
            (2026, 11, 19),
            Some("lunch"),
        ))]);
        assert!(
            html.contains(&format!(
                r#"<a href="/recipes/{id}"><strong>Crêpes</strong></a> <span class="badge accent">jeu. 19/11 · midi</span>"#
            )),
            "{html}"
        );
        let html = planned_tab_html(&[with_stock(dated(
            selection(1, "Crêpes", Some(6)),
            (2026, 11, 21),
            None,
        ))]);
        assert!(
            html.contains(r#"</a> <span class="badge accent">sam. 21/11</span>"#),
            "{html}"
        );
    }
}
