//! Recipes screens (front epic F5, issue #20): the family recipe list with a
//! ranked suggestion view, manual create, per-recipe detail, and
//! edit/delete behind a permission bar. Same SSR
//! pattern as `routes/stocks/*` and `routes/agenda/*` — plain
//! `<form method=post>` submissions, per-page error tables mapping
//! `apps/api/src/recipes/`'s exact status/error codes to French copy, PRG
//! (`?notice=`/`?error=` codes) after every mutation. Full spec (route
//! table, error tables, acceptance criteria, the score-vs-order and
//! grocery-list-hook decisions) in `docs/front-epic-5-recipes.md`.
//!
//! Permission bar (mirrors `apps/api/src/recipes/mod.rs::can_modify`): any
//! family member may create/read a recipe or mark a planned one done, so
//! those controls render for everyone; the edit link and delete button
//! render only for the recipe's creator or a member whose role meets the
//! family's `recipes_edit` threshold (#866, admin/owner by default). The
//! backend stays the authority (a forged edit/delete is still 403'd, mapped
//! defensively here).

pub mod allergy;
pub mod cook;
pub mod detail;
pub mod edit;
pub mod equipment;
pub mod import;
pub mod library;
pub mod list;
pub mod mine;
pub mod missing;
pub mod new;
pub mod outcome;
pub mod publish;
pub mod replace;
pub mod report;
pub mod start;
pub mod week;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use chrono::NaiveDate;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::GroupSummary;
use manage_our_home_shared::dto::recipes::{
    PlannedSelection, PlannedSelectionList, RecipeSelectionResponse, SelectRecipeRequest,
    UpdateSelectionRequest,
};
use manage_our_home_shared::validation::groups::{role_meets, Feature};
use manage_our_home_shared::validation::recipes::{parse_servings, SERVINGS_MAX};
use uuid::Uuid;

use crate::app::{html_escape, shell, Width};
use crate::family::{active_group_id_from_headers, resolve_active_group};
use crate::layout::CurrentUser;
use crate::routes::agenda::calendar::short_weekday;
use crate::routes::groups::{cookie_of, header_with_groups};
use crate::state::{api_request_auth, AppState};

/// Resolved active-family context for a Recipes page: the family id every
/// `/groups/:gid/…` API call is scoped to, the caller's role in it (for the
/// permission bar), and the shared authenticated header. `None` means the
/// user has no group yet — callers redirect to `/groups/new`, but for the
/// library's pages, which read [`reader_context`] (#990).
pub(crate) struct FamilyContext {
    pub gid: Uuid,
    pub name: String,
    pub role: String,
    /// The family's `recipes_edit` threshold (#866): from which role a member
    /// edits or deletes what another member wrote (`can_modify`).
    pub edit_min_role: String,
    pub header: String,
}

pub(crate) async fn family_context(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    redirect_to: &str,
) -> Option<FamilyContext> {
    reader_context(state, headers, me, redirect_to).await.0
}

/// [`family_context`] for the pages a reader opens without a family (the
/// library, #990): the family when there is one, and the header either
/// way — without one, that of no family, as on `/groups/new`.
pub(crate) async fn reader_context(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    redirect_to: &str,
) -> (Option<FamilyContext>, String) {
    let (groups, header) = header_with_groups(state, headers, me, redirect_to).await;
    let preferred = active_group_id_from_headers(headers);
    let active: Option<&GroupSummary> = resolve_active_group(&groups, preferred);
    let fam = active.map(|g| FamilyContext {
        gid: g.group_id,
        name: g.name.clone(),
        role: g.role.clone(),
        edit_min_role: g.min_role(Feature::RecipesEdit).to_string(),
        header: header.clone(),
    });
    (fam, header)
}

/// Mirror of `apps/api/src/recipes/mod.rs::can_modify`: the recipe's
/// creator, or a member whose role meets `min_role`, the family's
/// `recipes_edit` threshold (#866, `admin` by default), may edit or delete
/// it. Used to decide
/// whether to render the edit/delete controls (the backend still 403s a
/// forged request). Create/read/« Faite » are *not* gated by this — they're
/// open to any member.
pub(crate) fn can_modify(role: &str, min_role: &str, is_creator: bool) -> bool {
    is_creator || role_meets(role, min_role)
}

