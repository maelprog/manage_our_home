//! « Mes publications » (#911, API #947): `/recipes/library/mine`, the
//! recipes the reader published to the library, the ones whose recipe is
//! gone included — the only place they are withdrawn from once their
//! family is (user's decision of 2026-11-20). Reached from the library's
//! list and from the « Bibliothèque » card of a recipe.
//!
//! - `GET /recipes/library/mine` — `GET /library/recipes/mine`: one card per
//!   publication, its dates, « Voir la fiche » (the library's page, never
//!   the family's recipe) and « Retirer ».
//! - `POST /recipes/library/:id/withdraw` — one click, no confirmation
//!   (arbitrage of 2026-11-20): `DELETE /library/recipes/:id`, then back
//!   to the list (PRG), `?notice=unpublished` or `?error=unpublish_failed`.
//!   The URL carries no name, which anyone could forge into the banner: a
//!   failure carries the publication's id, and the page names it from the
//!   API's list — a publication of the reader's own, or no name at all.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use manage_our_home_shared::dto::recipes::{MyPublication, MyPublications};
use uuid::Uuid;

use crate::app::{html_escape, kitchen_segment, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::lenient_query::LenientQuery;
use crate::state::{api_request_auth, AppState};

use super::publish::dates_text;
use super::recipes_cookie;
use crate::routes::groups::header_with_groups;

const LIST: &str = "/recipes/library/mine";

/// The longest name a notice repeats: a publication's (0066).
const NAME_MAX: usize = 200;

#[derive(serde::Deserialize, Default)]
pub struct MineQuery {
    #[serde(default)]
    notice: Option<String>,
    #[serde(default)]
    error: Option<String>,
    /// The publication a failed withdrawal was about.
    #[serde(default)]
    id: Option<String>,
}

/// Where « Retirer » leads, by the API's answer: the list, saying it went
/// — gone already (404) is gone too — or that publication `id` did not.
pub(crate) fn withdraw_target(status: Option<u16>, id: Uuid) -> String {
    match status {
        Some(204 | 404) => format!("{LIST}?notice=unpublished"),
        _ => format!("{LIST}?error=unpublish_failed&id={id}"),
    }
}

/// The name of the publication `id` of the URL, read from `list`, the
/// API's: a forged id names nothing the reader did not publish.
pub(crate) fn named<'a>(list: Option<&'a [MyPublication]>, id: Option<&str>) -> Option<&'a str> {
    let id: Uuid = id?.parse().ok()?;
    list?.iter().find(|p| p.id == id).map(|p| p.name.as_str())
}

/// The banner of the URL's codes, naming the recipe when `name`, read from
/// the API, says which.
pub(crate) fn banner(notice: Option<&str>, error: Option<&str>, name: Option<&str>) -> String {
    let name: Option<String> = name
        .map(|n| n.trim().chars().take(NAME_MAX).collect())
        .filter(|n: &String| !n.is_empty());
    let quoted = |fallback: &str| {
        name.as_deref()
            .map(|n| format!("« {} »", html_escape(n)))
            .unwrap_or_else(|| fallback.to_string())
    };
    match (notice, error) {
        (_, Some("unpublish_failed")) => format!(
            r#"<p class="notice error" role="alert">Le retrait n'a pas pu être enregistré. {} est toujours dans la bibliothèque. Réessayez dans un instant.</p>"#,
            quoted("La recette")
        ),
        (Some("unpublished"), _) => format!(
            r#"<p class="notice success" role="status">{} retirée de la bibliothèque. Les foyers qui l'avaient déjà copiée la gardent.</p>"#,
            quoted("Recette")
        ),
        _ => String::new(),
    }
}

fn card(p: &MyPublication) -> String {
    let id = p.id;
    let name = html_escape(&p.name);
    let orphan = if p.source_recipe_kept {
        ""
    } else {
        r#"<p class="muted">Cette recette n'est plus dans vos recettes : elle a été supprimée, ou son foyer l'a été. Elle reste visible dans la bibliothèque tant que vous ne la retirez pas.</p>"#
    };
    format!(
        r#"<li class="card"><h2>{name}</h2>
<p class="muted"><span class="badge ok">Publiée</span> {dates}</p>
{orphan}<div class="actions"><a class="btn secondary" href="/recipes/library/{id}" aria-label="Voir la fiche de {name} dans la bibliothèque">Voir la fiche</a>
<form method="post" action="/recipes/library/{id}/withdraw"><button type="submit" class="secondary danger" aria-label="Retirer {name} de la bibliothèque" data-busy-label="Retrait…">Retirer</button></form></div></li>"#,
        dates = html_escape(&dates_text(p.published_at, p.updated_at)),
    )
}

