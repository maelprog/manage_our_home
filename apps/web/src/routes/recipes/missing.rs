//! What the stock lacks of a planned recipe (#560), from the `missing`,
//! `use_by` and `days_left` the selections carry since #559.
//!
//! - In « Prévues », a recipe whose `missing` is not empty carries a
//!   « Il manque n élément(s) » badge. With JavaScript it opens a
//!   `<dialog>` (`enhance.js`, `data-dialog`) listing each element with the
//!   quantity lacking; without, the same link leads to
//!   `GET /recipes/:id/missing`, a page with the same list. Both offer
//!   « Ajouter à la liste de courses » (`POST /recipes/:id/missing`), which
//!   comes back to « Prévues » with a banner. The recipe stays « Faite »-able:
//!   the list is a warning.
//! - A recipe with a `use_by` carries a priority badge, « À faire avant le
//!   jj/mm (sous x jours) », or « Produit périmé depuis le jj/mm » once
//!   past. The API sends those first; the order is kept.
//! - After a change of the stock (a stepper or a deletion on `/stocks`, a
//!   recipe marked done), a banner counts the planned recipes left
//!   incomplete and leads to « Prévues » (`incomplete_banner`).
//!
//! The words carry every state; the tint of a badge only repeats them
//! (WCAG 1.4.1).

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::NaiveDate;
use manage_our_home_shared::dto::recipes::{MissingIngredient, PlannedSelection};
use manage_our_home_shared::validation::recipes::servings_label;
use manage_our_home_shared::validation::stocks::format_quantity;
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{family_context, planned_with_stock, recipes_cookie, service_unavailable_page};

/// Where the badge, the page and the form come back to: « Prévues ».
const PLANNED_TAB: &str = "/recipes?vue=prevues";

/// The `id` of a recipe's dialog, which its badge names in `data-dialog`.
fn dialog_id(recipe_id: Uuid) -> String {
    format!("missing-{recipe_id}")
}

/// « Il manque n élément(s) », a link to the page listing them, which
/// `enhance.js` turns into the opening of the dialog. Nothing when nothing
/// is missing.
pub(crate) fn missing_badge(recipe_id: Uuid, count: usize) -> String {
    let what = match count {
        0 => return String::new(),
        1 => "1 élément".to_string(),
        n => format!("{n} éléments"),
    };
    format!(
        r#" <a class="badge soon" href="/recipes/{recipe_id}/missing" data-dialog="{dialog}" aria-haspopup="dialog">Il manque {what}</a>"#,
        dialog = dialog_id(recipe_id),
    )
}