/// Fetches `cookie_of` for a Recipes handler (re-exported so the submodules
/// don't each reach into `routes::groups`).
pub(crate) fn recipes_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_of(headers)
}

// -- recipes to cook (#470) --------------------------------------------------

/// The family's `planned` selections (#469), which the grocery list is
/// generated from. `Err` on a transport failure and on any status but
/// `200`: the selections are then unknown, not empty.
pub(crate) async fn planned_selections(
    state: &AppState,
    gid: Uuid,
    cookie: Option<&str>,
) -> Result<Vec<RecipeSelectionResponse>, ()> {
    planned_with_stock(state, gid, cookie)
        .await
        .map(|list| list.into_iter().map(|s| s.selection).collect())
}

/// [`planned_selections`] with what the stock holds of each (#559): the
/// elements missing, and the date by which to cook it, the dated ones
/// first (#560).
pub(crate) async fn planned_with_stock(
    state: &AppState,
    gid: Uuid,
    cookie: Option<&str>,
) -> Result<Vec<PlannedSelection>, ()> {
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
            Ok(serde_json::from_value::<PlannedSelectionList>(resp.body)
                .map(|l| l.selections)
                .unwrap_or_default())
        }
        // Any other status is not « no selection »: read as unknown, like a
        // transport failure, rather than offering the stocks alone.
        Ok(_) | Err(_) => Err(()),
    }
}

/// Where the « À réaliser » and « Retirer » forms send the member back:
/// the list when they came from it, its « Prévues » tab (`planned`, #539),
/// the planning on its window (`week`, `week@AAAA-MM-JJ`, #813), the recipe
/// otherwise.
pub(crate) fn back_path(back: &str, recipe_id: Uuid) -> String {
    if let Some(start) = week::back_window(back) {
        return week::week_href(start);
    }
    match back {
        "list" => "/recipes".to_string(),
        "planned" => "/recipes?vue=prevues".to_string(),
        _ => format!("/recipes/{recipe_id}"),
    }
}

/// [`back_path`] with a `notice=` or `error=` parameter added, after the
/// tab's own when it has one.
pub(crate) fn back_with(back: &str, recipe_id: Uuid, param: &str) -> String {
    let path = back_path(back, recipe_id);
    let sep = if path.contains('?') { '&' } else { '?' };
    format!("{path}{sep}{param}")
}

/// The « À réaliser » form (#470): plans the recipe for the next shopping,
/// for the number of people typed — the recipe's own base by default, and
/// blank when it has none (the quantities are then taken as written).
/// `back` is `list` or `detail`, the page the redirect returns to.
pub(crate) fn select_form(recipe_id: Uuid, base: Option<i32>, back: &str, class: &str) -> String {
    select_form_with(recipe_id, base, back, class, "")
}

/// [`select_form`] with `hidden`, more hidden fields, after `back`.
fn select_form_with(
    recipe_id: Uuid,
    base: Option<i32>,
    back: &str,
    class: &str,
    hidden: &str,
) -> String {
    let value = base.map(|s| s.to_string()).unwrap_or_default();
    format!(
        r#"<form method="post" action="/recipes/{recipe_id}/select" class="{class}">
<input type="hidden" name="back" value="{back}"/>{hidden}
<label>Personnes <input type="number" name="servings" min="1" max="{SERVINGS_MAX}" step="1" value="{value}" placeholder="Optionnel"/></label>
<button type="submit">À réaliser</button>
</form>"#,
        class = html_escape(class),
        back = html_escape(back),
    )
}

/// The « À réaliser » form of a recipe chosen for a slot of the planning
/// (#813): the same form, which also sends the slot's day and meal, so that
/// the recipe is planned on them in one go.
pub(crate) fn select_form_on(
    recipe_id: Uuid,
    base: Option<i32>,
    back: &str,
    class: &str,
    slot: (NaiveDate, &str),
) -> String {
    let hidden = format!(
        r#"
<input type="hidden" name="planned_on" value="{day}"/>
<input type="hidden" name="meal_slot" value="{meal}"/>"#,
        day = slot.0.format("%Y-%m-%d"),
        meal = html_escape(slot.1),
    );
    select_form_with(recipe_id, base, back, class, &hidden)
}

