//! `/account/sessions` — the member's live sessions (#225): when each was
//! opened and last used, which one is this one, a button to end any other,
//! and one to end them all, this one included (arbitrated 2026-10-02): the
//! member is then sent back to the login page.
//!
//! Dates only. The service keeps no IP and no user-agent for a session
//! (data minimization, arbitrated 2026-10-02), so the page says so rather
//! than leaving the member to wonder which device is which.
//!
//! The one way out for a Google-only account, which has no password to
//! change — changing it was the only other way to end the other sessions.

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{Html, IntoResponse, Redirect, Response};
use manage_our_home_shared::dto::auth::ActiveSession;
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{account_cookie, account_header, format_rgpd_datetime, service_unavailable_page};

#[derive(serde::Deserialize)]
pub struct PageQuery {
    notice: Option<String>,
    error: Option<String>,
}

fn notice_html(notice: Option<&str>) -> String {
    let text = match notice {
        Some("session_revoked") => "La session a été déconnectée.",
        _ => return String::new(),
    };
    format!(r#"<p class="notice success">{}</p>"#, html_escape(text))
}

fn error_html(error: Option<&str>) -> String {
    let text = match error {
        Some("session_not_found") => "Cette session était déjà terminée.",
        Some("unavailable") => "Service momentanément indisponible, merci de réessayer.",
        _ => return String::new(),
    };
    format!(r#"<p class="notice error">{}</p>"#, html_escape(text))
}

/// The body of the page, from `GET /auth/sessions`'s answer.
fn page_body(sessions: &[ActiveSession], notice: &str, error: &str) -> String {
    let rows: String = sessions.iter().map(session_row).collect();
    format!(
        r#"<p><a href="/account">← Retour à mon compte</a></p>
<h1>Sessions actives</h1>
{notice}{error}
<p>Chaque connexion ouvre une session, sur un appareil ou un navigateur. Une session prend fin d'elle-même après 7 jours sans activité, et au plus tard 30 jours après la connexion.</p>
<p class="muted">Le service ne conserve ni l'adresse IP ni le navigateur d'une session : seules ses dates permettent de la reconnaître.</p>
<ul class="list">{rows}</ul>
<section class="card">
<h2>Déconnecter toutes les sessions</h2>
<p class="muted">Toutes vos sessions prennent fin, celle-ci comprise : vous devrez vous reconnecter, sur cet appareil aussi.</p>
<form method="post" action="/account/sessions/revoke-all">
<button type="submit" class="danger">Déconnecter toutes les sessions</button>
</form>
</section>"#
    )
}

/// One session: its dates, then either the "this session" badge or the
/// form that ends it. The button's visible text is the same on every row,
/// so its accessible name carries the opening date that tells them apart.
fn session_row(session: &ActiveSession) -> String {
    let opened = html_escape(&format_rgpd_datetime(session.created_at));
    let last_seen = html_escape(&format_rgpd_datetime(session.last_seen_at));
    let end = if session.current {
        r#"<span class="badge">Cette session</span>"#.to_string()
    } else {
        format!(
            r#"<form method="post" action="/account/sessions/{id}/revoke">
<button type="submit" class="secondary" aria-label="Déconnecter la session ouverte le {opened}">Déconnecter</button>
</form>"#,
            id = session.id,
        )
    };
    format!(
        r#"<li class="list-row" data-session="{id}">
<span>Ouverte le {opened}<br/><span class="muted">Dernière activité le {last_seen}</span></span>
{end}
</li>"#,
        id = session.id,
    )
}

/// `GET /auth/sessions` from apps/api, `None` on any failure.
async fn fetch_sessions(state: &AppState, cookie: Option<&str>) -> Option<Vec<ActiveSession>> {
    let resp = api_request_auth(state, reqwest::Method::GET, "/auth/sessions", cookie, None)
        .await
        .ok()?;
    if resp.status != reqwest::StatusCode::OK {
        return None;
    }
    serde_json::from_value(resp.body).ok()
}

/// `GET /account/sessions`.
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Response {
    let cookie = account_cookie(&headers);
    let Some(sessions) = fetch_sessions(&state, cookie.as_deref()).await else {
        return service_unavailable_page().into_response();
    };
    let header = account_header(&state, &headers, &me, "/account/sessions").await;
    let body = page_body(
        &sessions,
        &notice_html(query.notice.as_deref()),
        &error_html(query.error.as_deref()),
    );
    Html(shell_with_header(
        Width::Read,
        "Sessions actives",
        &header,
        &body,
    ))
    .into_response()
}

/// A redirect to the login page carrying apps/api's removal of the session
/// cookie, when it sent one.
fn back_to_login(set_cookie: Option<String>) -> Response {
    let mut response = Redirect::to("/login").into_response();
    if let Some(value) = set_cookie.and_then(|v| HeaderValue::from_str(&v).ok()) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

/// `POST /account/sessions/:id/revoke` — relays to
/// `POST /auth/sessions/:id/revoke`. 204 → back to the list; 204 with the
/// cookie cleared (the page offers no button for this session, but a
/// request can name it) → the login page; 404 → the "already ended"
/// banner; any other status → the generic banner; a transport error → the
/// service-unavailable page.
pub async fn revoke(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let cookie = account_cookie(&headers);
    let path = format!("/auth/sessions/{id}/revoke");
    match api_request_auth(
        &state,
        reqwest::Method::POST,
        &path,
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => match resp.set_cookie {
            Some(cleared) => back_to_login(Some(cleared)),
            None => Redirect::to("/account/sessions?notice=session_revoked").into_response(),
        },
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            Redirect::to("/account/sessions?error=session_not_found").into_response()
        }
        Ok(_) => Redirect::to("/account/sessions?error=unavailable").into_response(),
        Err(_) => service_unavailable_page().into_response(),
    }
}

/// `POST /account/sessions/revoke-all` — relays to
/// `POST /auth/sessions/revoke-all`. 204 → the login page, the session
/// cookie cleared: this session ended with the others. Any other status →
/// the generic banner; a transport error → the service-unavailable page.
pub async fn revoke_all(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let cookie = account_cookie(&headers);
    match api_request_auth(
        &state,
        reqwest::Method::POST,
        "/auth/sessions/revoke-all",
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            back_to_login(resp.set_cookie)
        }
        Ok(_) => Redirect::to("/account/sessions?error=unavailable").into_response(),
        Err(_) => service_unavailable_page().into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn session(id: u128, current: bool) -> ActiveSession {
        ActiveSession {
            id: Uuid::from_u128(id),
            created_at: at("2026-09-28T07:30:00Z"),
            last_seen_at: at("2026-10-02T12:05:00Z"),
            current,
        }
    }

    /// The `<li>` of the session `id`, which the assertions below look into.
    fn row_of(body: &str, id: u128) -> String {
        let marker = format!(r#"data-session="{}""#, Uuid::from_u128(id));
        let start = body.find(&marker).expect("the session has a row");
        let start = body[..start].rfind("<li").unwrap();
        let end = start + body[start..].find("</li>").unwrap();
        body[start..end].to_string()
    }

    /// Both dates, in Europe/Paris, as the rest of the account screens
    /// write them.
    #[test]
    fn each_session_shows_when_it_was_opened_and_last_used() {
        let body = page_body(&[session(1, true)], "", "");
        let row = row_of(&body, 1);
        assert!(row.contains("28/09/2026 à 09:30"), "{row}");
        assert!(row.contains("02/10/2026 à 14:05"), "{row}");
    }

    /// The current session is named, and offers no button of its own:
    /// ending it is the global button's, or logging out.
    #[test]
    fn the_current_session_is_marked_and_has_no_revoke_button() {
        let body = page_body(&[session(1, true), session(2, false)], "", "");
        let current = row_of(&body, 1);
        assert!(current.contains("Cette session"), "{current}");
        assert!(!current.contains("<form"), "{current}");
        let other = row_of(&body, 2);
        assert!(!other.contains("Cette session"), "{other}");
    }

    /// Every other session has its own form, posting to its own id, and a
    /// button whose name says which session it ends.
    #[test]
    fn every_other_session_has_its_own_revoke_form() {
        let body = page_body(&[session(1, true), session(2, false)], "", "");
        let other = row_of(&body, 2);
        let action = format!(
            r#"<form method="post" action="/account/sessions/{}/revoke""#,
            Uuid::from_u128(2)
        );
        assert!(other.contains(&action), "{other}");
        assert!(
            other.contains(r#"aria-label="Déconnecter la session ouverte le 28/09/2026 à 09:30""#),
            "{other}"
        );
        assert!(other.contains(">Déconnecter</button>"), "{other}");
    }

    /// The global button ends every session, this one included
    /// (arbitrated 2026-10-02), and says so before it is pressed.
    #[test]
    fn the_global_button_ends_every_session_this_one_included() {
        let body = page_body(&[session(1, true)], "", "");
        assert!(body.contains(r#"<form method="post" action="/account/sessions/revoke-all">"#));
        assert!(body.contains(">Déconnecter toutes les sessions</button>"));
        assert!(body.contains("celle-ci comprise"), "{body}");
        assert!(!body.contains("toutes les autres"), "{body}");
    }

    /// No IP and no user-agent are kept: the page says the dates are all
    /// there is to tell sessions apart.
    #[test]
    fn the_page_says_no_device_information_is_kept() {
        let body = page_body(&[session(1, true)], "", "");
        assert!(body.contains("ni l'adresse IP ni le navigateur"), "{body}");
    }

    #[test]
    fn the_banners_are_rendered_where_given() {
        let body = page_body(
            &[session(1, true)],
            &notice_html(Some("session_revoked")),
            &error_html(None),
        );
        assert!(body.contains("La session a été déconnectée."));
        assert_eq!(error_html(Some("bogus")), "");
        assert!(error_html(Some("session_not_found")).contains("déjà terminée"));
    }
}