/// The priority badge of a recipe that uses up what expires: « À faire
/// avant le jj/mm (sous x jours) », or « Produit périmé depuis le jj/mm »
/// once the date is past. Red from tomorrow, amber further out — the
/// stocks' expiry pastille (`stocks::list::expiry_pill`).
pub(crate) fn use_by_badge(use_by: NaiveDate, days_left: i64) -> String {
    let date = use_by.format("%d/%m");
    let (class, text) = match days_left {
        ..=-1 => ("badge late", format!("Produit périmé depuis le {date}")),
        0 => (
            "badge late",
            format!("À faire avant le {date} (aujourd'hui)"),
        ),
        1 => (
            "badge late",
            format!("À faire avant le {date} (sous 1 jour)"),
        ),
        n => (
            "badge soon",
            format!("À faire avant le {date} (sous {n} jours)"),
        ),
    };
    format!(r#" <span class="{class}">{text}</span>"#)
}

/// One line per element missing: its name, then the quantity lacking in
/// its unit when it has one.
pub(crate) fn missing_items_html(missing: &[MissingIngredient]) -> String {
    let items: String = missing
        .iter()
        .map(|m| {
            let qty = m
                .quantity
                .map(|q| format!(" — {}", format_quantity(q, m.unit.as_deref().unwrap_or(""))))
                .unwrap_or_default();
            format!("<li>{}{}</li>", html_escape(&m.name), html_escape(&qty))
        })
        .collect();
    format!("<ul>{items}</ul>")
}

/// « Ajouter à la liste de courses »: one item per element missing.
fn add_form(recipe_id: Uuid) -> String {
    format!(
        r#"<form method="post" action="/recipes/{recipe_id}/missing"><button type="submit">Ajouter à la liste de courses</button></form>"#
    )
}

/// The dialog a badge opens: the elements missing, « Ajouter à la liste de
/// courses » and « Fermer ». Nothing when nothing is missing.
pub(crate) fn missing_dialog(s: &PlannedSelection) -> String {
    if s.missing.is_empty() {
        return String::new();
    }
    let id = dialog_id(s.selection.recipe_id);
    format!(
        r#"<dialog id="{id}" aria-labelledby="{id}-title"><h2 id="{id}-title">Il manque à « {name} »</h2>{list}<div class="actions">{add}<form method="dialog"><button type="submit" class="secondary">Fermer</button></form></div></dialog>"#,
        name = html_escape(&s.selection.recipe_name),
        list = missing_items_html(&s.missing),
        add = add_form(s.selection.recipe_id),
    )
}

/// How many planned recipes lack something.
pub(crate) fn incomplete_count(planned: &[PlannedSelection]) -> usize {
    planned.iter().filter(|s| !s.missing.is_empty()).count()
}

/// « n recette(s) prévue(s) incomplète(s) », leading to « Prévues ».
/// Nothing at 0.
pub(crate) fn incomplete_banner(count: usize) -> String {
    let text = match count {
        0 => return String::new(),
        1 => "1 recette prévue incomplète.".to_string(),
        n => format!("{n} recettes prévues incomplètes."),
    };
    format!(
        r#"<p class="notice warning">{text} <a href="{PLANNED_TAB}">Voir les recettes prévues</a></p>"#
    )
}

/// Whether the page's `?notice=` follows a change of the stock: a stepper
/// or a deletion on `/stocks`, a recipe marked done. Those pages then tell
/// the planned recipes left incomplete.
pub(crate) fn after_stock_change(notice: Option<&str>) -> bool {
    matches!(
        notice,
        Some("quantity_adjusted" | "item_deleted" | "recipe_done")
    )
}

/// The banner of [`incomplete_banner`] for the family's planned recipes as
/// they are now. Nothing when they cannot be read: it is a suggestion, and
/// the page it sits on has its own banner already.
pub(crate) async fn incomplete_banner_for(
    state: &AppState,
    gid: Uuid,
    cookie: Option<&str>,
) -> String {
    match planned_with_stock(state, gid, cookie).await {
        Ok(planned) => incomplete_banner(incomplete_count(&planned)),
        Err(()) => String::new(),
    }
}

/// The page `GET /recipes/:id/missing` renders, the dialog's content
/// without JavaScript.
fn page_body(s: &PlannedSelection) -> String {
    let people = s
        .selection
        .servings
        .map(|n| format!(" pour {}", servings_label(n)))
        .unwrap_or_default();
    let content = if s.missing.is_empty() {
        r#"<p>Rien ne manque : le stock couvre cette recette.</p>"#.to_string()
    } else {
        format!(
            r#"{list}
<div class="actions">{add}</div>"#,
            list = missing_items_html(&s.missing),
            add = add_form(s.selection.recipe_id),
        )
    };
    format!(
        r#"<h1>Éléments manquants</h1>
<p>Ce que le stock n'a pas pour « {name} »{people}. La recette reste faisable : marquez-la faite quand vous voulez.</p>
{content}
<p><a href="{PLANNED_TAB}">Retour aux recettes prévues</a></p>"#,
        name = html_escape(&s.selection.recipe_name),
    )
}

/// `GET /recipes/:id/missing`: the elements a planned recipe lacks, for a
/// browser without JavaScript. A recipe no longer planned goes back to
/// « Prévues » with the banner of a selection gone.
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
) -> Response {
    let Some(fam) = family_context(
        &state,
        &headers,
        &me,
        &format!("/recipes/{recipe_id}/missing"),
    )
    .await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);
    let Ok(planned) = planned_with_stock(&state, fam.gid, cookie.as_deref()).await else {
        return service_unavailable_page().into_response();
    };
    let Some(selection) = planned.iter().find(|s| s.selection.recipe_id == recipe_id) else {
        return Redirect::to(&format!("{PLANNED_TAB}&error=selection_gone")).into_response();
    };
    Html(shell_with_header(
        Width::Form,
        "Éléments manquants",
        &fam.header,
        &page_body(selection),
    ))
    .into_response()
}

