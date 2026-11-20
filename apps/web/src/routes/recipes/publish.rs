//! Publishing a family recipe to the library (#911, API #906): the
//! « Bibliothèque » card of the recipe's page, the screen of consent, and
//! the withdrawal.
//!
//! - `GET /recipes/:id/publish` — what becomes visible, what does not, and
//!   the box of consent. The recipe already published by the caller
//!   (`GET /groups/:gid/recipes/:id/publish`), the same screen updates the
//!   publication: the consent is asked again, and publishing replaces the
//!   snapshot (arbitrage of 2026-11-20).
//! - `POST /recipes/:id/publish` — without the box ticked, nothing is sent
//!   to the API: the screen comes back 422 saying so, without JavaScript
//!   too. Ticked, `POST /groups/:gid/recipes/:id/publish {"consent": true}`,
//!   then back to the recipe (PRG): `?notice=published` (201) or
//!   `?notice=publication_updated` (200). A refusal is shown on the screen.
//! - `POST /recipes/:id/unpublish` — one click, no confirmation (arbitrage
//!   of 2026-11-20): `DELETE /groups/:gid/recipes/:id/publish`, then back to
//!   the recipe, `?notice=unpublished` or `?error=unpublish_failed`.
//!
//! Who may: whoever may edit the recipe (`can_modify`, the API's
//! `publish_bar`); the others read why the card offers them nothing.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use chrono::{DateTime, Utc};
use manage_our_home_shared::dto::recipes::{PublicationOfRecipe, PublishRequest, RecipeResponse};
use uuid::Uuid;

use crate::app::{html_escape, kitchen_segment, shell_with_header, Width};
use crate::icons::{icon, Icon};
use crate::layout::CurrentUser;
use crate::routes::account::allergies::long_date;
use crate::state::{api_request_auth, AppState};

use super::detail::source_domain;
use super::{
    can_modify, family_context, forbidden_page, recipe_not_found_page, recipes_cookie,
    service_unavailable_page,
};

// -- pure logic ---------------------------------------------------------------

/// Whether the form's box of consent came ticked: a checkbox sends its
/// value when ticked, nothing otherwise.
pub(crate) fn consent_given(raw: Option<&str>) -> bool {
    matches!(raw, Some("on"))
}

/// What the screen does after the API's answer to the publication.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PublishOutcome {
    /// Published (201) or published again (200): back to the recipe.
    Done(String),
    /// Another member of the family published it already (409
    /// `published_by_another_member`).
    Already,
    /// Refused or failed: the screen again, with this message.
    Refused(&'static str),
    Forbidden,
    NotFound,
}

/// The message of a publication that did not go through. Nothing was
/// published, whatever the reason; the screen says so.
pub(crate) fn refusal_text(code: &str) -> &'static str {
    match code {
        "consent_required" => "Rien n'a été publié : cochez la case de consentement pour continuer.",
        "withdrawn_by_moderation" => {
            "Rien n'a été publié : cette recette a été retirée de la bibliothèque par la modération et ne peut pas y revenir pour l'instant."
        }
        "library_quota_exceeded" => {
            "Rien n'a été publié : votre foyer a déjà 50 recettes dans la bibliothèque. Retirez-en une pour publier celle-ci."
        }
        "name_too_long" | "name_required" | "instructions_too_long" | "invalid_servings"
        | "invalid_duration" | "too_many_ingredients" | "ingredient_name_required"
        | "ingredient_name_too_long" | "ingredient_unit_too_long"
        | "ingredient_quantity_must_be_non_negative" | "invalid_seasonal_month" => {
            "Rien n'a été publié : la recette dépasse les limites de la bibliothèque (nom de 200 caractères, étapes de 20 000 caractères, 100 ingrédients au plus). Modifiez-la, puis réessayez."
        }
        "too_many" => "Rien n'a été publié : trop de publications en peu de temps. Réessayez dans une minute.",
        _ => "La publication a échoué. Rien n'a été publié. Réessayez dans un instant.",
    }
}

