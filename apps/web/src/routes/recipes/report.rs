//! Reporting a publication of the library (#911, API #908):
//! `/recipes/library/:id/report`.
//!
//! - `GET` — the reasons as radios, in the order of the arbitrage of
//!   2026-11-20, each sent as one of the API's six (`library_reports.reason`,
//!   0068, unchanged): « Inapproprié ou choquant » `hateful`, « Illégal (dont
//!   données personnelles d'un tiers) » `illegal`, « Erreur ou danger »
//!   `health_hazard`, « Droits d'auteur » `copyright`, « Spam » `spam`,
//!   « Autre » `other`, whose comment is required here. A comment of 500
//!   characters at most. Already reported by the reader: back to the
//!   publication, which says so.
//! - `POST` — checked here first (a reason, a comment for « Autre », 500
//!   characters): a mistake comes back 422 with the entry kept and said
//!   twice, at the top and at the field. Then
//!   `POST /library/recipes/:id/report`, and back to the publication
//!   (PRG) with `?notice=reported`; a refusal of the API keeps the entry.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::recipes::{LibraryRecipe, ReportRequest};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::library::{error_html, library_segment, refusal_code, withdrawn_body};
use super::{reader_context, recipes_cookie};

/// The longest comment, in characters once trimmed: the API's
/// `COMMENT_MAX_CHARS`.
pub(crate) const COMMENT_MAX: usize = 500;

/// The reasons, in the order the screen lists them: the API's value and
/// the words the reader picks (arbitrage of 2026-11-20).
pub(crate) const REASONS: [(&str, &str); 6] = [
    ("hateful", "Inapproprié ou choquant"),
    ("illegal", "Illégal (dont données personnelles d'un tiers)"),
    ("health_hazard", "Erreur ou danger"),
    ("copyright", "Droits d'auteur"),
    ("spam", "Spam"),
    ("other", "Autre (commentaire obligatoire)"),
];

/// What the form got wrong, field by field.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ReportErrors {
    pub reason: Option<&'static str>,
    pub comment: Option<&'static str>,
}

/// The report the form makes: a reason of [`REASONS`], and the comment
/// trimmed, absent when blank — required for « Autre », of
/// [`COMMENT_MAX`] characters at most.
pub(crate) fn parse_report(
    reason: Option<&str>,
    comment: &str,
) -> Result<ReportRequest, ReportErrors> {
    let reason = reason.filter(|r| REASONS.iter().any(|(value, _)| value == r));
    let comment = comment.trim();
    let mut errors = ReportErrors::default();
    if reason.is_none() {
        errors.reason = Some("Choisissez un motif.");
    }
    if comment.chars().count() > COMMENT_MAX {
        errors.comment = Some("Le commentaire dépasse 500 caractères.");
    } else if comment.is_empty() && reason == Some("other") {
        errors.comment = Some("Précisez le problème dans le commentaire.");
    }
    match reason {
        Some(reason) if errors == ReportErrors::default() => Ok(ReportRequest {
            reason: reason.to_string(),
            comment: (!comment.is_empty()).then(|| comment.to_string()),
        }),
        _ => Err(errors),
    }
}

/// What the screen does after the API's answer to the report.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReportOutcome {
    /// Sent (204, a second report answered the same): back to the
    /// publication.
    Done(String),
    /// The publication is gone (404).
    Gone,
    /// Not sent: the screen again, the entry kept, with this message.
    Failed(&'static str),
}

pub(crate) fn report_outcome(id: Uuid, status: Option<u16>) -> ReportOutcome {
    match status {
        Some(204) => ReportOutcome::Done(format!("/recipes/library/{id}?notice=reported")),
        Some(404) => ReportOutcome::Gone,
        Some(429) => ReportOutcome::Failed(
            "Trop de signalements en peu de temps : le vôtre n'a pas été envoyé. Réessayez dans une heure ; votre saisie est conservée.",
        ),
        _ => ReportOutcome::Failed(
            "Le signalement n'a pas pu être envoyé. Réessayez dans un instant ; votre saisie est conservée.",
        ),
    }
}