/// Where `POST /recipes/:id/missing` lands, from the API's answer: the
/// number of items it created on a 201, or the status otherwise.
fn added_target(result: Result<(reqwest::StatusCode, usize), ()>) -> String {
    let param = match result {
        Ok((reqwest::StatusCode::CREATED, 0)) => "notice=missing_none",
        Ok((reqwest::StatusCode::CREATED, _)) => "notice=missing_added",
        Ok((reqwest::StatusCode::NOT_FOUND, _)) => "error=selection_gone",
        Ok((reqwest::StatusCode::FORBIDDEN, _)) => "error=forbidden",
        Ok(_) | Err(()) => "error=unavailable",
    };
    format!("{PLANNED_TAB}&{param}")
}

/// `POST /recipes/:id/missing`: puts on the grocery list what the stock
/// lacks of the planned recipe (`POST …/selection/missing`, #559), then
/// back to « Prévues ».
pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, PLANNED_TAB).await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!(
            "/groups/{}/recipes/{}/selection/missing",
            fam.gid, recipe_id
        ),
        cookie.as_deref(),
        None,
    )
    .await
    .map(|resp| {
        let created = resp
            .body
            .get("items")
            .and_then(|items| items.as_array())
            .map_or(0, Vec::len);
        (resp.status, created)
    })
    .map_err(|_| ());
    Redirect::to(&added_target(result)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use manage_our_home_shared::dto::recipes::RecipeSelectionResponse;

    fn missing(name: &str, quantity: Option<f64>, unit: Option<&str>) -> MissingIngredient {
        MissingIngredient {
            name: name.into(),
            quantity,
            unit: unit.map(str::to_string),
        }
    }

    fn planned(
        recipe_id: u128,
        name: &str,
        missing: Vec<MissingIngredient>,
        use_by: Option<(NaiveDate, i64)>,
    ) -> PlannedSelection {
        PlannedSelection {
            selection: RecipeSelectionResponse {
                id: Uuid::from_u128(recipe_id + 1000),
                recipe_id: Uuid::from_u128(recipe_id),
                recipe_name: name.into(),
                status: "planned".into(),
                servings: Some(4),
                created_by: Uuid::from_u128(200),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            missing,
            use_by: use_by.map(|(d, _)| d),
            days_left: use_by.map(|(_, n)| n),
        }
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn nothing_missing_is_no_badge_and_no_dialog() {
        assert_eq!(missing_badge(Uuid::from_u128(1), 0), "");
        assert_eq!(missing_dialog(&planned(1, "Crêpes", Vec::new(), None)), "");
    }

    #[test]
    fn the_missing_badge_counts_singular_and_plural_and_names_its_dialog() {
        let id = Uuid::from_u128(1);
        assert_eq!(
            missing_badge(id, 1),
            format!(
                r#" <a class="badge soon" href="/recipes/{id}/missing" data-dialog="missing-{id}" aria-haspopup="dialog">Il manque 1 élément</a>"#
            )
        );
        assert!(missing_badge(id, 3).contains(">Il manque 3 éléments</a>"));
    }

    #[test]
    fn a_coming_date_says_by_when_and_in_how_many_days() {
        assert_eq!(
            use_by_badge(d(2026, 11, 2), 1),
            r#" <span class="badge late">À faire avant le 02/11 (sous 1 jour)</span>"#
        );
        assert_eq!(
            use_by_badge(d(2026, 11, 4), 3),
            r#" <span class="badge soon">À faire avant le 04/11 (sous 3 jours)</span>"#
        );
        // The day itself: still to do, today.
        assert_eq!(
            use_by_badge(d(2026, 11, 1), 0),
            r#" <span class="badge late">À faire avant le 01/11 (aujourd'hui)</span>"#
        );
    }

    #[test]
    fn a_past_date_says_since_when_the_product_is_expired() {
        assert_eq!(
            use_by_badge(d(2026, 10, 30), -2),
            r#" <span class="badge late">Produit périmé depuis le 30/10</span>"#
        );
        assert_eq!(
            use_by_badge(d(2026, 10, 31), -1),
            r#" <span class="badge late">Produit périmé depuis le 31/10</span>"#
        );
    }

    #[test]
    fn each_missing_element_reads_its_name_and_the_quantity_lacking() {
        let html = missing_items_html(&[
            missing("Lait", Some(300.0), Some("ml")),
            missing("Œufs", Some(2.0), Some("unité")),
            missing("Sel <fin>", None, None),
            missing("Farine", Some(0.1 + 0.2), Some("kg")),
        ]);
        assert_eq!(
            html,
            "<ul><li>Lait — 300 ml</li><li>Œufs — 2</li><li>Sel &lt;fin&gt;</li><li>Farine — 0,3 kg</li></ul>"
        );
    }

    #[test]
    fn the_dialog_lists_what_is_missing_and_adds_it_to_the_grocery_list() {
        let s = planned(
            1,
            "Crêpes <maison>",
            vec![missing("Lait", Some(300.0), Some("ml"))],
            None,
        );
        let id = Uuid::from_u128(1);
        let html = missing_dialog(&s);
        assert!(
            html.starts_with(&format!(
                r#"<dialog id="missing-{id}" aria-labelledby="missing-{id}-title">"#
            )),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"<h2 id="missing-{id}-title">Il manque à « Crêpes &lt;maison&gt; »</h2>"#
            )),
            "{html}"
        );
        assert!(html.contains("<li>Lait — 300 ml</li>"), "{html}");
        assert!(
            html.contains(&format!(
                r#"<form method="post" action="/recipes/{id}/missing"><button type="submit">Ajouter à la liste de courses</button></form>"#
            )),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<form method="dialog"><button type="submit" class="secondary">Fermer</button></form>"#
            ),
            "{html}"
        );
        assert!(html.ends_with("</dialog>"), "{html}");
    }

    #[test]
    fn the_incomplete_recipes_are_counted_and_worded() {
        let lacking = || vec![missing("Lait", Some(1.0), Some("L"))];
        let list = [
            planned(1, "A", lacking(), None),
            planned(2, "B", Vec::new(), Some((d(2026, 11, 2), 1))),
            planned(3, "C", lacking(), None),
        ];
        assert_eq!(incomplete_count(&list), 2);
        assert_eq!(incomplete_count(&[]), 0);
        assert_eq!(incomplete_banner(0), "");
        assert_eq!(
            incomplete_banner(1),
            r#"<p class="notice warning">1 recette prévue incomplète. <a href="/recipes?vue=prevues">Voir les recettes prévues</a></p>"#
        );
        assert!(incomplete_banner(2).contains(">2 recettes prévues incomplètes. <a"));
    }

    #[test]
    fn only_a_change_of_the_stock_tells_the_incomplete_recipes() {
        assert!(after_stock_change(Some("quantity_adjusted")));
        assert!(after_stock_change(Some("item_deleted")));
        assert!(after_stock_change(Some("recipe_done")));
        assert!(!after_stock_change(Some("item_created")));
        assert!(!after_stock_change(Some("item_updated")));
        assert!(!after_stock_change(Some("recipe_abandoned")));
        assert!(!after_stock_change(Some("missing_added")));
        assert!(!after_stock_change(None));
    }

    #[test]
    fn the_page_without_javascript_lists_the_same_elements() {
        let s = planned(
            1,
            "Crêpes",
            vec![missing("Lait", Some(300.0), Some("ml"))],
            None,
        );
        let html = page_body(&s);
        assert!(html.contains("<h1>Éléments manquants</h1>"), "{html}");
        assert!(html.contains("« Crêpes » pour 4 personnes"), "{html}");
        assert!(html.contains("<li>Lait — 300 ml</li>"), "{html}");
        assert!(
            html.contains(">Ajouter à la liste de courses</button>"),
            "{html}"
        );
        assert!(html.contains(r#"href="/recipes?vue=prevues""#), "{html}");
        let complete = page_body(&planned(1, "Crêpes", Vec::new(), None));
        assert!(complete.contains("Rien ne manque"), "{complete}");
        assert!(!complete.contains("<form"), "{complete}");
    }

    #[test]
    fn adding_lands_on_the_planned_tab_with_what_happened() {
        use reqwest::StatusCode;
        assert_eq!(
            added_target(Ok((StatusCode::CREATED, 2))),
            "/recipes?vue=prevues&notice=missing_added"
        );
        assert_eq!(
            added_target(Ok((StatusCode::CREATED, 0))),
            "/recipes?vue=prevues&notice=missing_none"
        );
        assert_eq!(
            added_target(Ok((StatusCode::NOT_FOUND, 0))),
            "/recipes?vue=prevues&error=selection_gone"
        );
        assert_eq!(
            added_target(Ok((StatusCode::FORBIDDEN, 0))),
            "/recipes?vue=prevues&error=forbidden"
        );
        assert_eq!(
            added_target(Ok((StatusCode::INTERNAL_SERVER_ERROR, 0))),
            "/recipes?vue=prevues&error=unavailable"
        );
        assert_eq!(
            added_target(Err(())),
            "/recipes?vue=prevues&error=unavailable"
        );
    }
}