/// The `back` of a plan made from the planning (#813): the window that
/// shows the day chosen, so that the recipe is seen where it went; without
/// a day, or from another page, `back` itself.
pub(crate) fn plan_back(back: &str, request: &UpdateSelectionRequest, today: NaiveDate) -> String {
    match (week::back_window(back), request.planned_on) {
        (Some(_), Some(Some(day))) => week::week_back(week::window_for(day, today)),
        _ => back.to_string(),
    }
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

/// The « Faite » and « Abandonner » links of a planned recipe (#474): each
/// opens its confirmation page (`outcome.rs`), which comes back to `back`.
pub(crate) fn outcome_links(recipe_id: Uuid, back: &str) -> String {
    let back = match back {
        "list" | "planned" => back,
        _ => "detail",
    };
    format!(
        r#"<a class="btn secondary" href="/recipes/{recipe_id}/done?back={back}">Faite</a>
<a class="btn secondary" href="/recipes/{recipe_id}/abandon?back={back}">Abandonner</a>"#
    )
}

/// What a planned recipe shows of its selection: "À réaliser". Its number
/// of people is said by the field beside it ([`servings_form`], #846).
pub(crate) fn planned_badge() -> String {
    r#"<span class="badge">À réaliser</span>"#.to_string()
}

/// The number of people of a planned recipe (#846), at the end of its
/// actions: sent as soon as it changes (`data-submit-on-change`), by
/// « Appliquer » without JavaScript. `change=servings` has `select` change
/// the selection only — never plan the recipe again, nor touch its day or
/// meal. Blank, with « Optionnel », when the selection says no number;
/// emptied, it removes the number. `name` is the recipe's: a list holds
/// one such field per row, the visible « Personnes » opening each name
/// (WCAG 2.5.3).
pub(crate) fn servings_form(
    recipe_id: Uuid,
    servings: Option<i32>,
    back: &str,
    name: &str,
) -> String {
    let (value, placeholder) = match servings {
        Some(n) => (n.to_string(), ""),
        None => (String::new(), r#" placeholder="Optionnel""#),
    };
    format!(
        r#"<form method="post" action="/recipes/{recipe_id}/select" class="servings-form" data-keep-scroll>
<input type="hidden" name="back" value="{back}"/>
<input type="hidden" name="change" value="servings"/>
<label>Personnes <input type="number" name="servings" min="1" max="{SERVINGS_MAX}" step="1" value="{value}"{placeholder} aria-label="Personnes, {name}" data-submit-on-change/></label>
<button type="submit" class="secondary" data-hidden-with-js>Appliquer</button>
</form>"#,
        back = html_escape(back),
        name = html_escape(name),
    )
}

/// The `PATCH …/selection` of [`servings_form`] (#846): the number typed,
/// or `null` for a field left empty, which removes it; the day and the
/// meal are not sent, so they stay. Out of 1–50, or not a whole number,
/// `invalid_servings`.
pub(crate) fn servings_request(input: &str) -> Result<UpdateSelectionRequest, &'static str> {
    Ok(UpdateSelectionRequest {
        servings: Some(parse_servings(input)?),
        ..UpdateSelectionRequest::default()
    })
}

/// The `notice=` of a number of people changed (#846), with the number.
pub(crate) fn servings_notice_param(servings: Option<i32>) -> String {
    match servings {
        Some(n) => format!("servings_updated&servings={n}"),
        None => "servings_cleared".to_string(),
    }
}

/// The banner of [`servings_notice_param`]'s codes; `servings` is the
/// URL's, repeated only when it is a number the form could have sent.
pub(crate) fn servings_notice(code: &str, servings: Option<&str>) -> Option<String> {
    match code {
        "servings_updated" => Some(
            match servings.and_then(|s| parse_servings(s).ok().flatten()) {
                Some(n) => format!("Nombre de personnes mis à jour : {n}."),
                None => "Nombre de personnes mis à jour.".to_string(),
            },
        ),
        "servings_cleared" => Some(
            "Nombre de personnes retiré : les quantités de la recette sont prises telles qu'écrites."
                .to_string(),
        ),
        _ => None,
    }
}

/// [`servings_notice`] as its page shows it: a success banner announced
/// (`role="status"`), the field that sent it applying on change, as the
/// settings of #844 do.
pub(crate) fn servings_banner(code: &str, servings: Option<&str>) -> Option<String> {
    servings_notice(code, servings).map(|n| {
        format!(
            r#"<p class="notice success" role="status">{}</p>"#,
            html_escape(&n)
        )
    })
}

// -- the day and the meal (#812) --------------------------------------------

/// A meal (#811) as a planned recipe says it: `lunch` midi, `dinner` soir.
pub(crate) fn meal_word(slot: &str) -> Option<&'static str> {
    match slot {
        "lunch" => Some("midi"),
        "dinner" => Some("soir"),
        _ => None,
    }
}

