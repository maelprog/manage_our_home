//! `/recipes/:id/replace` — « Changer ce plat » (#816), the page of the
//! API's replacements (#815). Reached from a planned recipe on the
//! planning (`/recipes/week`, its slots and « À placer ») and from the
//! « À réaliser » box of its page; `?back=` names the page it came from,
//! as the selection forms do (`recipes::back_path`).
//!
//! `GET` shows what is replaced — its name, day, meal and number of people
//! — and one to three recipes to plan instead (`GET …/selection/
//! replacements`), each with its duration and what the stock lacks of it;
//! none left is said, and nothing is changed. « Choisir » posts the
//! recipe chosen (`POST …/selection/replacement`), which keeps the day,
//! the meal and the number of people; the page it came from then says
//! « X remplacé par Y. » — the planning on its window, or the page of the
//! new recipe. Without JavaScript throughout.
//!
//! Since #825 « Propositions » opens on the box « Éviter les recettes
//! contenant les allergènes de la famille », ticked by default; unticked,
//! the replacements are asked with `avoid_allergens=false` for that request
//! and no offer shows its allergy warning, ticked each shows its own.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::recipes::{
    RecipeResponse, RecipeSelectionResponse, RecipeSuggestion, ReplacementList,
};
use manage_our_home_shared::validation::recipes::{duration_label, servings_label};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::detail::plan_sentence;
use super::{
    allergy, back_path, back_with, family_context, planned_with_stock, recipes_cookie, week,
};

/// The address of the page, from the page `back` names.
pub(crate) fn replace_href(recipe_id: Uuid, back: &str) -> String {
    format!("/recipes/{recipe_id}/replace?back={back}")
}

/// « Changer ce plat » under a recipe of the planning, named for the ear
/// by `what`: the recipe, and its day and meal when it has them.
pub(crate) fn change_link(recipe_id: Uuid, back: &str, what: &str) -> String {
    format!(
        r#"<a class="change" href="{href}" aria-label="Changer ce plat : {what}">Changer ce plat</a>"#,
        href = html_escape(&replace_href(recipe_id, back)),
        what = html_escape(what),
    )
}

/// The confirmation, « X remplacé par Y. »; a name that could not be read
/// is left out rather than guessed.
pub(crate) fn replaced_notice(from: Option<&str>, to: Option<&str>) -> String {
    let from = from.unwrap_or("Plat");
    match to {
        Some(to) => format!("{from} remplacé par {to}."),
        None => format!("{from} remplacé."),
    }
}

/// Where a replacement done sends the member: the planning on its window
/// when they came from it, the page of the new recipe otherwise — with
/// the two recipes, which the confirmation names.
pub(crate) fn done_target(back: &str, old: Uuid, new: Uuid) -> String {
    let param = format!("notice=recipe_replaced&from={old}&to={new}");
    if week::back_window(back).is_some() {
        back_with(back, new, &param)
    } else {
        format!("/recipes/{new}?{param}")
    }
}

/// The `?error=` of this page after a refused `POST`, from the API's
/// status (#815): the recipe chosen planned meanwhile (409), gone or the
/// selection gone (404, which `get` tells apart), the recipe replaced by
/// itself (400), the right refused (403).
pub(crate) fn refusal_code(status: reqwest::StatusCode) -> &'static str {
    match status {
        reqwest::StatusCode::CONFLICT => "already_planned",
        reqwest::StatusCode::NOT_FOUND => "not_found",
        reqwest::StatusCode::BAD_REQUEST => "same_recipe",
        reqwest::StatusCode::FORBIDDEN => "forbidden",
        _ => "unavailable",
    }
}

/// What the page says of `?error=`, `tried` the name of the recipe that
/// was already planned, when it could be read.
pub(crate) fn error_sentence(code: &str, tried: Option<&str>) -> Option<String> {
    const CHOOSE: &str = "Rien n'a été modifié : choisissez une autre proposition.";
    Some(match code {
        "already_planned" => match tried {
            Some(name) => format!("« {name} » est déjà prévue. {CHOOSE}"),
            None => format!("Cette recette est déjà prévue. {CHOOSE}"),
        },
        "not_found" => format!("Cette recette n'existe plus. {CHOOSE}"),
        "same_recipe" => format!("C'est la recette déjà prévue. {CHOOSE}"),
        "forbidden" => {
            "Vous n'avez pas les droits nécessaires pour cette action. Rien n'a été modifié."
                .to_string()
        }
        "unavailable" => {
            "Le remplacement n'a pas pu être enregistré. Rien n'a été modifié : merci de réessayer."
                .to_string()
        }
        _ => return None,
    })
}

