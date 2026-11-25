//! `/recipes/library` (#910) — the recipe library, shared by every family
//! (API #906, #907): search it by name, page through it 20 at a time by
//! links, open a publication, and add it to the family's recipes. Nothing
//! of who published a recipe, nor of their family, is shown: the API does
//! not send it.
//!
//! - `GET /recipes/library?q=…&apres=…` — the list. The API pages by cursor
//!   (`next_cursor`); `apres` carries the cursors of the pages already
//!   passed, `~`-separated, so that « Précédent » is a link too: page `n`
//!   holds `n − 1` cursors, its own is the last.
//! - `GET /recipes/library/:id` — a publication. When the family already
//!   holds a copy of it (`library_recipe_id` of its recipes), « Ajouter » is
//!   replaced by « Voir ma copie » (arbitrage of 2026-11-20). A publication
//!   the API no longer knows (withdrawn by its publisher or by moderation)
//!   says « retirée »: its content left with it, and so did the link of any
//!   copy (`ON DELETE SET NULL`), which stays among the family's recipes.
//! - `POST /recipes/library/:id/add` — the copy
//!   (`POST /groups/:gid/library/recipes/:id/copy`), then back to the
//!   publication (PRG): `?notice=added`, or as it now stands.
//!
//! #911 adds « Mes publications » beside the list's title (`mine.rs`), the
//! page a publication was imported from (« Source : <site> ») under its
//! title, and « Signaler cette recette » at its foot (`report.rs`), or the
//! day the reader reported it; `?notice=reported` after a report.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::{DateTime, Utc};
use manage_our_home_shared::dto::recipes::{
    IngredientResponse, LibraryPage, LibraryRecipe, LibraryRecipeSummary, RecipeList,
    RecipeResponse,
};
use manage_our_home_shared::validation::recipes::{duration_label, servings_label, steps_of};
use uuid::Uuid;

use crate::app::{html_escape, kitchen_segment, shell_with_header, Width};
use crate::icons::{icon, Icon};
use crate::layout::CurrentUser;
use crate::lenient_query::LenientQuery;
use crate::routes::account::allergies::long_date;
use crate::state::{api_request_auth, AppState};

use super::detail::{ingredient_html, source_html};
use super::{family_context, forbidden_page, reader_context, recipes_cookie};

/// Publications per page: the API's default page, asked for explicitly.
pub(crate) const PAGE_SIZE: i64 = 20;

/// The longest search the API takes (`SEARCH_MAX_CHARS`).
const SEARCH_MAX: usize = 100;

/// Between two cursors of `apres`: a character no cursor holds (a cursor is
/// `<microseconds>.<uuid>`).
const TRAIL_SEP: char = '~';

const BADGE: &str = r#"<span class="badge accent">Issue de la bibliothèque</span>"#;

#[derive(serde::Deserialize, Default)]
pub struct ListQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    apres: Option<String>,
}

#[derive(serde::Deserialize, Default)]
pub struct DetailQuery {
    #[serde(default)]
    notice: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

// -- pure logic ---------------------------------------------------------------

/// The cursors of the pages before this one, as `apres` carries them;
/// blanks dropped.
pub(crate) fn trail_of(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(TRAIL_SEP)
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .collect()
}

/// The list's address for a search and the cursors of the pages before the
/// one it opens: `q` and `apres` only when they hold something.
pub(crate) fn list_href(q: &str, trail: &[String]) -> String {
    let mut pairs: Vec<(&str, String)> = Vec::new();
    if !q.trim().is_empty() {
        pairs.push(("q", q.trim().to_string()));
    }
    if !trail.is_empty() {
        pairs.push(("apres", trail.join(&TRAIL_SEP.to_string())));
    }
    if pairs.is_empty() {
        return "/recipes/library".to_string();
    }
    format!(
        "/recipes/library?{}",
        serde_urlencoded::to_string(&pairs).unwrap_or_default()
    )
}

/// The API's address for that page: 20 entries, after the last cursor.
pub(crate) fn api_list_path(q: &str, trail: &[String]) -> String {
    let mut pairs: Vec<(&str, String)> = vec![("limit", PAGE_SIZE.to_string())];
    if !q.trim().is_empty() {
        pairs.push(("q", q.trim().to_string()));
    }
    if let Some(cursor) = trail.last() {
        pairs.push(("cursor", cursor.clone()));
    }
    format!(
        "/library/recipes?{}",
        serde_urlencoded::to_string(&pairs).unwrap_or_default()
    )
}

/// How many pages `total` publications fill; one, even empty.
pub(crate) fn page_count(total: i64) -> i64 {
    ((total + PAGE_SIZE - 1) / PAGE_SIZE).max(1)
}

/// The line that says what the list holds: the whole library, or what a
/// search found.
pub(crate) fn count_text(total: i64, q: &str) -> String {
    let q = q.trim();
    if q.is_empty() {
        match total {
            1 => "1 recette".to_string(),
            n => format!("{n} recettes"),
        }
    } else {
        let noun = if total > 1 { "résultats" } else { "résultat" };
        format!("{total} {noun} pour « {q} »")
    }
}

/// A row's summary: for how many, how long, how many ingredients — what the
/// publication says of them.
pub(crate) fn summary_line(s: &LibraryRecipeSummary) -> String {
    let mut parts = Vec::new();
    if let Some(n) = s.servings {
        parts.push(servings_label(n));
    }
    if let Some(m) = s.duration_minutes {
        parts.push(duration_label(m));
    }
    match s.ingredient_count {
        0 => {}
        1 => parts.push("1 ingrédient".to_string()),
        n => parts.push(format!("{n} ingrédients")),
    }
    parts.join(" · ")
}

/// The family's copies: publication → its recipe.
pub(crate) fn copies(recipes: &[RecipeResponse]) -> HashMap<Uuid, Uuid> {
    recipes
        .iter()
        .filter_map(|r| r.library_recipe_id.map(|l| (l, r.id)))
        .collect()
}

/// Where « Ajouter à mes recettes » leads, by the API's answer to the copy.
/// A copy already there, or a publication gone, is shown as it now stands.
pub(crate) fn add_target(library_id: Uuid, status: Option<u16>) -> String {
    let fiche = format!("/recipes/library/{library_id}");
    match status {
        Some(201) => format!("{fiche}?notice=added"),
        Some(409) | Some(404) => fiche,
        Some(429) => format!("{fiche}?error=too_many"),
        _ => format!("{fiche}?error=unavailable"),
    }
}

/// French copy of a refusal: `too_many` (the API's 429), `search_too_long`,
/// `invalid_cursor`; anything else is an outage.
pub(crate) fn error_text(code: &str) -> &'static str {
    match code {
        "too_many" => {
            "Trop de consultations de la bibliothèque en peu de temps. Réessayez dans une minute."
        }
        "search_too_long" => "Cette recherche est trop longue : 100 caractères au plus.",
        "invalid_cursor" => "Ce lien de page n'est plus valable. Reprenez depuis la première page.",
        _ => "La bibliothèque est momentanément indisponible. Vos recettes ne sont pas touchées.",
    }
}

