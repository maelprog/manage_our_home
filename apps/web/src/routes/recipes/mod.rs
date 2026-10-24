//! Recipes screens (front epic F5, issue #20): the family recipe list with a
//! ranked suggestion view, manual create, per-recipe detail with a
//! log-a-meal action, and edit/delete behind a permission bar. Same SSR
//! pattern as `routes/stocks/*` and `routes/agenda/*` — plain
//! `<form method=post>` submissions, per-page error tables mapping
//! `apps/api/src/recipes/`'s exact status/error codes to French copy, PRG
//! (`?notice=`/`?error=` codes) after every mutation. Full spec (route
//! table, error tables, acceptance criteria, the score-vs-order and
//! grocery-list-hook decisions) in `docs/front-epic-5-recipes.md`.
//!
//! Permission bar (mirrors `apps/api/src/recipes/mod.rs::can_modify`): any
//! family member may create/read a recipe or log a meal, so those controls
//! render for everyone; the edit link and delete button render only for the
//! recipe's creator or a group admin/owner. The backend stays the authority
//! (a forged edit/delete/log is still 403'd, mapped defensively here).

pub mod detail;
pub mod edit;
pub mod import;
pub mod list;
pub mod new;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::GroupSummary;
use manage_our_home_shared::dto::recipes::{
    RecipeSelectionList, RecipeSelectionResponse, SelectRecipeRequest,
};
use manage_our_home_shared::validation::recipes::{parse_servings, servings_label, SERVINGS_MAX};
use uuid::Uuid;

use crate::app::{html_escape, shell, Width};
use crate::family::{active_group_id_from_headers, resolve_active_group};
use crate::layout::CurrentUser;
use crate::routes::groups::{cookie_of, header_with_groups};
use crate::state::{api_request_auth, AppState};

/// Resolved active-family context for a Recipes page: the family id every
/// `/groups/:gid/…` API call is scoped to, the caller's role in it (for the
/// permission bar), and the shared authenticated header. `None` means the
/// user has no group yet — callers redirect to `/groups/new`.
pub(crate) struct FamilyContext {
    pub gid: Uuid,
    pub role: String,
    pub header: String,
}

pub(crate) async fn family_context(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    redirect_to: &str,
) -> Option<FamilyContext> {
    let (groups, header) = header_with_groups(state, headers, me, redirect_to).await;
    let preferred = active_group_id_from_headers(headers);
    let active: Option<&GroupSummary> = resolve_active_group(&groups, preferred);
    active.map(|g| FamilyContext {
        gid: g.group_id,
        role: g.role.clone(),
        header,
    })
}

/// Mirror of `apps/api/src/recipes/mod.rs::can_modify`: the recipe's
/// creator, or a group owner/admin, may edit or delete it. Used to decide
/// whether to render the edit/delete controls (the backend still 403s a
/// forged request). Create/read/log-a-meal are *not* gated by this — they're
/// open to any member.
pub(crate) fn can_modify(role: &str, is_creator: bool) -> bool {
    is_creator || role == "owner" || role == "admin"
}

/// Fetches `cookie_of` for a Recipes handler (re-exported so the submodules
/// don't each reach into `routes::groups`).
pub(crate) fn recipes_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_of(headers)
}

// -- recipes to cook (#470) --------------------------------------------------

/// The family's `planned` selections (#469), which the grocery list is
/// generated from. `Err` only on a transport failure; any other status
/// reads as no selection, as the other lists of these pages do.
pub(crate) async fn planned_selections(
    state: &AppState,
    gid: Uuid,
    cookie: Option<&str>,
) -> Result<Vec<RecipeSelectionResponse>, ()> {
    match api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/recipes/selections"),
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            Ok(serde_json::from_value::<RecipeSelectionList>(resp.body)
                .map(|l| l.selections)
                .unwrap_or_default())
        }
        Ok(_) => Ok(Vec::new()),
        Err(_) => Err(()),
    }
}