/// The `PATCH …/selection` body of the « Quand ? » form (#812): the day
/// (`AAAA-MM-JJ`, empty to remove it) and the meal (`lunch`, `dinner`, empty
/// to remove it), both always sent. A meal without a day is
/// `meal_without_day`, which the API would refuse too; a day that is not a
/// date `invalid_planned_on`, a meal of neither kind `invalid_meal_slot`.
pub(crate) fn plan_request(
    planned_on: &str,
    meal_slot: &str,
) -> Result<UpdateSelectionRequest, &'static str> {
    let day = match planned_on.trim() {
        "" => None,
        text => {
            Some(NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_| "invalid_planned_on")?)
        }
    };
    let meal = match meal_slot.trim() {
        "" => None,
        slot if meal_word(slot).is_some() => Some(slot.to_string()),
        _ => return Err("invalid_meal_slot"),
    };
    if meal.is_some() && day.is_none() {
        return Err("meal_without_day");
    }
    Ok(UpdateSelectionRequest {
        servings: None,
        planned_on: Some(day),
        meal_slot: Some(meal),
    })
}

/// The pastille of a dated recipe in the list (#812): "jeu. 19/11 · midi",
/// "sam. 21/11" without a meal; nothing for a recipe without a day.
pub(crate) fn plan_badge(planned_on: Option<NaiveDate>, meal_slot: Option<&str>) -> String {
    let Some(day) = planned_on else {
        return String::new();
    };
    let meal = meal_slot
        .and_then(meal_word)
        .map(|m| format!(" · {m}"))
        .unwrap_or_default();
    format!(
        r#"<span class="badge accent">{weekday} {date}{meal}</span>"#,
        weekday = short_weekday(day),
        date = day.format("%d/%m"),
    )
}

#[derive(serde::Deserialize)]
pub struct PlanForm {
    #[serde(default)]
    planned_on: String,
    #[serde(default)]
    meal_slot: String,
    #[serde(default)]
    back: String,
}

/// `POST /recipes/:id/planning` (#812): sets, changes or removes the day
/// and the meal of the recipe's planned selection (`PATCH …/selection`).
/// Any member may, as for its number of people.
pub async fn plan(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
    Form(form): Form<PlanForm>,
) -> Response {
    let back = form.back.as_str();
    let request = match plan_request(&form.planned_on, &form.meal_slot) {
        Ok(request) => request,
        Err(code) => {
            return Redirect::to(&back_with(back, recipe_id, &format!("error={code}")))
                .into_response()
        }
    };
    let back = plan_back(back, &request, crate::routes::agenda::today_paris());
    patch_plan(me, state, headers, recipe_id, &back, request, "plan_saved").await
}

#[derive(serde::Deserialize)]
pub struct ClearPlanForm {
    #[serde(default)]
    back: String,
}

/// `POST /recipes/:id/planning/clear` (#812): « Retirer la date », the
/// day and the meal together; the recipe stays planned.
pub async fn clear_plan(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
    Form(form): Form<ClearPlanForm>,
) -> Response {
    let request = UpdateSelectionRequest {
        servings: None,
        planned_on: Some(None),
        meal_slot: Some(None),
    };
    patch_plan(
        me,
        state,
        headers,
        recipe_id,
        &form.back,
        request,
        "plan_cleared",
    )
    .await
}