/// What the page knows of the replacements: those offered, or none read.
pub(crate) enum Offer<'a> {
    Ready(&'a [RecipeSuggestion]),
    Unavailable,
}

pub(crate) struct ReplaceView<'a> {
    pub recipe_id: Uuid,
    pub back: &'a str,
    /// The planned selection replaced; `None` when the selections could
    /// not be read.
    pub selection: Option<&'a RecipeSelectionResponse>,
    pub offer: Offer<'a>,
    /// The sentence of `?error=`.
    pub error: Option<&'a str>,
    /// The allergens' box (#825): ticked, the offers keep the family's
    /// allergens out and show their warnings; unticked, neither.
    pub avoid: bool,
}

/// « Vendredi 13 novembre, au dîner », or « Sans jour pour l'instant ».
fn when_said(s: &RecipeSelectionResponse) -> String {
    match s.planned_on {
        Some(day) => {
            let sentence = plan_sentence(day, s.meal_slot.as_deref());
            let mut chars = sentence.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
        None => "Sans jour pour l'instant".to_string(),
    }
}

/// « Vous remplacez »: the recipe, its day and meal, its number of people,
/// and what of them the replacement keeps.
fn context_html(s: &RecipeSelectionResponse) -> String {
    let people = s
        .servings
        .map(|n| format!(" · {}", servings_label(n)))
        .unwrap_or_default();
    let kept = match (s.planned_on, s.servings) {
        (Some(_), _) => "Le jour, le repas et le nombre de personnes sont conservés.",
        (None, Some(_)) => {
            "Le nombre de personnes est conservé ; le jour et le repas restent à choisir."
        }
        (None, None) => "Le jour et le repas restent à choisir.",
    };
    format!(
        r#"<section class="card ctx" aria-labelledby="ctx-h"><p class="eyebrow" id="ctx-h">Vous remplacez</p><p class="ctx-name">{name}</p><p class="muted">{when}{people}</p><p class="muted">{kept}</p></section>"#,
        name = html_escape(&s.recipe_name),
        when = when_said(s),
    )
}

/// One offer: a form of its own, the recipe's name over its duration and
/// what the stock lacks of it — nothing of the stock for a recipe without
/// a required ingredient —, its allergy warning when `warn` (#825), then
/// « Choisir ».
fn offer_html(recipe_id: Uuid, back: &str, o: &RecipeSuggestion, warn: bool) -> String {
    let duration = o
        .duration_minutes
        .map(|m| format!("<span>{}</span>", duration_label(m)))
        .unwrap_or_default();
    let stock = match (o.total_required_ingredients, o.missing_ingredients.len()) {
        (0, _) => String::new(),
        (_, 0) => r#"<span class="badge ok">Tout est en stock</span>"#.to_string(),
        (_, 1) => r#"<span class="badge soon">Il manque 1 élément</span>"#.to_string(),
        (_, n) => format!(r#"<span class="badge soon">Il manque {n} éléments</span>"#),
    };
    let meta = if duration.is_empty() && stock.is_empty() {
        String::new()
    } else {
        format!(r#"<span class="meta">{duration}{stock}</span>"#)
    };
    let allergy = if warn {
        allergy::banner(&o.allergy_warning, true, false)
    } else {
        String::new()
    };
    format!(
        r#"<li class="card prop"><form method="post" action="/recipes/{recipe_id}/replace"><input type="hidden" name="back" value="{back}"/><input type="hidden" name="replacement" value="{id}"/><div class="what"><span class="name">{name}</span>{meta}</div>{allergy}<button type="submit">Choisir<span class="sr-only"> {name}</span></button></form></li>"#,
        back = html_escape(back),
        id = o.recipe_id,
        name = html_escape(&o.name),
    )
}

/// The page below its title: the error of a refused choice, what is
/// replaced, then the offers — or that none is left, or that they could
/// not be read — and « Annuler ».
pub(crate) fn replace_html(view: &ReplaceView) -> String {
    let cancel = format!(
        r#"<div class="actions"><a class="btn secondary" href="{}">Annuler</a></div>"#,
        html_escape(&back_path(view.back, view.recipe_id))
    );
    let context = view.selection.map(context_html).unwrap_or_default();
    let alert = |text: &str| {
        format!(
            r#"<p class="notice error" role="alert">{}</p>"#,
            html_escape(text)
        )
    };
    let error = view.error.map(alert).unwrap_or_default();
    // The allergens' box (#825), over the offers or their absence.
    let avoid = allergy::avoid_form(
        &format!("/recipes/{}/replace", view.recipe_id),
        &[("back", view.back)],
        view.avoid,
        "Actualiser",
    );
    let offers = match view.offer {
        Offer::Unavailable => format!(
            r#"<h2>Propositions</h2>{alert}<div class="actions"><a class="btn" href="{href}">Réessayer</a></div>"#,
            alert = alert(
                "Impossible de charger les propositions pour le moment. Rien n'a été modifié."
            ),
            href = html_escape(&replace_href(view.recipe_id, view.back)),
        ),
        Offer::Ready([]) => {
            let kept = match view.selection {
                Some(s) => {
                    let name = html_escape(&s.recipe_name);
                    match s.planned_on {
                        Some(day) => format!(
                            " : « {name} » reste prévu le {}",
                            plan_sentence(day, s.meal_slot.as_deref())
                        ),
                        None => format!(" : « {name} » reste à réaliser, sans jour"),
                    }
                }
                None => String::new(),
            };
            format!(
                r#"<h2>Propositions</h2>{avoid}<div class="card"><p><strong>Aucune autre recette à proposer pour l'instant.</strong></p><p class="muted">Toutes vos autres recettes sont déjà prévues, ou aucune n'est enregistrée. Rien n'a été modifié{kept}.</p><div class="actions"><a class="btn" href="/recipes">Voir les recettes</a></div></div>"#
            )
        }
        Offer::Ready(list) => {
            let title = if list.len() == 1 {
                "Proposition"
            } else {
                "Propositions"
            };
            let items: String = list
                .iter()
                .map(|o| offer_html(view.recipe_id, view.back, o, view.avoid))
                .collect();
            let aim = allergy::aim_line(
                view.avoid
                    && list
                        .iter()
                        .any(|o| allergy::says_something(&o.allergy_warning)),
            );
            format!(
                r#"<h2>{title}</h2><p class="muted">Classées d'après votre stock, ce qui a été cuisiné récemment et la saison. La première est la mieux placée.</p>{avoid}{aim}<ol class="props">{items}</ol>"#
            )
        }
    };
    format!("{error}{context}{offers}{cancel}")
}

#[derive(serde::Deserialize)]
pub struct ReplaceQuery {
    #[serde(default)]
    back: String,
    error: Option<String>,
    /// The recipe found already planned (`error=already_planned`).
    tried: Option<String>,
    /// The allergens' box (#825): its form was sent, and whether the box
    /// was ticked (`allergy::avoid_from`).
    avoid_form: Option<String>,
    avoid_allergens: Option<String>,
}

/// The name of one of the family's recipes, `None` when it cannot be read.
pub(crate) async fn recipe_name(
    state: &AppState,
    gid: Uuid,
    recipe_id: Uuid,
    cookie: Option<&str>,
) -> Option<String> {
    let resp = api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/recipes/{recipe_id}"),
        cookie,
        None,
    )
    .await
    .ok()?;
    if resp.status != reqwest::StatusCode::OK {
        return None;
    }
    serde_json::from_value::<RecipeResponse>(resp.body)
        .ok()
        .map(|r| r.name)
}

/// `GET /recipes/:id/replace`. A recipe that is no longer planned sends
/// the member back where they came from, with `selection_gone`.
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
    Query(query): Query<ReplaceQuery>,
) -> Response {
    let back = query.back.as_str();
    let Some(fam) = family_context(&state, &headers, &me, &replace_href(recipe_id, back)).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);
    let gone = || Redirect::to(&back_with(back, recipe_id, "error=selection_gone")).into_response();
    let avoid = allergy::avoid_from(
        query.avoid_form.as_deref(),
        query.avoid_allergens.as_deref(),
    );

    let planned = planned_with_stock(&state, fam.gid, cookie.as_deref()).await;
    let selection = match &planned {
        Ok(list) => match list.iter().find(|p| p.selection.recipe_id == recipe_id) {
            Some(p) => Some(p.selection.clone()),
            None => return gone(),
        },
        Err(()) => None,
    };
    let offers = match selection {
        None => None,
        Some(_) => match api_request_auth(
            &state,
            reqwest::Method::GET,
            &format!(
                "/groups/{}/recipes/{}/selection/replacements{}",
                fam.gid,
                recipe_id,
                if avoid { "" } else { "?avoid_allergens=false" },
            ),
            cookie.as_deref(),
            None,
        )
        .await
        {
            Ok(resp) if resp.status == reqwest::StatusCode::OK => {
                serde_json::from_value::<ReplacementList>(resp.body)
                    .ok()
                    .map(|l| l.replacements)
            }
            Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => return gone(),
            Ok(_) | Err(_) => None,
        },
    };

    let tried = match (query.error.as_deref(), query.tried.as_deref()) {
        (Some("already_planned"), Some(tried)) => match Uuid::parse_str(tried) {
            Ok(tried) => recipe_name(&state, fam.gid, tried, cookie.as_deref()).await,
            Err(_) => None,
        },
        _ => None,
    };
    let error = query
        .error
        .as_deref()
        .and_then(|code| error_sentence(code, tried.as_deref()));
    let body = format!(
        "<h1>Changer ce plat</h1>\n{}",
        replace_html(&ReplaceView {
            recipe_id,
            back,
            selection: selection.as_ref(),
            offer: match &offers {
                Some(list) => Offer::Ready(list),
                None => Offer::Unavailable,
            },
            error: error.as_deref(),
            avoid,
        })
    );
    Html(shell_with_header(
        Width::Read,
        "Changer ce plat",
        &fam.header,
        &body,
    ))
    .into_response()
}