// -- rendering ----------------------------------------------------------------

fn search_form(q: &str) -> String {
    format!(
        r#"<form method="get" action="/recipes/library" role="search" class="card inline find" aria-label="Rechercher dans la bibliothèque">
<label>Rechercher une recette <input type="search" name="q" value="{q}" maxlength="{SEARCH_MAX}" autocomplete="off" placeholder="Nom de la recette : gratin, lentilles…"/></label>
<button type="submit" data-busy-label="Recherche en cours…">Rechercher</button>
</form>"#,
        q = html_escape(q),
    )
}

fn row_html(s: &LibraryRecipeSummary, copied: bool) -> String {
    let summary = summary_line(s);
    let summary = if summary.is_empty() {
        String::new()
    } else {
        format!(r#"<span class="muted">{}</span>"#, html_escape(&summary))
    };
    format!(
        r#"<li><a class="list-row" href="/recipes/library/{id}"><span><strong>{name}</strong>{summary}{badge}</span>{next}</a></li>"#,
        id = s.id,
        name = html_escape(&s.name),
        badge = if copied { BADGE } else { "" },
        next = icon(Icon::Next),
    )
}

/// « Précédent » or « Suivant »: a link, or the same box marked unavailable
/// (no `href`, so out of the tab order) at either end.
fn pager_link(href: Option<String>, rel: &str, label: &str) -> String {
    match href {
        Some(href) => format!(
            r#"<a class="btn secondary" href="{href}" rel="{rel}">{label}</a>"#,
            href = html_escape(&href),
        ),
        None => format!(r#"<a class="btn secondary" aria-disabled="true">{label}</a>"#),
    }
}

/// What the list shows under the search: the results and their pages, or
/// why there are none.
pub(crate) fn results_html(
    page: &LibraryPage,
    q: &str,
    trail: &[String],
    copied: &HashMap<Uuid, Uuid>,
    family: bool,
) -> String {
    let q = q.trim();
    if page.total == 0 && page.recipes.is_empty() {
        if q.is_empty() {
            // Without a family, « Mes recettes » would only redirect (#990).
            if !family {
                return r#"<div class="card void"><h2>La bibliothèque est vide pour le moment</h2>
<p class="muted">Aucune recette n'a encore été partagée. Revenez plus tard.</p></div>"#
                    .to_string();
            }
            return r#"<div class="card void"><h2>La bibliothèque est vide pour le moment</h2>
<p class="muted">Aucune recette n'a encore été partagée. Revenez plus tard, ou ajoutez vos propres recettes depuis « Mes recettes ».</p>
<a class="btn secondary" href="/recipes">Mes recettes</a></div>"#
                .to_string();
        }
        return format!(
            r#"<p class="muted" role="status">{count}.</p>
<div class="card void"><h2>Aucune recette ne correspond</h2>
<p class="muted">Essayez un autre mot, ou une partie seulement du nom.</p>
<a class="btn secondary" href="/recipes/library">Effacer la recherche</a></div>"#,
            count = html_escape(&count_text(0, q)),
        );
    }
    let rows: String = page
        .recipes
        .iter()
        .map(|s| row_html(s, copied.contains_key(&s.id)))
        .collect();
    let number = trail.len() as i64 + 1;
    let prev = (!trail.is_empty()).then(|| list_href(q, &trail[..trail.len() - 1]));
    let next = page.next_cursor.as_ref().map(|c| {
        let mut on = trail.to_vec();
        on.push(c.clone());
        list_href(q, &on)
    });
    format!(
        r#"<h2 id="lib-h">Recettes</h2>
<p class="muted" role="status">{count}</p>
<ul class="list lib" aria-labelledby="lib-h">{rows}</ul>
<nav class="pager" aria-label="Pagination">{prev}<span class="muted">Page {number} sur {pages}</span>{next}</nav>"#,
        count = html_escape(&count_text(page.total, q)),
        pages = page_count(page.total).max(number),
        prev = pager_link(
            prev,
            "prev",
            r#"<span aria-hidden="true">‹</span> Précédent"#
        ),
        next = pager_link(next, "next", r#"Suivant <span aria-hidden="true">›</span>"#),
    )
}

/// The list page: its header, the search, then `results` (or the refusal).
/// Without a family, no « Mes recettes »: it would only redirect (#990).
pub(crate) fn list_body(q: &str, results: &str, family: bool) -> String {
    let mine = if family {
        r#"<a class="btn secondary" href="/recipes">Mes recettes</a>"#
    } else {
        ""
    };
    format!(
        r#"{segment}<div class="page-header">
<h1>Bibliothèque</h1>
<div class="actions"><a class="btn secondary" href="/recipes/library/mine">Mes publications</a>{mine}</div>
</div>
<p class="muted">Des recettes à copier dans les vôtres, pour les adapter à votre foyer.</p>
{search}
{results}"#,
        segment = library_segment(family),
        search = search_form(q),
    )
}

/// The kitchen's segment over a library page. The library opens without a
/// family (#990), the family's recipes do not: there, « Recettes » would
/// only lead to `/groups/new`, so the tab is left out (arbitrage of
/// 2026-11-25).
pub(crate) fn library_segment(family: bool) -> String {
    if family {
        kitchen_segment("/recipes")
    } else {
        r#"<nav class="segment" aria-label="Cuisine"><a href="/stocks">Stocks</a></nav>"#
            .to_string()
    }
}

/// A refusal of the list or of a publication, with the way to ask again.
pub(super) fn error_html(code: &str, retry: &str) -> String {
    format!(
        r#"<p class="notice error" role="alert">{text} <a href="{retry}">Réessayer</a></p>"#,
        text = html_escape(error_text(code)),
        retry = html_escape(retry),
    )
}

/// A publication's steps (#911): numbered and aired as its ingredients are
/// (`.spaced`), as the mockup draws them. The family's page keeps its own
/// (`detail::steps_html`).
pub(crate) fn steps_html(instructions: Option<&str>) -> String {
    let steps = steps_of(instructions.unwrap_or_default());
    if steps.is_empty() {
        return String::new();
    }
    let items: String = steps
        .iter()
        .map(|s| format!("<li>{}</li>", html_escape(s)))
        .collect();
    format!(r#"<h2>Étapes</h2><ol class="spaced">{items}</ol>"#)
}

/// The foot of a publication (#911): the way to report it, or the day the
/// reader did — once per reader (the API's).
pub(crate) fn report_foot(id: Uuid, reported_at: Option<DateTime<Utc>>) -> String {
    let inner = match reported_at {
        Some(at) => format!(
            r#"<p class="muted">{check} Vous avez signalé cette recette le {day}.</p>"#,
            check = icon(Icon::Check),
            day = html_escape(&long_date(at)),
        ),
        None => format!(
            r#"<p class="muted">Un problème avec cette recette (contenu inapproprié, illégal, erroné ou dangereux) ?</p>
<a class="btn secondary" href="/recipes/library/{id}/report">Signaler cette recette</a>"#
        ),
    };
    format!(r#"<div class="report-foot">{inner}</div>"#)
}

/// A publication's page. `copy` is the family's copy of it, if any;
/// `notice` and `error` the codes of the URL. Without a `family` (#990)
/// there is nothing to copy into: the form gives way to a card that
/// sends to `/groups/new`, the one way to have a group of one's own —
/// joining one goes through an invitation.
pub(crate) fn detail_body(
    recipe: &LibraryRecipe,
    copy: Option<Uuid>,
    notice: Option<&str>,
    error: Option<&str>,
    family: bool,
) -> String {
    let id = recipe.id;
    let notice = match (notice, copy) {
        (Some("added"), Some(c)) => format!(
            r#"<p class="notice success" role="status">Recette ajoutée à vos recettes. <a href="/recipes/{c}">Ouvrir ma copie</a></p>"#
        ),
        (Some("added"), None) => {
            r#"<p class="notice success" role="status">Recette ajoutée à vos recettes.</p>"#
                .to_string()
        }
        (Some("reported"), _) => {
            r#"<p class="notice success" role="status">Merci, votre signalement a été transmis. Nous l'examinerons.</p>"#
                .to_string()
        }
        _ => String::new(),
    };
    let error = error
        .map(|code| {
            format!(
                r#"<p class="notice error" role="alert">{}</p>"#,
                html_escape(error_text(code))
            )
        })
        .unwrap_or_default();
    let mut meta = Vec::new();
    if let Some(n) = recipe.servings {
        meta.push(format!("Pour {}", servings_label(n)));
    }
    if let Some(m) = recipe.duration_minutes {
        meta.push(format!("Temps : {}", duration_label(m)));
    }
    let meta = html_escape(&meta.join(" · "));
    let meta = match (meta.is_empty(), copy.is_some()) {
        (true, false) => String::new(),
        (true, true) => format!(r#"<p class="muted">{BADGE}</p>"#),
        (false, false) => format!(r#"<p class="muted">{meta}</p>"#),
        (false, true) => format!(r#"<p class="muted">{meta} {BADGE}</p>"#),
    };
    let action = match copy {
        _ if !family => r#"<div class="card"><p><strong>Ajouter cette recette à un groupe</strong></p>
<p class="muted">Pour en garder une copie à adapter, il faut un groupe : créez le vôtre, ou rejoignez celui de vos proches avec leur invitation.</p>
<a class="btn" href="/groups/new">Ajouter à mon groupe</a></div>"#
            .to_string(),
        Some(c) => format!(
            r#"<div class="card"><p>Cette recette est déjà dans vos recettes.</p>
<a class="btn secondary" href="/recipes/{c}">Voir ma copie</a></div>"#
        ),
        None => format!(
            r#"<form method="post" action="/recipes/library/{id}/add" class="card">
<button type="submit" data-busy-label="Ajout en cours…">Ajouter à mes recettes</button>
<p class="muted">Une copie est ajoutée à vos recettes. Vous pourrez la modifier sans toucher à l'original.</p>
</form>"#
        ),
    };
    let ingredients = if recipe.ingredients.is_empty() {
        r#"<p class="muted">Aucun ingrédient renseigné.</p>"#.to_string()
    } else {
        let items: String = recipe
            .ingredients
            .iter()
            .map(|i| {
                ingredient_html(&IngredientResponse {
                    id: Uuid::nil(),
                    name: i.name.clone(),
                    quantity: i.quantity,
                    unit: i.unit.clone(),
                    is_optional: i.is_optional,
                    seasonal_months: i.seasonal_months.clone(),
                })
            })
            .collect();
        format!(r#"<ul class="spaced">{items}</ul>"#)
    };
    format!(
        r#"{segment}<div class="links"><a href="/recipes/library"><span aria-hidden="true">‹</span> Retour à la bibliothèque</a></div>
{notice}{error}
<h1>{name}</h1>
{meta}
{source}
{action}
<h2>Ingrédients</h2>
{ingredients}
{steps}
{foot}"#,
        segment = library_segment(family),
        name = html_escape(&recipe.name),
        source = source_html(recipe.source_url.as_deref()),
        steps = steps_html(recipe.instructions.as_deref()),
        foot = report_foot(id, recipe.reported_at),
    )
}

/// A publication the API no longer knows: withdrawn, by its publisher or
/// by moderation. Its content left with it.
pub(super) fn withdrawn_body(family: bool) -> String {
    format!(
        r#"{segment}<h1>Recette retirée</h1>
<p class="notice warning" role="status">Cette recette a été retirée de la bibliothèque et ne peut plus être ajoutée.</p>
<div class="actions"><a class="btn secondary" href="/recipes/library">Retour à la bibliothèque</a></div>"#,
        segment = library_segment(family),
    )
}

// -- handlers -----------------------------------------------------------------

/// The family's recipes, for its copies; unknown (empty) when the API does
/// not answer them: a badge then goes missing, and « Ajouter » meets the
/// API's `already_copied`, which leads back here.
async fn family_copies(state: &AppState, gid: Uuid, cookie: Option<&str>) -> HashMap<Uuid, Uuid> {
    match api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/recipes"),
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<RecipeList>(resp.body)
                .map(|l| copies(&l.recipes))
                .unwrap_or_default()
        }
        Ok(_) | Err(_) => HashMap::new(),
    }
}

/// The API's error code of a refusal, `too_many` for a 429.
pub(super) fn refusal_code(status: reqwest::StatusCode, body: &serde_json::Value) -> String {
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return "too_many".to_string();
    }
    body.get("error")
        .and_then(|e| e.as_str())
        .unwrap_or("unavailable")
        .to_string()
}

pub async fn list(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    LenientQuery(query): LenientQuery<ListQuery>,
) -> Response {
    // No family needed to read the library (#990): its copies are then
    // none, and so are their badges.
    let (fam, header) = reader_context(&state, &headers, &me, "/recipes/library").await;
    let q: String = query
        .q
        .as_deref()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(SEARCH_MAX)
        .collect();
    let trail = trail_of(query.apres.as_deref());
    let cookie = recipes_cookie(&headers);

    let results = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &api_list_path(&q, &trail),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match serde_json::from_value::<LibraryPage>(resp.body) {
                Ok(page) => {
                    let copied = match &fam {
                        Some(fam) => family_copies(&state, fam.gid, cookie.as_deref()).await,
                        None => HashMap::new(),
                    };
                    results_html(&page, &q, &trail, &copied, fam.is_some())
                }
                Err(_) => error_html("unavailable", &list_href(&q, &trail)),
            }
        }
        Ok(resp) => {
            let code = refusal_code(resp.status, &resp.body);
            // A page link gone stale starts again from the first page.
            let retry = if code == "invalid_cursor" {
                list_href(&q, &[])
            } else {
                list_href(&q, &trail)
            };
            error_html(&code, &retry)
        }
        Err(_) => error_html("unavailable", &list_href(&q, &trail)),
    };
    Html(shell_with_header(
        Width::Read,
        "Bibliothèque",
        &header,
        &list_body(&q, &results, fam.is_some()),
    ))
    .into_response()
}

pub async fn detail(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    LenientQuery(query): LenientQuery<DetailQuery>,
) -> Response {
    let here = format!("/recipes/library/{id}");
    // Read without a family too (#990): « Ajouter » then sends to
    // `/groups/new`, and there is no copy to look for.
    let (fam, header) = reader_context(&state, &headers, &me, &here).await;
    let family = fam.is_some();
    let cookie = recipes_cookie(&headers);
    let page = |status: StatusCode, title: &str, body: &str| {
        (
            status,
            Html(shell_with_header(Width::Read, title, &header, body)),
        )
            .into_response()
    };

    match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/library/recipes/{id}"),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match serde_json::from_value::<LibraryRecipe>(resp.body) {
                Ok(recipe) => {
                    let copy = match &fam {
                        Some(fam) => family_copies(&state, fam.gid, cookie.as_deref())
                            .await
                            .get(&id)
                            .copied(),
                        None => None,
                    };
                    let body = detail_body(
                        &recipe,
                        copy,
                        query.notice.as_deref(),
                        query.error.as_deref(),
                        family,
                    );
                    page(StatusCode::OK, &recipe.name, &body)
                }
                Err(_) => page(
                    StatusCode::OK,
                    "Bibliothèque",
                    &error_html("unavailable", &here),
                ),
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => page(
            StatusCode::NOT_FOUND,
            "Recette retirée",
            &withdrawn_body(family),
        ),
        Ok(resp) => {
            let code = refusal_code(resp.status, &resp.body);
            page(StatusCode::OK, "Bibliothèque", &error_html(&code, &here))
        }
        Err(_) => page(
            StatusCode::OK,
            "Bibliothèque",
            &error_html("unavailable", &here),
        ),
    }
}

pub async fn add(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let here = format!("/recipes/library/{id}");
    let Some(fam) = family_context(&state, &headers, &me, &here).await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);
    let status = match api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/library/recipes/{id}/copy", fam.gid),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            return forbidden_page().into_response()
        }
        Ok(resp) => Some(resp.status.as_u16()),
        Err(_) => None,
    };
    Redirect::to(&add_target(id, status)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use manage_our_home_shared::dto::recipes::LibraryIngredient;

    fn summary(n: u128, name: &str) -> LibraryRecipeSummary {
        LibraryRecipeSummary {
            id: Uuid::from_u128(n),
            name: name.to_string(),
            servings: Some(4),
            duration_minutes: Some(80),
            ingredient_count: 6,
        }
    }

    fn page(total: i64, n: usize, next: Option<&str>) -> LibraryPage {
        LibraryPage {
            recipes: (0..n)
                .map(|i| summary(i as u128 + 1, &format!("Plat {i}")))
                .collect(),
            next_cursor: next.map(str::to_string),
            total,
        }
    }

    fn trail(cursors: &[&str]) -> Vec<String> {
        cursors.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn the_trail_reads_the_cursors_passed_in_order() {
        assert!(trail_of(None).is_empty());
        assert!(trail_of(Some("")).is_empty());
        assert_eq!(trail_of(Some("1.a~2.b")), ["1.a", "2.b"]);
        assert_eq!(trail_of(Some("~1.a~~ 2.b ~")), ["1.a", "2.b"]);
    }

    #[test]
    fn the_list_href_holds_only_what_it_needs() {
        assert_eq!(list_href("", &[]), "/recipes/library");
        assert_eq!(list_href("  ", &[]), "/recipes/library");
        assert_eq!(list_href("gratin", &[]), "/recipes/library?q=gratin");
        assert_eq!(
            list_href(" tarte & co ", &[]),
            "/recipes/library?q=tarte+%26+co"
        );
        assert_eq!(
            list_href("", &trail(&["17.a", "-3.b"])),
            "/recipes/library?apres=17.a%7E-3.b"
        );
        assert_eq!(
            list_href("gratin", &trail(&["17.a"])),
            "/recipes/library?q=gratin&apres=17.a"
        );
        // What it writes, the trail reads back.
        let href = list_href("x", &trail(&["1.a", "2.b"]));
        let query: Vec<(String, String)> =
            serde_urlencoded::from_str(href.split_once('?').unwrap().1).unwrap();
        assert_eq!(query[1].0, "apres");
        assert_eq!(trail_of(Some(&query[1].1)), ["1.a", "2.b"]);
    }

    #[test]
    fn the_api_is_asked_for_20_after_the_last_cursor() {
        assert_eq!(api_list_path("", &[]), "/library/recipes?limit=20");
        assert_eq!(
            api_list_path(" pâtes ", &trail(&["1.a", "2.b"])),
            "/library/recipes?limit=20&q=p%C3%A2tes&cursor=2.b"
        );
    }

    #[test]
    fn pages_hold_20_and_there_is_always_one() {
        assert_eq!(page_count(0), 1);
        assert_eq!(page_count(1), 1);
        assert_eq!(page_count(20), 1);
        assert_eq!(page_count(21), 2);
        assert_eq!(page_count(128), 7);
        assert_eq!(page_count(231), 12);
    }

    #[test]
    fn the_count_agrees_in_number_and_names_the_search() {
        assert_eq!(count_text(128, ""), "128 recettes");
        assert_eq!(count_text(1, " "), "1 recette");
        assert_eq!(count_text(0, ""), "0 recettes");
        assert_eq!(count_text(3, " gratin "), "3 résultats pour « gratin »");
        assert_eq!(count_text(1, "gratin"), "1 résultat pour « gratin »");
        assert_eq!(count_text(0, "tatin"), "0 résultat pour « tatin »");
    }

    #[test]
    fn a_summary_says_what_the_publication_says() {
        assert_eq!(
            summary_line(&summary(1, "x")),
            "4 personnes · 1 h 20 · 6 ingrédients"
        );
        let mut s = summary(1, "x");
        s.servings = None;
        s.ingredient_count = 1;
        assert_eq!(summary_line(&s), "1 h 20 · 1 ingrédient");
        s.duration_minutes = None;
        s.ingredient_count = 0;
        assert_eq!(summary_line(&s), "");
    }

    #[test]
    fn copies_map_each_publication_to_the_familys_recipe() {
        let recipe = |id: u128, library: Option<u128>| -> RecipeResponse {
            serde_json::from_value(serde_json::json!({
                "id": Uuid::from_u128(id),
                "group_id": Uuid::nil(),
                "created_by": Uuid::nil(),
                "name": "r",
                "instructions": null,
                "ingredients": [],
                "created_at": "2026-11-20T10:00:00Z",
                "updated_at": "2026-11-20T10:00:00Z",
                "library_recipe_id": library.map(Uuid::from_u128),
            }))
            .unwrap()
        };
        let map = copies(&[recipe(1, None), recipe(2, Some(10)), recipe(3, Some(11))]);
        assert_eq!(map.len(), 2);
        assert_eq!(map[&Uuid::from_u128(10)], Uuid::from_u128(2));
        assert_eq!(map[&Uuid::from_u128(11)], Uuid::from_u128(3));
    }

    #[test]
    fn adding_leads_back_to_the_publication_as_it_now_stands() {
        let id = Uuid::from_u128(7);
        let fiche = format!("/recipes/library/{id}");
        assert_eq!(add_target(id, Some(201)), format!("{fiche}?notice=added"));
        // Already copied: the page offers the copy. Gone: it says so.
        assert_eq!(add_target(id, Some(409)), fiche);
        assert_eq!(add_target(id, Some(404)), fiche);
        assert_eq!(add_target(id, Some(429)), format!("{fiche}?error=too_many"));
        assert_eq!(
            add_target(id, Some(500)),
            format!("{fiche}?error=unavailable")
        );
        assert_eq!(add_target(id, None), format!("{fiche}?error=unavailable"));
    }

    #[test]
    fn every_refusal_has_its_own_words() {
        let codes = [
            "too_many",
            "search_too_long",
            "invalid_cursor",
            "unavailable",
        ];
        let mut texts: Vec<&str> = codes.iter().map(|c| error_text(c)).collect();
        texts.sort();
        texts.dedup();
        assert_eq!(texts.len(), codes.len());
        assert_eq!(error_text("anything"), error_text("unavailable"));
    }

    #[test]
    fn an_empty_library_and_an_empty_search_say_different_things() {
        let none = HashMap::new();
        let empty = results_html(&page(0, 0, None), "", &[], &none, true);
        assert!(empty.contains("La bibliothèque est vide pour le moment"));
        assert!(!empty.contains("pager"));
        let missed = results_html(&page(0, 0, None), "tatin", &[], &none, true);
        assert!(missed.contains(r#"<p class="muted" role="status">0 résultat pour « tatin ».</p>"#));
        assert!(missed.contains("Aucune recette ne correspond"));
        assert!(missed.contains(r#"href="/recipes/library">Effacer la recherche"#));
    }

    #[test]
    fn the_first_page_links_forward_only() {
        let html = results_html(&page(128, 20, Some("9.z")), "", &[], &HashMap::new(), true);
        assert!(html.contains(r#"<p class="muted" role="status">128 recettes</p>"#));
        assert_eq!(html.matches(r#"<li><a class="list-row""#).count(), 20);
        assert!(html.contains("Page 1 sur 7"));
        assert!(html.contains(
            r#"<a class="btn secondary" aria-disabled="true"><span aria-hidden="true">‹</span> Précédent</a>"#
        ));
        assert!(html.contains(r#"href="/recipes/library?apres=9.z" rel="next""#));
    }

    #[test]
    fn a_middle_page_links_both_ways_and_keeps_the_search() {
        let html = results_html(
            &page(231, 20, Some("4.d")),
            "gratin",
            &trail(&["1.a", "2.b", "3.c"]),
            &HashMap::new(),
            true,
        );
        assert!(html.contains("Page 4 sur 12"));
        assert!(html.contains("231 résultats pour « gratin »"));
        assert!(html.contains(r#"href="/recipes/library?q=gratin&amp;apres=1.a%7E2.b" rel="prev""#));
        assert!(html.contains(
            r#"href="/recipes/library?q=gratin&amp;apres=1.a%7E2.b%7E3.c%7E4.d" rel="next""#
        ));
        // The second page goes back to the first, which has no trail.
        let second = results_html(
            &page(30, 10, None),
            "",
            &trail(&["1.a"]),
            &HashMap::new(),
            true,
        );
        assert!(second.contains(r#"href="/recipes/library" rel="prev""#));
        assert!(second.contains(
            r#"<a class="btn secondary" aria-disabled="true">Suivant <span aria-hidden="true">›</span></a>"#
        ));
    }

    #[test]
    fn a_row_links_its_publication_and_marks_a_copy() {
        let mut copied = HashMap::new();
        copied.insert(Uuid::from_u128(2), Uuid::from_u128(99));
        let html = results_html(&page(3, 3, None), "", &[], &copied, true);
        let rows: Vec<&str> = html.split("<li>").skip(1).collect();
        assert_eq!(rows.len(), 3);
        assert!(rows[0].contains(&format!(
            r#"href="/recipes/library/{}""#,
            Uuid::from_u128(1)
        )));
        assert!(rows[0].contains("<strong>Plat 0</strong>"));
        assert!(rows[0].contains("4 personnes · 1 h 20 · 6 ingrédients"));
        assert!(!rows[0].contains("Issue de la bibliothèque"));
        assert!(rows[1].contains(BADGE));
        let escaped = results_html(
            &LibraryPage {
                recipes: vec![summary(1, "<b>Tarte</b>")],
                next_cursor: None,
                total: 1,
            },
            "",
            &[],
            &HashMap::new(),
            true,
        );
        assert!(escaped.contains("&lt;b&gt;Tarte&lt;/b&gt;"));
    }

    fn publication() -> LibraryRecipe {
        LibraryRecipe {
            id: Uuid::from_u128(5),
            name: "Gratin dauphinois".into(),
            instructions: Some("Préchauffer.\nCuire.".into()),
            servings: Some(4),
            duration_minutes: Some(80),
            ingredients: vec![LibraryIngredient {
                name: "Pommes de terre".into(),
                quantity: Some(1.0),
                unit: Some("kg".into()),
                is_optional: false,
                seasonal_months: None,
            }],
            source_url: None,
            reported_at: None,
        }
    }

    #[test]
    fn a_publication_not_copied_offers_to_add_it() {
        let html = detail_body(&publication(), None, None, None, true);
        assert!(html.contains("<h1>Gratin dauphinois</h1>"));
        assert!(html.contains(r#"<p class="muted">Pour 4 personnes · Temps : 1 h 20</p>"#));
        assert!(html.contains(&format!(
            r#"<form method="post" action="/recipes/library/{}/add" class="card">"#,
            Uuid::from_u128(5)
        )));
        assert!(html.contains("Ajouter à mes recettes"));
        assert!(!html.contains("Voir ma copie"));
        assert!(!html.contains("Issue de la bibliothèque"));
        assert!(html.contains(
            r#"<ul class="spaced"><li><strong>Pommes de terre</strong> — 1 kg</li></ul>"#
        ));
        // Its own steps, aired as its ingredients (#911).
        assert!(html.contains(
            r#"<h2>Étapes</h2><ol class="spaced"><li>Préchauffer.</li><li>Cuire.</li></ol>"#
        ));
        assert!(!html.contains("Source :"), "typed by hand");
    }

    #[test]
    fn a_publication_names_the_site_it_was_imported_from() {
        let mut p = publication();
        p.source_url = Some("https://www.marmiton.org/recettes/gratin.aspx".into());
        let html = detail_body(&p, None, None, None, true);
        assert!(html.contains(
            r#"<p class="muted">Source : <a href="https://www.marmiton.org/recettes/gratin.aspx" rel="noopener noreferrer nofollow">marmiton.org</a></p>"#
        ));
        // Not a web page: no link.
        p.source_url = Some("ftp://e.fr/gratin".into());
        assert!(!detail_body(&p, None, None, None, true).contains("Source :"));
    }

    #[test]
    fn a_publication_ends_with_the_way_to_report_it_or_the_report_made() {
        let id = Uuid::from_u128(5);
        let html = detail_body(&publication(), None, None, None, true);
        assert!(html.ends_with(&report_foot(id, None)));
        assert!(html.contains(&format!(
            r#"<a class="btn secondary" href="/recipes/library/{id}/report">Signaler cette recette</a>"#
        )));
        let mut reported = publication();
        reported.reported_at = Some("2026-11-20T10:00:00Z".parse().unwrap());
        let html = detail_body(&reported, None, None, None, true);
        assert!(html.contains("Vous avez signalé cette recette le 20 novembre 2026."));
        assert!(!html.contains("/report"));
        let thanked = detail_body(&reported, None, Some("reported"), None, true);
        assert!(thanked.contains(
            r#"<p class="notice success" role="status">Merci, votre signalement a été transmis. Nous l'examinerons.</p>"#
        ));
    }

    #[test]
    fn the_list_leads_to_my_publications() {
        let html = list_body("", "", true);
        assert!(html.contains(
            r#"<a class="btn secondary" href="/recipes/library/mine">Mes publications</a>"#
        ));
    }

    #[test]
    fn a_publication_copied_shows_the_copy_instead_of_adding_it_again() {
        let copy = Uuid::from_u128(42);
        let html = detail_body(&publication(), Some(copy), None, None, true);
        assert!(!html.contains("Ajouter à mes recettes"));
        assert!(!html.contains("<form"));
        assert!(html.contains(&format!(
            r#"<a class="btn secondary" href="/recipes/{copy}">Voir ma copie</a>"#
        )));
        assert!(html.contains(&format!("Pour 4 personnes · Temps : 1 h 20 {BADGE}")));
        let added = detail_body(&publication(), Some(copy), Some("added"), None, true);
        assert!(added.contains(&format!(
            r#"<p class="notice success" role="status">Recette ajoutée à vos recettes. <a href="/recipes/{copy}">Ouvrir ma copie</a></p>"#
        )));
        let refused = detail_body(&publication(), None, None, Some("too_many"), true);
        assert!(refused.contains(r#"<p class="notice error" role="alert">Trop de consultations"#));
    }

    #[test]
    fn a_publication_keeps_the_kitchen_segment() {
        let html = detail_body(&publication(), None, None, None, true);
        assert!(html.starts_with(&kitchen_segment("/recipes")));
    }

    #[test]
    fn a_refusal_names_the_api_code_or_too_many_for_a_429() {
        let body = serde_json::json!({ "error": "invalid_cursor" });
        assert_eq!(
            refusal_code(reqwest::StatusCode::TOO_MANY_REQUESTS, &body),
            "too_many"
        );
        assert_eq!(
            refusal_code(reqwest::StatusCode::BAD_REQUEST, &body),
            "invalid_cursor"
        );
        assert_eq!(
            refusal_code(
                reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                &serde_json::Value::Null
            ),
            "unavailable"
        );
    }

    #[test]
    fn a_withdrawn_publication_says_so_and_offers_nothing_to_add() {
        let html = withdrawn_body(true);
        assert!(html.starts_with(&kitchen_segment("/recipes")));
        assert!(html.contains("retirée de la bibliothèque"));
        assert!(!html.contains("<form"));
        assert!(html.contains(r#"href="/recipes/library""#));
    }

    // -- without a family (#990) ----------------------------------------------

    #[test]
    fn without_a_family_the_segment_leaves_out_the_recipes_that_redirect() {
        assert_eq!(library_segment(true), kitchen_segment("/recipes"));
        assert_eq!(
            library_segment(false),
            r#"<nav class="segment" aria-label="Cuisine"><a href="/stocks">Stocks</a></nav>"#
        );
    }

    #[test]
    fn without_a_family_the_list_leads_nowhere_that_redirects() {
        let html = list_body("", "", false);
        assert!(html.starts_with(&library_segment(false)));
        assert!(html.contains(
            r#"<a class="btn secondary" href="/recipes/library/mine">Mes publications</a>"#
        ));
        assert!(!html.contains(r#"href="/recipes""#), "{html}");
        assert!(!html.contains("Mes recettes"));
        assert!(list_body("", "", true).contains(r#"href="/recipes">Mes recettes</a>"#));

        let none = HashMap::new();
        let empty = results_html(&page(0, 0, None), "", &[], &none, false);
        assert!(empty.contains("La bibliothèque est vide pour le moment"));
        assert!(empty.contains("Aucune recette n'a encore été partagée. Revenez plus tard."));
        assert!(!empty.contains(r#"href="/recipes""#), "{empty}");
        assert!(!empty.contains("Mes recettes"));
        let with = results_html(&page(0, 0, None), "", &[], &none, true);
        assert!(with.contains(r#"<a class="btn secondary" href="/recipes">Mes recettes</a>"#));
        // A search, and the rows, are the same either way.
        assert_eq!(
            results_html(&page(0, 0, None), "tatin", &[], &none, false),
            results_html(&page(0, 0, None), "tatin", &[], &none, true)
        );
        assert_eq!(
            results_html(&page(3, 3, None), "", &[], &none, false),
            results_html(&page(3, 3, None), "", &[], &none, true)
        );
    }

    #[test]
    fn without_a_family_a_publication_invites_to_make_or_join_a_group() {
        let html = detail_body(&publication(), None, None, None, false);
        assert!(html.starts_with(&library_segment(false)));
        assert!(html.contains("<h1>Gratin dauphinois</h1>"));
        assert!(!html.contains("<form"), "nothing to post without a family");
        assert!(!html.contains("Ajouter à mes recettes"));
        assert!(html.contains(
            r#"<div class="card"><p><strong>Ajouter cette recette à un groupe</strong></p>"#
        ));
        assert!(html.contains("rejoignez celui de vos proches avec leur invitation"));
        assert!(
            html.contains(r#"<a class="btn" href="/groups/new">Ajouter à mon groupe</a></div>"#)
        );
        assert_eq!(html.matches("/groups/new").count(), 1, "one link only");
        assert!(!html.contains("foyer"), "the app's words: groupe, famille");
        assert!(!html.contains(r#"href="/recipes""#));
        assert!(html.ends_with(&report_foot(Uuid::from_u128(5), None)));
        // With a family, the same publication is added as before.
        let with = detail_body(&publication(), None, None, None, true);
        assert!(with.contains("Ajouter à mes recettes"));
        assert!(!with.contains("/groups/new"));
    }

    #[test]
    fn without_a_family_a_withdrawn_publication_drops_the_recipes_tab() {
        let html = withdrawn_body(false);
        assert!(html.starts_with(&library_segment(false)));
        assert!(html.contains("retirée de la bibliothèque"));
        assert!(!html.contains(r#"href="/recipes""#));
    }
}