/// The `PATCH …/selection` of [`plan`], [`clear_plan`] and a number of
/// people changed ([`select`], #846), then back to the page with `notice`
/// — a code, and the parameters that follow it — or the API's refusal. A
/// selection gone in between (404, another member was quicker) is
/// `selection_gone`.
async fn patch_plan(
    me: MeResponse,
    state: AppState,
    headers: HeaderMap,
    recipe_id: Uuid,
    back: &str,
    request: UpdateSelectionRequest,
    notice: &str,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, &back_path(back, recipe_id)).await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::PATCH,
        &format!("/groups/{}/recipes/{}/selection", fam.gid, recipe_id),
        cookie.as_deref(),
        Some(serde_json::to_value(&request).unwrap()),
    )
    .await;
    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            back_with(back, recipe_id, &format!("notice={notice}"))
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            let code = match resp.body.get("error").and_then(|e| e.as_str()) {
                Some("invalid_meal_slot") => "invalid_meal_slot",
                Some("invalid_servings") => "invalid_servings",
                _ => "invalid_planned_on",
            };
            back_with(back, recipe_id, &format!("error={code}"))
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            back_with(back, recipe_id, "error=selection_gone")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            back_with(back, recipe_id, "error=forbidden")
        }
        Ok(_) | Err(_) => back_with(back, recipe_id, "error=unavailable"),
    };
    Redirect::to(&target).into_response()
}

#[derive(serde::Deserialize)]
pub struct SelectForm {
    #[serde(default)]
    servings: String,
    #[serde(default)]
    back: String,
    /// The slot of the planning the recipe is chosen for (#813), when the
    /// list was opened from one; both empty otherwise.
    #[serde(default)]
    planned_on: String,
    #[serde(default)]
    meal_slot: String,
    /// `servings` from the field of a planned recipe (#846).
    #[serde(default)]
    change: String,
}