/// The page: `list` the reader's publications, `None` when the API did not
/// answer them; `banner` the one of the URL.
pub(crate) fn mine_body(list: Option<&[MyPublication]>, banner: &str) -> String {
    let content = match list {
        None => format!(
            r#"<p class="notice error" role="alert">Vos publications n'ont pas pu être chargées. <a href="{LIST}">Réessayer</a></p>"#
        ),
        Some([]) => r#"<div class="card"><h2>Aucune publication</h2>
<p>Vous n'avez publié aucune recette dans la bibliothèque. Ouvrez une de vos recettes et choisissez « Publier dans la bibliothèque » pour la partager avec les autres foyers.</p>
<div class="actions"><a class="btn" href="/recipes">Voir mes recettes</a></div></div>"#
            .to_string(),
        Some(list) => format!(
            r#"<p class="muted">Les recettes que vous avez publiées, visibles par tous les foyers. « Retirer » les enlève de la bibliothèque ; les foyers qui les ont déjà copiées les gardent.</p>
<ul class="pubs" aria-label="Recettes publiées">{}</ul>"#,
            list.iter().map(card).collect::<String>()
        ),
    };
    format!(
        r#"{segment}<div class="links"><a href="/recipes/library"><span aria-hidden="true">‹</span> Retour à la bibliothèque</a></div>
<h1>Mes publications</h1>
{banner}
{content}
<div class="links"><a href="/recipes/library">Retour à la bibliothèque</a></div>"#,
        segment = kitchen_segment("/recipes"),
    )
}

// -- handlers -----------------------------------------------------------------

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    LenientQuery(query): LenientQuery<MineQuery>,
) -> Response {
    // No family needed: a publication outlives the family it came from,
    // and this is where it is withdrawn from (#947) — a reader without
    // one still gets the page, under the header of no family.
    let (_, header) = header_with_groups(&state, &headers, &me, LIST).await;
    let cookie = recipes_cookie(&headers);
    let list = match api_request_auth(
        &state,
        reqwest::Method::GET,
        "/library/recipes/mine",
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<MyPublications>(resp.body)
                .ok()
                .map(|l| l.recipes)
        }
        Ok(_) | Err(_) => None,
    };
    let status = if list.is_some() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let banner = banner(
        query.notice.as_deref(),
        query.error.as_deref(),
        named(list.as_deref(), query.id.as_deref()),
    );
    (
        status,
        Html(shell_with_header(
            Width::Read,
            "Mes publications",
            &header,
            &mine_body(list.as_deref(), &banner),
        )),
    )
        .into_response()
}