/// The API's answer to `POST /groups/:gid/recipes/:id/publish` — its
/// status, `None` when it did not answer, and its error code — read for
/// the recipe `id`.
pub(crate) fn publish_outcome(id: Uuid, status: Option<u16>, code: &str) -> PublishOutcome {
    match (status, code) {
        (Some(201), _) => PublishOutcome::Done(format!("/recipes/{id}?notice=published")),
        (Some(200), _) => PublishOutcome::Done(format!("/recipes/{id}?notice=publication_updated")),
        (Some(409), "published_by_another_member") => PublishOutcome::Already,
        (Some(403), _) => PublishOutcome::Forbidden,
        (Some(404), _) => PublishOutcome::NotFound,
        (Some(429), _) => PublishOutcome::Refused(refusal_text("too_many")),
        (Some(400 | 409), code) => PublishOutcome::Refused(refusal_text(code)),
        _ => PublishOutcome::Refused(refusal_text("unavailable")),
    }
}

/// Where « Retirer de la bibliothèque » leads, by the API's answer: the
/// recipe, saying it went or that it did not. Nothing of the caller's to
/// withdraw (404): the recipe as it now stands.
pub(crate) fn unpublish_target(id: Uuid, status: Option<u16>) -> String {
    match status {
        Some(204) => format!("/recipes/{id}?notice=unpublished"),
        Some(404) => format!("/recipes/{id}"),
        _ => format!("/recipes/{id}?error=unpublish_failed"),
    }
}

/// « le 18 novembre 2026 », and « · mise à jour le 20 novembre 2026 » once
/// published again: the dates of a publication, Paris days.
pub(crate) fn dates_text(published_at: DateTime<Utc>, updated_at: DateTime<Utc>) -> String {
    let published = format!("le {}", long_date(published_at));
    if updated_at > published_at {
        format!("{published} · mise à jour le {}", long_date(updated_at))
    } else {
        published
    }
}

// -- rendering ----------------------------------------------------------------

/// The « Bibliothèque » card of a recipe's page (`detail.rs`), before
/// « Modifier » and « Supprimer ». `publication` is the caller's own
/// publication of it; `withdrawn` says the page follows a withdrawal.
pub(crate) fn library_card(
    id: Uuid,
    can_edit: bool,
    publication: Option<&PublicationOfRecipe>,
    withdrawn: bool,
) -> String {
    const MINE: &str =
        r#"<div class="links"><a href="/recipes/library/mine">Mes publications</a></div>"#;
    let body = if !can_edit {
        r#"<p class="muted">Seul le créateur ou un administrateur peut publier cette recette dans la bibliothèque.</p>"#.to_string()
    } else if let Some(p) = publication {
        format!(
            r#"<p><span class="badge ok">Publiée</span> <span class="muted">{dates}</span></p>
<p class="muted">Cette recette est visible par tous les foyers, dans l'état où vous l'avez publiée. Si vous l'avez modifiée depuis, « Mettre à jour la publication » remplace la version publiée. Si vous la retirez, les foyers ne pourront plus la copier ; ceux qui l'ont déjà copiée la gardent.</p>
<div class="actions"><a class="btn" href="/recipes/{id}/publish">Mettre à jour la publication</a>
<form method="post" action="/recipes/{id}/unpublish"><button type="submit" class="secondary danger" data-busy-label="Retrait…">Retirer de la bibliothèque</button></form></div>
{MINE}"#,
            dates = html_escape(&dates_text(p.published_at, p.updated_at)),
        )
    } else if withdrawn {
        format!(
            r#"<p>Cette recette n'est plus dans la bibliothèque.</p>
<div class="actions"><a class="btn secondary" href="/recipes/{id}/publish">Publier à nouveau</a></div>
{MINE}"#
        )
    } else {
        format!(
            r#"<p>Partagez cette recette avec les autres foyers de MHome. Seul son contenu devient visible : ni vous, ni votre famille.</p>
<div class="actions"><a class="btn" href="/recipes/{id}/publish">Publier dans la bibliothèque</a></div>
{MINE}"#
        )
    };
    format!(
        r#"<section class="card bib" id="bibliotheque" aria-labelledby="bibliotheque-h"><h2 id="bibliotheque-h">Bibliothèque</h2>
{body}</section>"#
    )
}