/// Where the « À réaliser » and « Retirer » forms send the member back:
/// the list when they came from it, the recipe otherwise.
pub(crate) fn back_path(back: &str, recipe_id: Uuid) -> String {
    if back == "list" {
        "/recipes".to_string()
    } else {
        format!("/recipes/{recipe_id}")
    }
}

/// The « À réaliser » form (#470): plans the recipe for the next shopping,
/// for the number of people typed — the recipe's own base by default, and
/// blank when it has none (the quantities are then taken as written).
/// `back` is `list` or `detail`, the page the redirect returns to.
pub(crate) fn select_form(recipe_id: Uuid, base: Option<i32>, back: &str, class: &str) -> String {
    let value = base.map(|s| s.to_string()).unwrap_or_default();
    format!(
        r#"<form method="post" action="/recipes/{recipe_id}/select" class="{class}">
<input type="hidden" name="back" value="{back}"/>
<label>Personnes <input type="number" name="servings" min="1" max="{SERVINGS_MAX}" step="1" value="{value}" placeholder="Optionnel"/></label>
<button type="submit">À réaliser</button>
</form>"#,
        class = html_escape(class),
        back = html_escape(back),
    )
}

/// The « Retirer » form (#470): takes the recipe out of the next shopping.
pub(crate) fn deselect_form(recipe_id: Uuid, back: &str) -> String {
    format!(
        r#"<form method="post" action="/recipes/{recipe_id}/deselect">
<input type="hidden" name="back" value="{back}"/>
<button type="submit" class="secondary">Retirer</button>
</form>"#,
        back = html_escape(back),
    )
}