#[derive(serde::Deserialize)]
pub struct ReplaceForm {
    #[serde(default)]
    back: String,
    #[serde(default)]
    replacement: String,
}

/// `POST /recipes/:id/replace`: « Choisir ». Done, back to the page the
/// member came from (`done_target`); refused, this page again with the
/// reason — the selection gone is then said by `get`, where it came from.
pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
    Form(form): Form<ReplaceForm>,
) -> Response {
    let back = form.back.as_str();
    let Some(fam) = family_context(&state, &headers, &me, &back_path(back, recipe_id)).await else {
        return Redirect::to("/groups/new").into_response();
    };
    let again = |code: &str, tried: Option<Uuid>| {
        let tried = tried.map(|t| format!("&tried={t}")).unwrap_or_default();
        Redirect::to(&format!(
            "{}&error={code}{tried}",
            replace_href(recipe_id, back)
        ))
        .into_response()
    };
    let Ok(new) = Uuid::parse_str(form.replacement.trim()) else {
        return again("not_found", None);
    };
    let cookie = recipes_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!(
            "/groups/{}/recipes/{}/selection/replacement",
            fam.gid, recipe_id
        ),
        cookie.as_deref(),
        Some(serde_json::json!({ "recipe_id": new })),
    )
    .await;
    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            Redirect::to(&done_target(back, recipe_id, new)).into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            Redirect::to(&back_with(back, recipe_id, "error=forbidden")).into_response()
        }
        Ok(resp) => {
            let code = refusal_code(resp.status);
            again(code, (code == "already_planned").then_some(new))
        }
        Err(_) => again("unavailable", None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, Utc};
    use manage_our_home_shared::dto::recipes::MissingIngredient;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn selection(
        name: &str,
        on: Option<NaiveDate>,
        meal: Option<&str>,
        servings: Option<i32>,
    ) -> RecipeSelectionResponse {
        RecipeSelectionResponse {
            id: id(100),
            recipe_id: id(1),
            recipe_name: name.into(),
            status: "planned".into(),
            servings,
            planned_on: on,
            meal_slot: meal.map(str::to_string),
            created_by: id(200),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn offer(
        n: u128,
        name: &str,
        minutes: Option<i32>,
        missing: &[&str],
        total: usize,
    ) -> RecipeSuggestion {
        RecipeSuggestion {
            recipe_id: id(n),
            name: name.into(),
            score: 0.0,
            matched_ingredients: total - missing.len(),
            total_required_ingredients: total,
            missing_ingredients: missing
                .iter()
                .map(|m| MissingIngredient {
                    name: m.to_string(),
                    quantity: None,
                    unit: None,
                })
                .collect(),
            recently_eaten: false,
            last_eaten_on: None,
            duration_minutes: minutes,
            allergy_warning: Default::default(),
        }
    }

    fn friday() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 13).unwrap()
    }

    fn render(
        selection: Option<&RecipeSelectionResponse>,
        offer: Offer,
        error: Option<&str>,
    ) -> String {
        replace_html(&ReplaceView {
            recipe_id: id(1),
            back: "week",
            selection,
            offer,
            error,
            avoid: true,
        })
    }

    fn warned(mut o: RecipeSuggestion, json: serde_json::Value) -> RecipeSuggestion {
        o.allergy_warning = serde_json::from_value(json).unwrap();
        o
    }

    fn render_box(offers: &[RecipeSuggestion], avoid: bool) -> String {
        let s = selection("Gratin", Some(friday()), Some("dinner"), Some(4));
        replace_html(&ReplaceView {
            recipe_id: id(1),
            back: "week@2026-11-13",
            selection: Some(&s),
            offer: Offer::Ready(offers),
            error: None,
            avoid,
        })
    }

    // -- the allergens' box and the warnings (#825) ---------------------------

    #[test]
    fn the_offers_open_on_the_box_ticked_back_to_this_page() {
        let html = render_box(&[offer(2, "Crêpes", Some(30), &[], 1)], true);
        let form = format!(
            r#"<form class="avoid" method="get" action="/recipes/{}/replace"><input type="hidden" name="back" value="week@2026-11-13"/><input type="hidden" name="avoid_form" value="1"/>"#,
            id(1)
        );
        assert!(html.contains(&form), "{html}");
        assert!(html.contains("data-submit-on-change checked/>"), "{html}");
        assert!(html.contains(">Actualiser</button>"), "{html}");
        let at = |s: &str| html.find(s).unwrap_or_else(|| panic!("{s} in {html}"));
        assert!(at("<h2>Proposition</h2>") < at(&form));
        assert!(at(&form) < at(r#"<ol class="props">"#));
        // None left: the box stays, to untick it.
        let html = render_box(&[], true);
        assert!(html.contains(&form), "{html}");
    }

    #[test]
    fn ticked_each_offer_shows_its_warning_and_the_aim_once() {
        let offers = [
            warned(
                offer(2, "Crêpes", Some(30), &[], 1),
                serde_json::json!({"mine": [{"tag": "en:milks", "label": "Laits"}]}),
            ),
            warned(
                offer(3, "Sauté", None, &[], 1),
                serde_json::json!({"anonymous": true, "unverifiable": ["sauce soja"]}),
            ),
            offer(4, "Pâtes", None, &[], 1),
        ];
        let html = render_box(&offers, true);
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
            html.contains(
                "Un membre de la famille est allergique à un ingrédient de cette recette."
            ),
            "{html}"
        );
        assert_eq!(html.matches(allergy::AIM).count(), 1, "{html}");
        // The banner sits between the offer's stock and « Choisir ».
        let crepes = &html[html.find("Crêpes").unwrap()..];
        assert!(
            crepes.find(r#"class="allergy sm""#).unwrap() < crepes.find("Choisir").unwrap(),
            "{crepes}"
        );
    }

    #[test]
    fn unticked_no_offer_shows_a_warning() {
        let offers = [warned(
            offer(2, "Crêpes", Some(30), &[], 1),
            serde_json::json!({
                "mine": [{"tag": "en:milks", "label": "Laits"}],
                "unverifiable": ["sauce soja"],
            }),
        )];
        let html = render_box(&offers, false);
        assert!(html.contains("Crêpes"), "{html}");
        assert!(!html.contains(r#"class="allergy"#), "{html}");
        assert!(!html.contains("Aide uniquement"), "{html}");
        assert!(!html.contains(" checked"), "{html}");
    }

    // -- the ways in and out ------------------------------------------------

    #[test]
    fn the_page_is_reached_with_the_way_back() {
        assert_eq!(
            replace_href(id(1), "week"),
            format!("/recipes/{}/replace?back=week", id(1))
        );
        assert_eq!(
            replace_href(id(1), "week@2026-11-20"),
            format!("/recipes/{}/replace?back=week@2026-11-20", id(1))
        );
        assert_eq!(
            replace_href(id(1), "detail"),
            format!("/recipes/{}/replace?back=detail", id(1))
        );
    }

    #[test]
    fn the_link_of_the_planning_names_its_recipe_and_its_slot() {
        let html = change_link(
            id(1),
            "week@2026-11-20",
            "Gratin <dauphinois>, vendredi 13 novembre, soir",
        );
        assert_eq!(
            html,
            format!(
                r#"<a class="change" href="/recipes/{}/replace?back=week@2026-11-20" aria-label="Changer ce plat : Gratin &lt;dauphinois&gt;, vendredi 13 novembre, soir">Changer ce plat</a>"#,
                id(1)
            )
        );
    }

    #[test]
    fn the_confirmation_names_both_recipes() {
        assert_eq!(
            replaced_notice(Some("Gratin dauphinois"), Some("Soupe de potimarron")),
            "Gratin dauphinois remplacé par Soupe de potimarron."
        );
        assert_eq!(
            replaced_notice(None, Some("Soupe")),
            "Plat remplacé par Soupe."
        );
        assert_eq!(replaced_notice(Some("Gratin"), None), "Gratin remplacé.");
        assert_eq!(replaced_notice(None, None), "Plat remplacé.");
    }

    #[test]
    fn a_replacement_returns_to_the_planning_or_to_the_new_recipe() {
        let (old, new) = (id(1), id(2));
        assert_eq!(
            done_target("week", old, new),
            format!("/recipes/week?notice=recipe_replaced&from={old}&to={new}")
        );
        assert_eq!(
            done_target("week@2026-11-20", old, new),
            format!("/recipes/week?date=2026-11-20&notice=recipe_replaced&from={old}&to={new}")
        );
        assert_eq!(
            done_target("detail", old, new),
            format!("/recipes/{new}?notice=recipe_replaced&from={old}&to={new}")
        );
        // Anything else is the new recipe's page, never a path of the form.
        assert_eq!(
            done_target("https://evil.test", old, new),
            format!("/recipes/{new}?notice=recipe_replaced&from={old}&to={new}")
        );
    }

    #[test]
    fn a_refusal_stays_on_the_page_with_its_reason() {
        use reqwest::StatusCode;
        assert_eq!(refusal_code(StatusCode::CONFLICT), "already_planned");
        assert_eq!(refusal_code(StatusCode::NOT_FOUND), "not_found");
        assert_eq!(refusal_code(StatusCode::BAD_REQUEST), "same_recipe");
        assert_eq!(refusal_code(StatusCode::FORBIDDEN), "forbidden");
        assert_eq!(
            refusal_code(StatusCode::INTERNAL_SERVER_ERROR),
            "unavailable"
        );
        assert_eq!(refusal_code(StatusCode::OK), "unavailable");
    }

    #[test]
    fn each_refusal_says_that_nothing_changed() {
        assert_eq!(
            error_sentence("already_planned", Some("Omelette")).as_deref(),
            Some("« Omelette » est déjà prévue. Rien n'a été modifié : choisissez une autre proposition.")
        );
        assert_eq!(
            error_sentence("already_planned", None).as_deref(),
            Some("Cette recette est déjà prévue. Rien n'a été modifié : choisissez une autre proposition.")
        );
        for code in ["not_found", "same_recipe", "unavailable", "forbidden"] {
            let s = error_sentence(code, None).unwrap();
            assert!(s.contains("Rien n'a été modifié"), "{code}: {s}");
        }
        assert_eq!(error_sentence("autre", None), None);
    }

    // -- the page -----------------------------------------------------------

    #[test]
    fn the_page_says_what_is_replaced_and_what_is_kept() {
        let s = selection(
            "Gratin <dauphinois>",
            Some(friday()),
            Some("dinner"),
            Some(4),
        );
        let html = render(Some(&s), Offer::Ready(&[]), None);
        assert!(
            html.contains(r#"<section class="card ctx" aria-labelledby="ctx-h"><p class="eyebrow" id="ctx-h">Vous remplacez</p><p class="ctx-name">Gratin &lt;dauphinois&gt;</p>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<p class="muted">Vendredi 13 novembre, au dîner · 4 personnes</p>"#),
            "{html}"
        );
        assert!(
            html.contains("Le jour, le repas et le nombre de personnes sont conservés."),
            "{html}"
        );
        // Without a day, the day and the meal are still to choose.
        let s = selection("Tarte", None, None, Some(8));
        let html = render(Some(&s), Offer::Ready(&[]), None);
        assert!(
            html.contains(r#"<p class="muted">Sans jour pour l'instant · 8 personnes</p>"#),
            "{html}"
        );
        assert!(
            html.contains(
                "Le nombre de personnes est conservé ; le jour et le repas restent à choisir."
            ),
            "{html}"
        );
        // Without a number of people, none is said.
        let s = selection("Tarte", Some(friday()), None, None);
        let html = render(Some(&s), Offer::Ready(&[]), None);
        assert!(
            html.contains(r#"<p class="muted">Vendredi 13 novembre</p>"#),
            "{html}"
        );
    }

    #[test]
    fn each_offer_is_a_form_with_its_duration_and_what_it_lacks() {
        let s = selection("Gratin", Some(friday()), Some("dinner"), Some(4));
        let offers = [
            offer(2, "Soupe <potimarron>", Some(45), &["Crème"], 3),
            offer(3, "Lasagnes", Some(75), &["Pâtes", "Tomates"], 4),
            offer(4, "Omelette", None, &[], 2),
        ];
        let html = render(Some(&s), Offer::Ready(&offers), None);
        assert!(html.contains("<h2>Propositions</h2>"), "{html}");
        assert!(
            html.contains("Classées d'après votre stock, ce qui a été cuisiné récemment et la saison. La première est la mieux placée."),
            "{html}"
        );
        assert_eq!(
            html.matches(r#"<li class="card prop">"#).count(),
            3,
            "{html}"
        );
        assert_eq!(
            html.matches(&format!(
                r#"<form method="post" action="/recipes/{}/replace">"#,
                id(1)
            ))
            .count(),
            3,
            "{html}"
        );
        // One per offer, and the allergens' box's (#825).
        assert_eq!(
            html.matches(r#"<input type="hidden" name="back" value="week"/>"#)
                .count(),
            4,
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"<input type="hidden" name="replacement" value="{}"/>"#,
                id(2)
            )),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="name">Soupe &lt;potimarron&gt;</span><span class="meta"><span>45 min</span><span class="badge soon">Il manque 1 élément</span></span>"#),
            "{html}"
        );
        assert!(
            html.contains(
                "<span>1 h 15</span><span class=\"badge soon\">Il manque 2 éléments</span>"
            ),
            "{html}"
        );
        // In stock: the badge of the home page (arbitrage of the mock-up).
        assert!(
            html.contains(r#"<span class="name">Omelette</span><span class="meta"><span class="badge ok">Tout est en stock</span></span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<button type="submit">Choisir<span class="sr-only"> Soupe &lt;potimarron&gt;</span></button>"#),
            "{html}"
        );
        // The best first, as the API gives them.
        assert!(html.find("Soupe").unwrap() < html.find("Lasagnes").unwrap());
        assert!(html.find("Lasagnes").unwrap() < html.find("Omelette").unwrap());
        // « Annuler » goes back without a change.
        assert!(
            html.contains(r#"<div class="actions"><a class="btn secondary" href="/recipes/week">Annuler</a></div>"#),
            "{html}"
        );
    }

    #[test]
    fn a_single_offer_is_titled_in_the_singular() {
        let s = selection("Gratin", Some(friday()), Some("dinner"), Some(4));
        let offers = [offer(2, "Soupe", Some(45), &[], 3)];
        let html = render(Some(&s), Offer::Ready(&offers), None);
        assert!(html.contains("<h2>Proposition</h2>"), "{html}");
        assert_eq!(
            html.matches("<li class=\"card prop\">").count(),
            1,
            "{html}"
        );
    }

    #[test]
    fn a_recipe_without_an_ingredient_says_nothing_of_the_stock() {
        let s = selection("Gratin", Some(friday()), Some("dinner"), Some(4));
        let offers = [offer(2, "Salade", None, &[], 0)];
        let html = render(Some(&s), Offer::Ready(&offers), None);
        assert!(!html.contains("Tout est en stock"), "{html}");
        assert!(!html.contains("class=\"meta\""), "{html}");
    }

    #[test]
    fn no_offer_left_is_said_and_nothing_can_be_chosen() {
        let s = selection(
            "Gratin <dauphinois>",
            Some(friday()),
            Some("dinner"),
            Some(4),
        );
        let html = render(Some(&s), Offer::Ready(&[]), None);
        assert!(
            html.contains("Aucune autre recette à proposer pour l'instant."),
            "{html}"
        );
        assert!(
            html.contains("Rien n'a été modifié : « Gratin &lt;dauphinois&gt; » reste prévu le vendredi 13 novembre, au dîner."),
            "{html}"
        );
        assert!(
            html.contains(r#"<a class="btn" href="/recipes">Voir les recettes</a>"#),
            "{html}"
        );
        // Nothing to post; the allergens' box alone (#825).
        assert!(!html.contains(r#"<form method="post""#), "{html}");
        assert_eq!(html.matches("<form").count(), 1, "{html}");
        assert!(!html.contains("Choisir"), "{html}");
        assert!(html.contains(">Annuler</a>"), "{html}");
        // Without a day.
        let s = selection("Tarte", None, None, Some(8));
        let html = render(Some(&s), Offer::Ready(&[]), None);
        assert!(
            html.contains("Rien n'a été modifié : « Tarte » reste à réaliser, sans jour."),
            "{html}"
        );
    }

    #[test]
    fn offers_that_could_not_be_read_are_an_error_with_retry() {
        let s = selection("Gratin", Some(friday()), Some("dinner"), Some(4));
        let html = render(Some(&s), Offer::Unavailable, None);
        assert!(
            html.contains(r#"<p class="notice error" role="alert">Impossible de charger les propositions pour le moment. Rien n'a été modifié.</p>"#),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"<a class="btn" href="/recipes/{}/replace?back=week">Réessayer</a>"#,
                id(1)
            )),
            "{html}"
        );
        assert!(!html.contains("<form"), "{html}");
        assert!(!html.contains("Aucune autre recette"), "{html}");
        // Nor the selection: no context, the error and the way back.
        let html = render(None, Offer::Unavailable, None);
        assert!(!html.contains("Vous remplacez"), "{html}");
        assert!(html.contains("Réessayer"), "{html}");
        assert!(html.contains(">Annuler</a>"), "{html}");
    }

    #[test]
    fn a_refused_choice_is_said_above_the_offers_still_open() {
        let s = selection("Gratin", Some(friday()), Some("dinner"), Some(4));
        let offers = [offer(2, "Soupe", Some(45), &[], 3)];
        let html = render(
            Some(&s),
            Offer::Ready(&offers),
            Some("« Omelette » est déjà prévue."),
        );
        let alert = html
            .find(r#"<p class="notice error" role="alert">« Omelette » est déjà prévue.</p>"#)
            .unwrap();
        assert!(alert < html.find("<h2>").unwrap(), "{html}");
        assert!(html.contains("<form"), "{html}");
    }
}