/// What the screen shows: the first publication, its update, or the
/// recipe another member published already.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Mode<'a> {
    Publish,
    Update(&'a PublicationOfRecipe),
    Already,
}

/// The screen's title, its tab's as well as its heading: the update's in
/// the update mode, the publication's otherwise.
pub(crate) fn mode_title(mode: Mode<'_>) -> &'static str {
    match mode {
        Mode::Update(_) => "Mettre à jour la publication",
        _ => "Publier dans la bibliothèque",
    }
}

/// The screen of `/recipes/:id/publish`. `consent_missing` after a form
/// sent unticked; `error` a refusal of the API.
pub(crate) fn screen_body(
    recipe: &RecipeResponse,
    mode: Mode<'_>,
    consent_missing: bool,
    error: Option<&str>,
) -> String {
    let id = recipe.id;
    let name = html_escape(&recipe.name);
    let back = format!(
        r#"<div class="links"><a href="/recipes/{id}"><span aria-hidden="true">‹</span> Retour à la recette</a></div>"#
    );
    let title = mode_title(mode);
    if matches!(mode, Mode::Already) {
        return format!(
            r#"{segment}{back}
<h1>{title}</h1>
<p class="notice warning" role="status">Cette recette est déjà publiée dans la bibliothèque par un autre membre de votre foyer. <a href="/recipes/{id}">Retour à la recette</a></p>"#,
            segment = kitchen_segment("/recipes"),
        );
    }
    let banner = if consent_missing {
        format!(
            r#"<p class="notice error" role="alert">{}</p>"#,
            refusal_text("consent_required")
        )
    } else {
        error
            .map(|e| {
                format!(
                    r#"<p class="notice error" role="alert">{}</p>"#,
                    html_escape(e)
                )
            })
            .unwrap_or_default()
    };
    let (lead, visible, again, button) = match mode {
        Mode::Update(p) => (
            format!(
                "<p><strong>{name}</strong> est publiée depuis le {}. La version actuelle de votre recette va remplacer la version publiée. Les foyers qui l'ont déjà copiée gardent leur copie, qui ne change pas.</p>",
                html_escape(&long_date(p.published_at)),
            ),
            "Ce qui sera visible après la mise à jour",
            "Je confirme à nouveau que cette recette",
            ("Mettre à jour", "Mise à jour…"),
        ),
        _ => (
            format!(
                "<p>Vous allez publier <strong>{name}</strong>. Les autres foyers pourront la lire et en ajouter une copie à leurs recettes.</p>"
            ),
            "Ce qui devient visible",
            "Je confirme que cette recette",
            ("Publier", "Publication…"),
        ),
    };
    let tick = icon(Icon::Check);
    let cross = icon(Icon::Close);
    let source = recipe
        .source_url
        .as_deref()
        .and_then(source_domain)
        .map(|domain| {
            format!(
                "<li>{tick}<span>Le lien de la page source, pour une recette importée (ici : {})</span></li>",
                html_escape(&domain)
            )
        })
        .unwrap_or_default();
    let (invalid, described, error_line) = if consent_missing {
        (
            " invalid",
            r#" aria-describedby="consent-help consent-err" aria-invalid="true""#,
            r#"<p class="field-error" id="consent-err">Cochez cette case pour publier.</p>"#,
        )
    } else {
        ("", r#" aria-describedby="consent-help""#, "")
    };
    format!(
        r#"{segment}{back}
<h1>{title}</h1>
{banner}
{lead}
<section class="card" aria-labelledby="visible-h"><h2 id="visible-h">{visible}</h2>
<ul class="ticks">
<li>{tick}<span>Le nom, les ingrédients et leurs quantités</span></li>
<li>{tick}<span>Les étapes, le nombre de personnes, le temps</span></li>
{source}</ul>
<h3>Ce qui ne l'est pas</h3>
<ul class="crosses">
<li>{cross}<span>Votre nom, votre foyer, votre photo</span></li>
<li>{cross}<span>Les jours planifiés et le journal des repas</span></li>
</ul></section>
<form method="post" action="/recipes/{id}/publish" class="publish" aria-label="Confirmer la publication">
<div class="consent{invalid}">
<label class="field inline"><input type="checkbox" name="consent" value="on"{described}/><span>{again} est de moi (ou libre de droits) et qu'elle ne contient aucune donnée personnelle d'un tiers : nom, adresse, numéro de téléphone.</span></label>
{error_line}</div>
<p class="muted" id="consent-help">Vérifiez surtout les étapes : « la tarte de Mamie Odile, 12 rue des Lilas » n'a rien à faire ici. Vous pourrez retirer la recette à tout moment.</p>
<div class="actions"><button type="submit" data-busy-label="{busy}">{label}</button>
<a class="btn secondary" href="/recipes/{id}">Annuler</a></div>
</form>"#,
        segment = kitchen_segment("/recipes"),
        label = button.0,
        busy = button.1,
    )
}

// -- handlers -----------------------------------------------------------------

/// The caller's own publication of the recipe, if any. Unknown (`None`)
/// when the API does not answer: the card then offers to publish, and the
/// API publishes again in place.
pub(crate) async fn my_publication(
    state: &AppState,
    gid: Uuid,
    id: Uuid,
    cookie: Option<&str>,
) -> Option<PublicationOfRecipe> {
    let resp = api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/recipes/{id}/publish"),
        cookie,
        None,
    )
    .await
    .ok()?;
    if resp.status != reqwest::StatusCode::OK {
        return None;
    }
    serde_json::from_value(resp.body).ok()
}

/// The recipe, or the page that says why there is none.
async fn fetch_recipe(
    state: &AppState,
    gid: Uuid,
    id: Uuid,
    cookie: Option<&str>,
) -> Result<RecipeResponse, Html<String>> {
    match api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/recipes/{id}"),
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value(resp.body).map_err(|_| service_unavailable_page())
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => Err(recipe_not_found_page()),
        _ => Err(service_unavailable_page()),
    }
}