/// `POST /recipes/:id/select`: plans the recipe (`POST …/selection`), or
/// changes the number of people of the one already planned — the API is
/// idempotent on it. Chosen for a slot of the planning (#813), the day and
/// the meal follow (`PATCH …/selection`, as « Choisir un jour »). From the
/// field of a planned recipe (`change=servings`, #846), the number alone
/// is changed or removed (`PATCH …/selection`): a selection gone in
/// between is `selection_gone`, not planned again.
pub async fn select(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
    Form(form): Form<SelectForm>,
) -> Response {
    let back = form.back.as_str();
    if form.change == "servings" {
        return match servings_request(&form.servings) {
            Ok(request) => {
                let notice = servings_notice_param(request.servings.flatten());
                patch_plan(me, state, headers, recipe_id, back, request, &notice).await
            }
            Err(code) => {
                Redirect::to(&back_with(back, recipe_id, &format!("error={code}"))).into_response()
            }
        };
    }
    let Some(fam) = family_context(&state, &headers, &me, &back_path(back, recipe_id)).await else {
        return Redirect::to("/groups/new").into_response();
    };
    let servings = match parse_servings(&form.servings) {
        Ok(s) => s,
        Err(code) => {
            return Redirect::to(&back_with(back, recipe_id, &format!("error={code}")))
                .into_response()
        }
    };
    let plan = if form.planned_on.is_empty() && form.meal_slot.is_empty() {
        None
    } else {
        match plan_request(&form.planned_on, &form.meal_slot) {
            Ok(request) => Some(request),
            Err(code) => {
                return Redirect::to(&back_with(back, recipe_id, &format!("error={code}")))
                    .into_response()
            }
        }
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
            if let Some(request) = plan {
                let back = plan_back(back, &request, crate::routes::agenda::today_paris());
                return patch_plan(me, state, headers, recipe_id, &back, request, "plan_saved")
                    .await;
            }
            back_with(back, recipe_id, "notice=recipe_selected")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return recipe_not_found_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            back_with(back, recipe_id, "error=invalid_servings")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            back_with(back, recipe_id, "error=forbidden")
        }
        Ok(_) | Err(_) => back_with(back, recipe_id, "error=unavailable"),
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
    let back = form.back.as_str();
    let Some(fam) = family_context(&state, &headers, &me, &back_path(back, recipe_id)).await else {
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
            back_with(back, recipe_id, "notice=recipe_deselected")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            back_with(back, recipe_id, "error=forbidden")
        }
        Ok(_) | Err(_) => back_with(back, recipe_id, "error=unavailable"),
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

    /// Every role × threshold × authorship (#866), written out rather than
    /// derived: the creator always may; anyone else when their role meets
    /// the family's threshold, the owner whatever it says.
    const CAN_MODIFY_CASES: [(&str, &str, bool, bool); 18] = [
        ("standard", "standard", false, true),
        ("standard", "admin", false, false),
        ("standard", "owner", false, false),
        ("admin", "standard", false, true),
        ("admin", "admin", false, true),
        ("admin", "owner", false, false),
        ("owner", "standard", false, true),
        ("owner", "admin", false, true),
        ("owner", "owner", false, true),
        ("standard", "standard", true, true),
        ("standard", "admin", true, true),
        ("standard", "owner", true, true),
        ("admin", "standard", true, true),
        ("admin", "admin", true, true),
        ("admin", "owner", true, true),
        ("owner", "standard", true, true),
        ("owner", "admin", true, true),
        ("owner", "owner", true, true),
    ];

    #[test]
    fn can_modify_follows_the_threshold_and_spares_the_creator() {
        for (role, min_role, creator, expected) in CAN_MODIFY_CASES {
            assert_eq!(
                can_modify(role, min_role, creator),
                expected,
                "{role} under {min_role}, creator: {creator}"
            );
        }
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
    fn the_planned_tab_is_a_way_back_too() {
        let id = Uuid::from_u128(7);
        assert_eq!(back_path("planned", id), "/recipes?vue=prevues");
    }

    #[test]
    fn the_planning_is_a_way_back_too_on_its_window() {
        let id = Uuid::from_u128(7);
        assert_eq!(back_path("week", id), "/recipes/week");
        assert_eq!(
            back_path("week@2026-11-20", id),
            "/recipes/week?date=2026-11-20"
        );
        assert_eq!(back_path("week@demain", id), "/recipes/week");
        assert_eq!(
            back_with("week@2026-11-20", id, "notice=plan_saved"),
            "/recipes/week?date=2026-11-20&notice=plan_saved"
        );
        assert_eq!(
            back_with("week", id, "error=forbidden"),
            "/recipes/week?error=forbidden"
        );
    }

    #[test]
    fn a_day_chosen_from_the_planning_returns_to_the_window_that_shows_it() {
        let today = day(2026, 11, 14);
        let on = |d: NaiveDate| plan_request(&d.format("%Y-%m-%d").to_string(), "").unwrap();
        // In the current window: the current window.
        assert_eq!(
            plan_back("week@2026-11-27", &on(day(2026, 11, 21)), today),
            "week"
        );
        // Further: the window of the day.
        assert_eq!(
            plan_back("week", &on(day(2026, 11, 22)), today),
            "week@2026-11-20"
        );
        // Without a day, the window the form came from.
        assert_eq!(
            plan_back("week@2026-11-27", &plan_request("", "").unwrap(), today),
            "week@2026-11-27"
        );
        // Another page keeps its own way back.
        assert_eq!(plan_back("detail", &on(day(2026, 11, 22)), today), "detail");
    }

    #[test]
    fn the_select_form_of_a_slot_carries_its_day_and_its_meal() {
        let id = Uuid::from_u128(7);
        let html = select_form_on(
            id,
            Some(4),
            "week",
            "actions",
            (day(2026, 11, 14), "dinner"),
        );
        assert!(
            html.contains(&format!(r#"action="/recipes/{id}/select" class="actions""#)),
            "{html}"
        );
        assert!(html.contains(r#"name="back" value="week""#), "{html}");
        assert!(
            html.contains(r#"<input type="hidden" name="planned_on" value="2026-11-14"/>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="hidden" name="meal_slot" value="dinner"/>"#),
            "{html}"
        );
        assert!(html.contains(r#"value="4""#), "{html}");
        assert!(html.contains(">À réaliser</button>"), "{html}");
        // The plain form carries neither.
        let html = select_form(id, Some(4), "list", "actions");
        assert!(!html.contains("planned_on"), "{html}");
    }

    #[test]
    fn a_notice_is_added_to_the_way_back_whatever_its_query() {
        let id = Uuid::from_u128(7);
        assert_eq!(
            back_with("list", id, "notice=recipe_done"),
            "/recipes?notice=recipe_done"
        );
        assert_eq!(
            back_with("planned", id, "notice=recipe_done"),
            "/recipes?vue=prevues&notice=recipe_done"
        );
        assert_eq!(
            back_with("detail", id, "error=forbidden"),
            format!("/recipes/{id}?error=forbidden")
        );
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
    fn the_outcome_links_open_the_confirmation_pages() {
        let id = Uuid::from_u128(7);
        assert_eq!(
            outcome_links(id, "list"),
            format!(
                r#"<a class="btn secondary" href="/recipes/{id}/done?back=list">Faite</a>
<a class="btn secondary" href="/recipes/{id}/abandon?back=list">Abandonner</a>"#
            )
        );
        // Anything but `list` or `planned` is the recipe.
        assert!(outcome_links(id, "https://evil.test").contains("?back=detail\""));
        // From the « Prévues » tab, back to it.
        assert!(outcome_links(id, "planned")
            .contains(&format!(r#"href="/recipes/{id}/done?back=planned""#)));
    }

    #[test]
    fn the_planned_badge_leaves_the_number_to_its_field() {
        // #846: the number of people is said once, in the field beside it.
        assert_eq!(planned_badge(), r#"<span class="badge">À réaliser</span>"#);
    }

    #[test]
    fn the_servings_form_sends_the_number_of_a_planned_recipe_on_change() {
        let id = Uuid::from_u128(7);
        let html = servings_form(id, Some(4), "detail", "Gratin <dauphinois>");
        assert!(
            html.contains(&format!(
                r#"<form method="post" action="/recipes/{id}/select" class="servings-form" data-keep-scroll>"#
            )),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="hidden" name="back" value="detail"/>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="hidden" name="change" value="servings"/>"#),
            "{html}"
        );
        // The visible « Personnes » opens the accessible name (WCAG 2.5.3).
        assert!(
            html.contains(r#"<label>Personnes <input type="number" name="servings" min="1" max="50" step="1" value="4" aria-label="Personnes, Gratin &lt;dauphinois&gt;" data-submit-on-change/></label>"#),
            "{html}"
        );
        // Without JavaScript, the button sends it.
        assert!(
            html.contains(
                r#"<button type="submit" class="secondary" data-hidden-with-js>Appliquer</button>"#
            ),
            "{html}"
        );
        assert!(!html.contains("placeholder"), "{html}");
        // A selection that says no number: the field is blank, « Optionnel ».
        let html = servings_form(id, None, "planned", "Soupe");
        assert!(
            html.contains(
                r#"step="1" value="" placeholder="Optionnel" aria-label="Personnes, Soupe""#
            ),
            "{html}"
        );
        assert!(html.contains(r#"name="back" value="planned""#), "{html}");
    }

    #[test]
    fn a_servings_change_patches_the_number_or_removes_it() {
        assert_eq!(
            serde_json::to_value(servings_request(" 6 ").unwrap()).unwrap(),
            serde_json::json!({"servings": 6})
        );
        // A field emptied removes the number, rather than being refused.
        assert_eq!(
            serde_json::to_value(servings_request("").unwrap()).unwrap(),
            serde_json::json!({"servings": null})
        );
        // Neither the day nor the meal is sent: both stay as they are.
        let r = servings_request("6").unwrap();
        assert_eq!((r.planned_on, r.meal_slot), (None, None));
        for bad in ["0", "51", "quatre", "2.5", "-1"] {
            assert_eq!(
                servings_request(bad).unwrap_err(),
                "invalid_servings",
                "{bad}"
            );
        }
    }

    #[test]
    fn a_servings_change_is_told_with_its_number() {
        assert_eq!(
            servings_notice_param(Some(6)),
            "servings_updated&servings=6"
        );
        assert_eq!(servings_notice_param(None), "servings_cleared");
        assert_eq!(
            back_with(
                "list",
                Uuid::from_u128(7),
                &format!("notice={}", servings_notice_param(Some(6)))
            ),
            "/recipes?notice=servings_updated&servings=6"
        );
        assert_eq!(
            servings_notice("servings_updated", Some("6")).as_deref(),
            Some("Nombre de personnes mis à jour : 6.")
        );
        // A number the form could not have sent is not repeated.
        for odd in [None, Some(""), Some("0"), Some("<b>")] {
            assert_eq!(
                servings_notice("servings_updated", odd).as_deref(),
                Some("Nombre de personnes mis à jour."),
                "{odd:?}"
            );
        }
        assert_eq!(
            servings_notice("servings_cleared", None).as_deref(),
            Some("Nombre de personnes retiré : les quantités de la recette sont prises telles qu'écrites.")
        );
        assert_eq!(servings_notice("recipe_selected", Some("6")), None);
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn the_meals_are_midi_and_soir() {
        assert_eq!(meal_word("lunch"), Some("midi"));
        assert_eq!(meal_word("dinner"), Some("soir"));
        assert_eq!(meal_word("brunch"), None);
        assert_eq!(meal_word(""), None);
    }

    #[test]
    fn the_plan_form_sends_the_day_and_the_meal_or_their_removal() {
        let r = plan_request("2026-11-19", "dinner").unwrap();
        assert_eq!(r.servings, None);
        assert_eq!(r.planned_on, Some(Some(day(2026, 11, 19))));
        assert_eq!(r.meal_slot, Some(Some("dinner".to_string())));
        // A day without a meal removes the meal.
        let r = plan_request(" 2026-11-19 ", "").unwrap();
        assert_eq!(r.planned_on, Some(Some(day(2026, 11, 19))));
        assert_eq!(r.meal_slot, Some(None));
        // Nothing removes both, and the number of people is not sent.
        let r = plan_request("", "").unwrap();
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            serde_json::json!({"planned_on": null, "meal_slot": null})
        );
        assert_eq!(
            serde_json::to_value(plan_request("2026-11-19", "lunch").unwrap()).unwrap(),
            serde_json::json!({"planned_on": "2026-11-19", "meal_slot": "lunch"})
        );
    }

    #[test]
    fn the_plan_form_refuses_a_meal_without_a_day_and_what_is_not_a_date() {
        assert_eq!(plan_request("", "lunch").unwrap_err(), "meal_without_day");
        assert_eq!(
            plan_request("  ", "dinner").unwrap_err(),
            "meal_without_day"
        );
        assert_eq!(
            plan_request("19/11/2026", "").unwrap_err(),
            "invalid_planned_on"
        );
        assert_eq!(
            plan_request("2026-02-30", "").unwrap_err(),
            "invalid_planned_on"
        );
        assert_eq!(
            plan_request("2026-11-19", "brunch").unwrap_err(),
            "invalid_meal_slot"
        );
    }

    #[test]
    fn the_plan_badge_names_the_day_and_the_meal_only_when_dated() {
        assert_eq!(plan_badge(None, None), "");
        assert_eq!(
            plan_badge(Some(day(2026, 11, 19)), Some("lunch")),
            r#"<span class="badge accent">jeu. 19/11 · midi</span>"#
        );
        assert_eq!(
            plan_badge(Some(day(2026, 11, 21)), None),
            r#"<span class="badge accent">sam. 21/11</span>"#
        );
        assert_eq!(
            plan_badge(Some(day(2026, 11, 20)), Some("dinner")),
            r#"<span class="badge accent">ven. 20/11 · soir</span>"#
        );
        // A meal the page does not know is not shown.
        assert_eq!(
            plan_badge(Some(day(2026, 11, 21)), Some("brunch")),
            r#"<span class="badge accent">sam. 21/11</span>"#
        );
    }
}