/// What a planned recipe shows of its selection: "À réaliser", and for how
/// many people when the selection says.
pub(crate) fn planned_badge(servings: Option<i32>) -> String {
    let people = servings
        .map(|s| format!(" · {}", servings_label(s)))
        .unwrap_or_default();
    format!(r#"<span class="badge">À réaliser{people}</span>"#)
}

#[derive(serde::Deserialize)]
pub struct SelectForm {
    #[serde(default)]
    servings: String,
    #[serde(default)]
    back: String,
}

/// `POST /recipes/:id/select`: plans the recipe (`POST …/selection`), or
/// changes the number of people of the one already planned — the API is
/// idempotent on it.
pub async fn select(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
    Form(form): Form<SelectForm>,
) -> Response {
    let back = back_path(&form.back, recipe_id);
    let Some(fam) = family_context(&state, &headers, &me, &back).await else {
        return Redirect::to("/groups/new").into_response();
    };
    let servings = match parse_servings(&form.servings) {
        Ok(s) => s,
        Err(code) => return Redirect::to(&format!("{back}?error={code}")).into_response(),
    };
    let cookie = recipes_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/recipes/{}/selection", fam.gid, recipe_id),
        cookie.as_deref(),
        Some(serde_json::to_value(SelectRecipeRequest { servings }).unwrap()),
    )
    .await;
    let target = match result {
        Ok(resp)
            if resp.status == reqwest::StatusCode::CREATED
                || resp.status == reqwest::StatusCode::OK =>
        {
            format!("{back}?notice=recipe_selected")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return recipe_not_found_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            format!("{back}?error=invalid_servings")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{back}?error=forbidden")
        }
        Ok(_) | Err(_) => format!("{back}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}

#[derive(serde::Deserialize)]
pub struct DeselectForm {
    #[serde(default)]
    back: String,
}

/// `POST /recipes/:id/deselect`: takes the recipe out of the next shopping
/// (`DELETE …/selection`). A selection already gone (404, another member
/// was quicker) reads as done: the recipe is no longer planned either way.
pub async fn deselect(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
    Form(form): Form<DeselectForm>,
) -> Response {
    let back = back_path(&form.back, recipe_id);
    let Some(fam) = family_context(&state, &headers, &me, &back).await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &format!("/groups/{}/recipes/{}/selection", fam.gid, recipe_id),
        cookie.as_deref(),
        None,
    )
    .await;
    let target = match result {
        Ok(resp)
            if resp.status == reqwest::StatusCode::NO_CONTENT
                || resp.status == reqwest::StatusCode::NOT_FOUND =>
        {
            format!("{back}?notice=recipe_deselected")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{back}?error=forbidden")
        }
        Ok(_) | Err(_) => format!("{back}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}

// -- shared error/landing pages ---------------------------------------------

pub(crate) fn recipe_not_found_page() -> Html<String> {
    let body = view! {
        <h1>"Recette introuvable"</h1>
        <p>"Cette recette n'existe pas ou vous n'y avez pas accès."</p>
        <a class="btn secondary" href="/recipes">"Retour aux recettes"</a>
    };
    Html(shell(Width::Form, "Recette introuvable", &body.to_html()))
}

pub(crate) fn service_unavailable_page() -> Html<String> {
    let body = view! {
        <h1>"Service momentanément indisponible"</h1>
        <p>"Merci de réessayer dans quelques instants."</p>
        <a class="btn secondary" href="/recipes">"Retour aux recettes"</a>
    };
    Html(shell(Width::Form, "Service indisponible", &body.to_html()))
}

pub(crate) fn forbidden_page() -> Html<String> {
    let body = view! {
        <h1>"Action non autorisée"</h1>
        <p>"Vous n'avez pas les droits nécessaires sur cette recette."</p>
        <a class="btn secondary" href="/recipes">"Retour aux recettes"</a>
    };
    Html(shell(Width::Form, "Action non autorisée", &body.to_html()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creator_can_modify_regardless_of_role() {
        assert!(can_modify("standard", true));
    }

    #[test]
    fn owner_and_admin_can_modify_others_recipes() {
        assert!(can_modify("owner", false));
        assert!(can_modify("admin", false));
    }

    #[test]
    fn standard_member_cannot_modify_another_members_recipe() {
        assert!(!can_modify("standard", false));
    }

    #[test]
    fn the_selection_forms_return_to_the_list_or_the_recipe_only() {
        let id = Uuid::from_u128(7);
        assert_eq!(back_path("list", id), "/recipes");
        assert_eq!(back_path("detail", id), format!("/recipes/{id}"));
        // Anything else is the recipe: never a path the form chose.
        assert_eq!(back_path("https://evil.test", id), format!("/recipes/{id}"));
        assert_eq!(back_path("", id), format!("/recipes/{id}"));
    }

    #[test]
    fn the_select_form_proposes_the_recipe_base() {
        let id = Uuid::from_u128(7);
        let html = select_form(id, Some(4), "list", "actions");
        assert!(
            html.contains(&format!(r#"action="/recipes/{id}/select" class="actions""#)),
            "{html}"
        );
        assert!(html.contains(r#"name="back" value="list""#), "{html}");
        assert!(
            html.contains(r#"name="servings" min="1" max="50" step="1" value="4""#),
            "{html}"
        );
        assert!(html.contains(">À réaliser</button>"), "{html}");
        // No base: the field is left blank, the quantities as written.
        let html = select_form(id, None, "detail", "card inline");
        assert!(html.contains(r#"step="1" value="""#), "{html}");
    }

    #[test]
    fn the_deselect_form_posts_retirer() {
        let id = Uuid::from_u128(7);
        let html = deselect_form(id, "detail");
        assert!(html.contains(&format!(r#"action="/recipes/{id}/deselect""#)));
        assert!(html.contains(r#"name="back" value="detail""#));
        assert!(html.contains(r#"class="secondary">Retirer</button>"#));
    }

    #[test]
    fn the_planned_badge_says_for_how_many_people() {
        assert_eq!(
            planned_badge(Some(4)),
            r#"<span class="badge">À réaliser · 4 personnes</span>"#
        );
        assert_eq!(
            planned_badge(None),
            r#"<span class="badge">À réaliser</span>"#
        );
    }
}