#[derive(serde::Deserialize, Default)]
pub struct PublishForm {
    #[serde(default)]
    consent: Option<String>,
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let here = format!("/recipes/{id}/publish");
    let Some(fam) = family_context(&state, &headers, &me, &here).await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);
    let recipe = match fetch_recipe(&state, fam.gid, id, cookie.as_deref()).await {
        Ok(r) => r,
        Err(page) => return page.into_response(),
    };
    if !can_modify(
        &fam.role,
        &fam.edit_min_role,
        recipe.created_by == me.user_id,
    ) {
        return forbidden_page().into_response();
    }
    let publication = my_publication(&state, fam.gid, id, cookie.as_deref()).await;
    let mode = publication.as_ref().map_or(Mode::Publish, Mode::Update);
    let title = mode_title(mode);
    Html(shell_with_header(
        Width::Read,
        title,
        &fam.header,
        &screen_body(&recipe, mode, false, None),
    ))
    .into_response()
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Form(form): Form<PublishForm>,
) -> Response {
    let here = format!("/recipes/{id}/publish");
    let Some(fam) = family_context(&state, &headers, &me, &here).await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);
    let consent = consent_given(form.consent.as_deref());
    // Without the consent, nothing goes to the API.
    let (status, outcome) = if consent {
        let body = serde_json::to_value(PublishRequest { consent: true }).ok();
        let answer = api_request_auth(
            &state,
            reqwest::Method::POST,
            &format!("/groups/{}/recipes/{id}/publish", fam.gid),
            cookie.as_deref(),
            body,
        )
        .await;
        let outcome = match &answer {
            Ok(resp) => publish_outcome(
                id,
                Some(resp.status.as_u16()),
                resp.body
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or_default(),
            ),
            Err(_) => publish_outcome(id, None, ""),
        };
        let status = match outcome {
            PublishOutcome::Already => StatusCode::CONFLICT,
            _ => StatusCode::OK,
        };
        (status, Some(outcome))
    } else {
        (StatusCode::UNPROCESSABLE_ENTITY, None)
    };
    let error = match outcome {
        Some(PublishOutcome::Done(to)) => return Redirect::to(&to).into_response(),
        Some(PublishOutcome::Forbidden) => return forbidden_page().into_response(),
        Some(PublishOutcome::NotFound) => return recipe_not_found_page().into_response(),
        Some(PublishOutcome::Refused(text)) => Some(text),
        Some(PublishOutcome::Already) | None => None,
    };
    let already = status == StatusCode::CONFLICT;
    // The screen again, as it stands now.
    let recipe = match fetch_recipe(&state, fam.gid, id, cookie.as_deref()).await {
        Ok(r) => r,
        Err(page) => return page.into_response(),
    };
    let publication = my_publication(&state, fam.gid, id, cookie.as_deref()).await;
    let mode = if already {
        Mode::Already
    } else {
        publication.as_ref().map_or(Mode::Publish, Mode::Update)
    };
    let status = if error.is_some() && status == StatusCode::OK {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        status
    };
    (
        status,
        Html(shell_with_header(
            Width::Read,
            mode_title(mode),
            &fam.header,
            &screen_body(&recipe, mode, !consent, error),
        )),
    )
        .into_response()
}