/// No family needed either: the API withdraws for the publisher alone.
pub async fn withdraw(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let cookie = recipes_cookie(&headers);
    let status = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &format!("/library/recipes/{id}"),
        cookie.as_deref(),
        None,
    )
    .await
    .ok()
    .map(|resp| resp.status.as_u16());
    Redirect::to(&withdraw_target(status, id)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publication(n: u128, name: &str, kept: bool, updated: &str) -> MyPublication {
        MyPublication {
            id: Uuid::from_u128(n),
            name: name.into(),
            published_at: "2026-11-02T10:00:00Z".parse().unwrap(),
            updated_at: updated.parse().unwrap(),
            source_recipe_kept: kept,
        }
    }

    #[test]
    fn a_withdrawal_comes_back_to_the_list_with_no_name_in_the_url() {
        let id = Uuid::from_u128(7);
        for status in [Some(204), Some(404)] {
            assert_eq!(
                withdraw_target(status, id),
                "/recipes/library/mine?notice=unpublished",
                "{status:?}"
            );
        }
        for status in [Some(500), Some(429), None] {
            assert_eq!(
                withdraw_target(status, id),
                format!("/recipes/library/mine?error=unpublish_failed&id={id}"),
                "{status:?}"
            );
        }
    }

    #[test]
    fn the_name_comes_from_the_api_list_never_from_the_url() {
        let list = [publication(1, "Gratin", true, "2026-11-02T10:00:00Z")];
        let id = Uuid::from_u128(1).to_string();
        assert_eq!(named(Some(&list), Some(&id)), Some("Gratin"));
        // An id the reader did not publish, garbage, nothing: no name.
        let foreign = Uuid::from_u128(2).to_string();
        assert_eq!(named(Some(&list), Some(&foreign)), None);
        assert_eq!(named(Some(&list), Some("<b>Arnaque</b>")), None);
        assert_eq!(named(Some(&list), None), None);
        assert_eq!(named(None, Some(&id)), None);
        // So a forged failure banner says « La recette », nothing more.
        let banner = banner(
            None,
            Some("unpublish_failed"),
            named(Some(&list), Some(&foreign)),
        );
        assert!(banner.contains("La recette est toujours dans la bibliothèque"));
    }

    #[test]
    fn the_banner_names_the_recipe_and_escapes_it() {
        assert_eq!(
            banner(Some("unpublished"), None, Some("Soupe")),
            r#"<p class="notice success" role="status">« Soupe » retirée de la bibliothèque. Les foyers qui l'avaient déjà copiée la gardent.</p>"#
        );
        assert!(banner(Some("unpublished"), None, None)
            .contains(">Recette retirée de la bibliothèque."));
        let failed = banner(None, Some("unpublish_failed"), Some("<b>"));
        assert!(failed.contains(r#"role="alert""#));
        assert!(failed.contains("« &lt;b&gt; » est toujours dans la bibliothèque"));
        assert_eq!(banner(Some("other"), None, Some("x")), "");
        assert_eq!(banner(None, None, None), "");
    }

    #[test]
    fn each_publication_is_a_card_with_its_two_actions() {
        let list = [
            publication(1, "Gratin", true, "2026-11-20T10:00:00Z"),
            publication(2, "Soupe", false, "2026-11-02T10:00:00Z"),
        ];
        let html = mine_body(Some(&list), "");
        assert!(html.contains("<h1>Mes publications</h1>"));
        assert_eq!(html.matches(r#"<li class="card">"#).count(), 2);
        let cards: Vec<&str> = html.split(r#"<li class="card">"#).skip(1).collect();
        assert!(cards[0].contains("le 2 novembre 2026 · mise à jour le 20 novembre 2026"));
        assert!(!cards[0].contains("n'est plus dans vos recettes"));
        assert!(
            cards[1].contains(r#"<span class="badge ok">Publiée</span> le 2 novembre 2026</p>"#)
        );
        assert!(cards[1].contains("Cette recette n'est plus dans vos recettes"));
        assert!(cards[1].contains(&format!(
            r#"href="/recipes/library/{}" aria-label="Voir la fiche de Soupe dans la bibliothèque">Voir la fiche</a>"#,
            Uuid::from_u128(2)
        )));
        assert!(cards[1].contains(&format!(
            r#"<form method="post" action="/recipes/library/{}/withdraw">"#,
            Uuid::from_u128(2)
        )));
        assert!(cards[1].contains(r#"aria-label="Retirer Soupe de la bibliothèque""#));
        assert!(!html.contains(r#"name="nom""#), "no name posted back");
        assert!(!html.contains("/recipes/0"), "never the family's recipe");
    }

    #[test]
    fn an_empty_list_and_a_list_that_failed_say_different_things() {
        let empty = mine_body(Some(&[]), "");
        assert!(empty.contains("<h2>Aucune publication</h2>"));
        assert!(empty.contains(r#"<a class="btn" href="/recipes">Voir mes recettes</a>"#));
        assert!(!empty.contains("pubs"));
        let failed = mine_body(None, "");
        assert!(failed.contains("Vos publications n'ont pas pu être chargées."));
        assert!(!failed.contains("Aucune publication"));
        let names = mine_body(
            Some(&[publication(1, "<i>", true, "2026-11-02T10:00:00Z")]),
            "",
        );
        assert!(names.contains("<h2>&lt;i&gt;</h2>"));
    }
}