/// The screen of a report of the publication `id` named `name`: the
/// reason ticked and the comment typed, kept after a refusal; `errors` of
/// the form, `failure` of the API.
pub(crate) fn report_body(
    id: Uuid,
    name: &str,
    reason: Option<&str>,
    comment: &str,
    errors: &ReportErrors,
    failure: Option<&str>,
    family: bool,
) -> String {
    let banner = if let Some(text) = failure {
        format!(
            r#"<p class="notice error" role="alert">{}</p>"#,
            html_escape(text)
        )
    } else if errors.reason.is_some() || errors.comment.is_some() {
        r#"<p class="notice error" role="alert">Le signalement n'a pas été envoyé : corrigez le champ indiqué ci-dessous.</p>"#.to_string()
    } else {
        String::new()
    };
    let radios: String = REASONS
        .iter()
        .map(|(value, label)| {
            let checked = if reason == Some(*value) { " checked" } else { "" };
            format!(
                r#"<label class="field inline"><input type="radio" name="reason" value="{value}"{checked}/><span>{label}</span></label>"#
            )
        })
        .collect();
    let (group_described, reason_error) = match errors.reason {
        Some(text) => (
            r#" aria-describedby="motif-err""#,
            format!(r#"<p class="field-error" id="motif-err">{text}</p>"#),
        ),
        None => ("", String::new()),
    };
    let (comment_attrs, comment_error) = match errors.comment {
        Some(text) => (
            r#" aria-describedby="c-help c-err" aria-invalid="true""#,
            format!(r#"<p class="field-error" id="c-err">{text}</p>"#),
        ),
        None => (r#" aria-describedby="c-help""#, String::new()),
    };
    format!(
        r#"{segment}<div class="links"><a href="/recipes/library/{id}"><span aria-hidden="true">‹</span> Retour à la recette</a></div>
<h1>Signaler une recette</h1>
{banner}
<p>Recette : <strong>{name}</strong>. Votre signalement est lu par l'équipe de MHome. L'auteur n'en est pas informé.</p>
<form method="post" action="/recipes/library/{id}/report" aria-label="Signalement">
<div class="card motifs" role="radiogroup" aria-labelledby="motif-l"{group_described}>
<h2 id="motif-l">Motif</h2>
{radios}
{reason_error}</div>
<label for="comment">Commentaire <span class="muted" id="c-help">(facultatif, obligatoire pour « Autre » · {COMMENT_MAX} caractères maximum)</span></label>
<textarea id="comment" name="comment" maxlength="{COMMENT_MAX}" rows="4"{comment_attrs} placeholder="Par exemple : l'étape 3 donne l'adresse d'une personne.">{comment}</textarea>
{comment_error}
<div class="actions"><button type="submit" data-busy-label="Envoi…">Envoyer le signalement</button>
<a class="btn secondary" href="/recipes/library/{id}">Annuler</a></div>
</form>"#,
        segment = library_segment(family),
        name = html_escape(name),
        comment = html_escape(comment),
    )
}

// -- handlers -----------------------------------------------------------------

#[derive(serde::Deserialize, Default)]
pub struct ReportForm {
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    comment: String,
}

/// The page that says why there is no publication to report: its status,
/// title and body.
type Refusal = (StatusCode, &'static str, String);

/// The publication, or why there is none; `family`, whether the reader
/// has one (#990), for the segment of a withdrawn one.
async fn fetch(
    state: &AppState,
    id: Uuid,
    cookie: Option<&str>,
    family: bool,
) -> Result<LibraryRecipe, Refusal> {
    let here = format!("/recipes/library/{id}/report");
    let unavailable = || {
        (
            StatusCode::OK,
            "Bibliothèque",
            error_html("unavailable", &here),
        )
    };
    match api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/library/recipes/{id}"),
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value(resp.body).map_err(|_| unavailable())
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => Err((
            StatusCode::NOT_FOUND,
            "Recette retirée",
            withdrawn_body(family),
        )),
        Ok(resp) => Err((
            StatusCode::OK,
            "Bibliothèque",
            error_html(&refusal_code(resp.status, &resp.body), &here),
        )),
        Err(_) => Err(unavailable()),
    }
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let here = format!("/recipes/library/{id}/report");
    // Reported without a family too (#990): the API asks a session only.
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
    let recipe = match fetch(&state, id, cookie.as_deref(), family).await {
        Ok(r) => r,
        Err((status, title, body)) => return page(status, title, &body),
    };
    if recipe.reported_at.is_some() {
        return Redirect::to(&format!("/recipes/library/{id}")).into_response();
    }
    page(
        StatusCode::OK,
        "Signaler une recette",
        &report_body(
            id,
            &recipe.name,
            None,
            "",
            &ReportErrors::default(),
            None,
            family,
        ),
    )
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Form(form): Form<ReportForm>,
) -> Response {
    let here = format!("/recipes/library/{id}/report");
    // Reported without a family too (#990): the API asks a session only.
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
    let (errors, failure) = match parse_report(form.reason.as_deref(), &form.comment) {
        Err(errors) => (errors, None),
        Ok(request) => {
            let status = api_request_auth(
                &state,
                reqwest::Method::POST,
                &format!("/library/recipes/{id}/report"),
                cookie.as_deref(),
                serde_json::to_value(&request).ok(),
            )
            .await
            .ok()
            .map(|resp| resp.status.as_u16());
            match report_outcome(id, status) {
                ReportOutcome::Done(to) => return Redirect::to(&to).into_response(),
                ReportOutcome::Gone => {
                    return page(
                        StatusCode::NOT_FOUND,
                        "Recette retirée",
                        &withdrawn_body(family),
                    )
                }
                ReportOutcome::Failed(text) => (ReportErrors::default(), Some(text)),
            }
        }
    };
    let recipe = match fetch(&state, id, cookie.as_deref(), family).await {
        Ok(r) => r,
        Err((status, title, body)) => return page(status, title, &body),
    };
    let status = if failure.is_some() {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    page(
        status,
        "Signaler une recette",
        &report_body(
            id,
            &recipe.name,
            form.reason.as_deref(),
            &form.comment,
            &errors,
            failure,
            family,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reasons_are_the_six_of_the_api_in_the_arbitrated_order() {
        let values: Vec<&str> = REASONS.iter().map(|(v, _)| *v).collect();
        assert_eq!(
            values,
            [
                "hateful",
                "illegal",
                "health_hazard",
                "copyright",
                "spam",
                "other"
            ]
        );
        let labels: Vec<&str> = REASONS.iter().map(|(_, l)| *l).collect();
        assert_eq!(
            labels,
            [
                "Inapproprié ou choquant",
                "Illégal (dont données personnelles d'un tiers)",
                "Erreur ou danger",
                "Droits d'auteur",
                "Spam",
                "Autre (commentaire obligatoire)",
            ]
        );
    }

    #[test]
    fn a_report_takes_a_reason_and_an_optional_trimmed_comment() {
        let ok = parse_report(Some("spam"), "  ").unwrap();
        assert_eq!((ok.reason.as_str(), ok.comment), ("spam", None));
        let ok = parse_report(Some("illegal"), "  L'étape 3 donne une adresse. ").unwrap();
        assert_eq!(ok.reason, "illegal");
        assert_eq!(ok.comment.as_deref(), Some("L'étape 3 donne une adresse."));
        let ok = parse_report(Some("other"), "Recette en double").unwrap();
        assert_eq!(ok.comment.as_deref(), Some("Recette en double"));
        let at_limit = "é".repeat(500);
        assert_eq!(
            parse_report(Some("copyright"), &at_limit).unwrap().comment,
            Some(at_limit.clone()),
            "500 characters, not bytes"
        );
    }

    #[test]
    fn a_report_without_reason_or_past_the_bounds_is_refused_field_by_field() {
        for reason in [
            None,
            Some(""),
            Some("personal_data"),
            Some("Spam"),
            Some("dangerous"),
        ] {
            let errors = parse_report(reason, "x").unwrap_err();
            assert_eq!(errors.reason, Some("Choisissez un motif."), "{reason:?}");
            assert_eq!(errors.comment, None);
        }
        let errors = parse_report(Some("other"), " \n ").unwrap_err();
        assert_eq!(
            errors,
            ReportErrors {
                reason: None,
                comment: Some("Précisez le problème dans le commentaire.")
            }
        );
        let errors = parse_report(Some("spam"), &"a".repeat(501)).unwrap_err();
        assert_eq!(
            errors.comment,
            Some("Le commentaire dépasse 500 caractères.")
        );
        // Both at once.
        let errors = parse_report(None, &"a".repeat(501)).unwrap_err();
        assert!(errors.reason.is_some() && errors.comment.is_some());
    }

    #[test]
    fn a_report_sent_leads_back_to_the_publication() {
        let id = Uuid::from_u128(5);
        assert_eq!(
            report_outcome(id, Some(204)),
            ReportOutcome::Done(format!("/recipes/library/{id}?notice=reported"))
        );
        assert_eq!(report_outcome(id, Some(404)), ReportOutcome::Gone);
        let failed = |status| match report_outcome(id, status) {
            ReportOutcome::Failed(text) => text,
            other => panic!("{status:?}: {other:?}"),
        };
        assert!(failed(Some(429)).contains("une heure"));
        assert!(failed(Some(500)).contains("votre saisie est conservée"));
        assert_eq!(failed(None), failed(Some(500)));
        assert_eq!(failed(Some(400)), failed(Some(500)));
    }

    #[test]
    fn the_screen_lists_the_reasons_and_keeps_the_entry() {
        let id = Uuid::from_u128(5);
        let html = report_body(id, "Gratin", None, "", &ReportErrors::default(), None, true);
        assert!(html.contains("<h1>Signaler une recette</h1>"));
        assert_eq!(
            html.matches(r#"<input type="radio" name="reason""#).count(),
            6
        );
        assert!(!html.contains("checked"));
        assert!(!html.contains("notice error"));
        assert!(html.contains(r#"maxlength="500""#));
        assert!(html.contains(&format!(r#"action="/recipes/library/{id}/report""#)));

        let errors = parse_report(Some("other"), "").unwrap_err();
        let kept = report_body(id, "Gratin", Some("other"), "<b>", &errors, None, true);
        assert!(kept.contains(r#"value="other" checked/>"#));
        assert!(kept
            .contains("Le signalement n'a pas été envoyé : corrigez le champ indiqué ci-dessous."));
        assert!(kept.contains(
            r#"<p class="field-error" id="c-err">Précisez le problème dans le commentaire.</p>"#
        ));
        assert!(kept.contains(r#"aria-describedby="c-help c-err" aria-invalid="true""#));
        assert!(kept.contains(">&lt;b&gt;</textarea>"));

        let errors = parse_report(None, "").unwrap_err();
        let no_reason = report_body(id, "Gratin", None, "", &errors, None, true);
        assert!(no_reason.contains(r#"aria-labelledby="motif-l" aria-describedby="motif-err""#));
        assert!(
            no_reason.contains(r#"<p class="field-error" id="motif-err">Choisissez un motif.</p>"#)
        );

        let failed = report_body(
            id,
            "Gratin",
            Some("spam"),
            "Pub",
            &ReportErrors::default(),
            Some("Hors service"),
            true,
        );
        assert!(failed.contains(r#"<p class="notice error" role="alert">Hors service</p>"#));
        assert!(failed.contains(r#"value="spam" checked/>"#));
        assert!(failed.contains(">Pub</textarea>"));
    }

    #[test]
    fn without_a_family_the_screen_drops_the_recipes_tab() {
        let id = Uuid::from_u128(5);
        let html = report_body(
            id,
            "Gratin",
            None,
            "",
            &ReportErrors::default(),
            None,
            false,
        );
        assert!(!html.contains(r#"class="segment""#), "{html}");
        assert!(!html.contains(r#"href="/recipes""#));
        assert!(html.contains(&format!(r#"action="/recipes/library/{id}/report""#)));
        let with = report_body(id, "Gratin", None, "", &ReportErrors::default(), None, true);
        assert!(with.starts_with(&library_segment(true)));
    }
}