pub async fn unpublish(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let here = format!("/recipes/{id}");
    let Some(fam) = family_context(&state, &headers, &me, &here).await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);
    let status = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &format!("/groups/{}/recipes/{id}/publish", fam.gid),
        cookie.as_deref(),
        None,
    )
    .await
    .ok()
    .map(|resp| resp.status.as_u16());
    Redirect::to(&unpublish_target(id, status)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(raw: &str) -> DateTime<Utc> {
        raw.parse().unwrap()
    }

    fn publication(published: &str, updated: &str) -> PublicationOfRecipe {
        PublicationOfRecipe {
            library_recipe_id: Uuid::from_u128(9),
            published_at: at(published),
            updated_at: at(updated),
        }
    }

    fn recipe(source: Option<&str>) -> RecipeResponse {
        serde_json::from_value(serde_json::json!({
            "id": Uuid::from_u128(7),
            "group_id": Uuid::nil(),
            "created_by": Uuid::nil(),
            "name": "Gratin <dauphinois>",
            "instructions": null,
            "ingredients": [],
            "source_url": source,
            "created_at": "2026-11-20T10:00:00Z",
            "updated_at": "2026-11-20T10:00:00Z",
        }))
        .unwrap()
    }

    #[test]
    fn only_a_ticked_box_is_a_consent() {
        assert!(consent_given(Some("on")));
        for raw in [None, Some(""), Some("off"), Some("true"), Some("ON")] {
            assert!(!consent_given(raw), "{raw:?}");
        }
    }

    #[test]
    fn a_publication_leads_back_to_the_recipe_saying_what_it_did() {
        let id = Uuid::from_u128(7);
        assert_eq!(
            publish_outcome(id, Some(201), ""),
            PublishOutcome::Done(format!("/recipes/{id}?notice=published"))
        );
        assert_eq!(
            publish_outcome(id, Some(200), ""),
            PublishOutcome::Done(format!("/recipes/{id}?notice=publication_updated"))
        );
    }

    #[test]
    fn a_refused_publication_stays_on_the_screen_with_its_reason() {
        let id = Uuid::from_u128(7);
        assert_eq!(
            publish_outcome(id, Some(409), "published_by_another_member"),
            PublishOutcome::Already
        );
        assert_eq!(
            publish_outcome(id, Some(403), "forbidden"),
            PublishOutcome::Forbidden
        );
        assert_eq!(
            publish_outcome(id, Some(404), "not_found"),
            PublishOutcome::NotFound
        );
        let refused = |status, code| match publish_outcome(id, status, code) {
            PublishOutcome::Refused(text) => text,
            other => panic!("{status:?} {code}: {other:?}"),
        };
        assert!(refused(Some(409), "withdrawn_by_moderation").contains("modération"));
        assert!(refused(Some(409), "library_quota_exceeded").contains("50 recettes"));
        assert!(refused(Some(400), "name_too_long").contains("limites de la bibliothèque"));
        assert_eq!(
            refused(Some(400), "too_many_ingredients"),
            refused(Some(400), "instructions_too_long")
        );
        assert!(refused(Some(429), "too_many_attempts").contains("une minute"));
        assert!(refused(Some(400), "consent_required").contains("cochez la case"));
        let failed = refused(Some(500), "");
        assert!(failed.contains("Rien n'a été publié"));
        assert_eq!(refused(None, ""), failed);
        assert_eq!(refused(Some(409), "anything_else"), failed);
        // Every refusal says nothing was published.
        for code in [
            "consent_required",
            "withdrawn_by_moderation",
            "library_quota_exceeded",
            "name_too_long",
            "too_many",
            "unavailable",
        ] {
            assert!(refusal_text(code).contains("Rien n'a été publié"), "{code}");
        }
    }

    #[test]
    fn a_withdrawal_says_whether_it_went() {
        let id = Uuid::from_u128(7);
        assert_eq!(
            unpublish_target(id, Some(204)),
            format!("/recipes/{id}?notice=unpublished")
        );
        assert_eq!(unpublish_target(id, Some(404)), format!("/recipes/{id}"));
        for status in [Some(500), Some(403), None] {
            assert_eq!(
                unpublish_target(id, status),
                format!("/recipes/{id}?error=unpublish_failed"),
                "{status:?}"
            );
        }
    }

    #[test]
    fn the_dates_say_the_update_only_once_published_again() {
        let first = publication("2026-11-18T09:00:00Z", "2026-11-18T09:00:00Z");
        assert_eq!(
            dates_text(first.published_at, first.updated_at),
            "le 18 novembre 2026"
        );
        let again = publication("2026-11-18T09:00:00Z", "2026-11-20T23:30:00Z");
        // Paris time: the 21st.
        assert_eq!(
            dates_text(again.published_at, again.updated_at),
            "le 18 novembre 2026 · mise à jour le 21 novembre 2026"
        );
    }

    #[test]
    fn the_card_offers_what_the_reader_may_do() {
        let id = Uuid::from_u128(7);
        let none = library_card(id, true, None, false);
        assert!(none.contains(&format!(
            r#"<a class="btn" href="/recipes/{id}/publish">Publier dans la bibliothèque</a>"#
        )));
        assert!(none.contains(r#"href="/recipes/library/mine">Mes publications</a>"#));
        assert!(!none.contains("<form"));

        let p = publication("2026-11-18T09:00:00Z", "2026-11-18T09:00:00Z");
        let published = library_card(id, true, Some(&p), false);
        assert!(published.contains(r#"<span class="badge ok">Publiée</span> <span class="muted">le 18 novembre 2026</span>"#));
        assert!(published.contains("Mettre à jour la publication"));
        assert!(published.contains(&format!(
            r#"<form method="post" action="/recipes/{id}/unpublish">"#
        )));
        assert!(published.contains("Retirer de la bibliothèque"));

        let gone = library_card(id, true, None, true);
        assert!(gone.contains("Cette recette n'est plus dans la bibliothèque."));
        assert!(gone.contains("Publier à nouveau"));

        let reader = library_card(id, false, Some(&p), false);
        assert!(reader.contains("Seul le créateur ou un administrateur peut publier"));
        assert!(!reader.contains("<form"));
        assert!(!reader.contains("/publish"));
        for html in [&none, &published, &gone, &reader] {
            assert!(html.starts_with(r#"<section class="card bib" id="bibliotheque""#));
        }
    }

    #[test]
    fn the_screen_says_what_becomes_visible_and_asks_the_consent() {
        let html = screen_body(
            &recipe(Some("https://www.marmiton.org/r.aspx")),
            Mode::Publish,
            false,
            None,
        );
        assert!(html.contains("<h1>Publier dans la bibliothèque</h1>"));
        assert!(html.contains("Vous allez publier <strong>Gratin &lt;dauphinois&gt;</strong>."));
        assert!(html.contains("Ce qui devient visible"));
        assert!(html.contains("(ici : marmiton.org)"));
        assert!(html.contains("Votre nom, votre foyer, votre photo"));
        assert!(html.contains(
            r#"<input type="checkbox" name="consent" value="on" aria-describedby="consent-help"/>"#
        ));
        assert!(
            !html.contains(" required"),
            "the server checks it, without JavaScript too"
        );
        assert!(!html.contains("notice error"));
        assert!(html
            .contains(r#"<button type="submit" data-busy-label="Publication…">Publier</button>"#));
        // A recipe typed by hand has no page to show.
        let typed = screen_body(&recipe(None), Mode::Publish, false, None);
        assert!(!typed.contains("page source"));
    }

    #[test]
    fn an_unticked_box_is_said_twice() {
        let html = screen_body(&recipe(None), Mode::Publish, true, None);
        assert!(html.contains(r#"<p class="notice error" role="alert">Rien n'a été publié : cochez la case de consentement pour continuer.</p>"#));
        assert!(html.contains(r#"<div class="consent invalid">"#));
        assert!(html.contains(r#"aria-describedby="consent-help consent-err" aria-invalid="true""#));
        assert!(html.contains(
            r#"<p class="field-error" id="consent-err">Cochez cette case pour publier.</p>"#
        ));
    }

    #[test]
    fn the_update_asks_the_consent_again() {
        let p = publication("2026-11-18T09:00:00Z", "2026-11-18T09:00:00Z");
        let html = screen_body(&recipe(None), Mode::Update(&p), false, None);
        assert!(html.contains("<h1>Mettre à jour la publication</h1>"));
        assert!(html.contains("est publiée depuis le 18 novembre 2026."));
        assert!(html.contains("Ce qui sera visible après la mise à jour"));
        assert!(html.contains("Je confirme à nouveau que cette recette"));
        assert!(html.contains(">Mettre à jour</button>"));
    }

    #[test]
    fn a_recipe_another_member_published_offers_no_form() {
        let html = screen_body(&recipe(None), Mode::Already, false, None);
        assert!(html.contains("déjà publiée dans la bibliothèque par un autre membre"));
        assert!(!html.contains("<form"));
        let refused = screen_body(&recipe(None), Mode::Publish, false, Some("Échec <b>"));
        assert!(refused.contains(r#"<p class="notice error" role="alert">Échec &lt;b&gt;</p>"#));
    }

    #[test]
    fn the_tab_is_titled_by_the_mode() {
        let p = publication("2026-11-02T10:00:00Z", "2026-11-02T10:00:00Z");
        assert_eq!(mode_title(Mode::Update(&p)), "Mettre à jour la publication");
        assert_eq!(mode_title(Mode::Publish), "Publier dans la bibliothèque");
        assert_eq!(mode_title(Mode::Already), "Publier dans la bibliothèque");
    }
}
