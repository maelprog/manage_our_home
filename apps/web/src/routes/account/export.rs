//! `/account/export` — RGPD Art. 15/20 (accès + portabilité). The page states
//! what the export contains and what it deliberately leaves out; the download
//! itself is a second route so the explanation can be linked/bookmarked and a
//! refresh never re-triggers a download.
//!
//! `/account/export/download` relays `GET /account/export` and re-serves the
//! bytes **verbatim** under a dated filename: the document is the user's own
//! data, so `apps/web` doesn't parse and re-serialize it (nothing here could
//! reshape it on the way out), it only adds the `Content-Disposition` the browser
//! needs to save a file. The `account_data_exported` audit row is written
//! server-side by apps/api, so there is no extra client action.

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::Utc;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_get_raw, AppState};

use super::{account_cookie, account_header, export_filename, service_unavailable_page};

#[derive(serde::Deserialize)]
pub struct ExportQuery {
    error: Option<String>,
}

fn error_html(error: Option<&str>) -> String {
    let text = match error {
        Some("unavailable") => {
            "L'export n'a pas pu être généré, merci de réessayer dans quelques instants."
        }
        _ => return String::new(),
    };
    format!(r#"<p class="notice error">{}</p>"#, html_escape(text))
}

/// `GET /account/export` — the explanation page + the download button.
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ExportQuery>,
) -> Response {
    let header = account_header(&state, &headers, &me, "/account/export").await;
    let body = format!(
        r#"<p><a href="/account">← Retour à mon compte</a></p>
<h1>Exporter mes données</h1>
{error}
<p>Le fichier JSON téléchargé contient votre profil et, pour chaque famille dont vous êtes membre, tout le contenu <strong>que vous avez créé</strong> : événements d'agenda, articles de stock, recettes, repas enregistrés, articles de liste de courses, dépenses, messages, et les connexions d'import calendrier.</p>
<p class="muted">Ce que l'export ne contient pas, par conception : le contenu créé par les autres membres de vos familles (l'export est strictement personnel), et l'URL secrète de vos flux calendrier — c'est un identifiant d'accès à votre compte Google, jamais réaffiché en clair, y compris ici (seules les métadonnées de la connexion sont exportées).</p>
<p class="muted">Chaque export est enregistré dans le journal d'audit du service.</p>
<a class="btn" href="/account/export/download">Télécharger mes données (JSON)</a>"#,
        error = error_html(query.error.as_deref()),
    );
    Html(shell_with_header(
        Width::Read,
        "Exporter mes données",
        &header,
        &body,
    ))
    .into_response()
}

/// `GET /account/export/download` — relays `GET /account/export` with the
/// caller's session cookie and serves the bytes as an attachment. 200 → the
/// download; 401 → `/login` (the session died between the two requests); any
/// other status → back to the export page with a banner; a transport error → the
/// service-unavailable page.
pub async fn download(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let cookie = account_cookie(&headers);
    match api_get_raw(&state, "/account/export", cookie.as_deref()).await {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            // The filename is generated (dated, ASCII) — no user input reaches
            // this header, so there is nothing to escape/encode here.
            let disposition = format!(r#"attachment; filename="{}""#, export_filename(Utc::now()));
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "application/json; charset=utf-8"),
                    (header::CONTENT_DISPOSITION, disposition.as_str()),
                ],
                resp.body,
            )
                .into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNAUTHORIZED => {
            Redirect::to("/login").into_response()
        }
        Ok(_) => Redirect::to("/account/export?error=unavailable").into_response(),
        Err(_) => service_unavailable_page().into_response(),
    }
}
