//! `/account/sessions` — the member's live sessions (#225): when each was
//! opened and last used, which one is this one, and one button that ends
//! them all, this one included (arbitrated 2026-10-02): the member is then
//! sent back to the login page. No session ends on its own (#753): no
//! button per row.
//!
//! Dates only. The service keeps no IP and no user-agent for a session
//! (data minimization, arbitrated 2026-10-02), so the page says so rather
//! than leaving the member to wonder which device is which.
//!
//! The one way out for a Google-only account, which has no password to
//! change — changing it was the only other way to end the other sessions.

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{Html, IntoResponse, Redirect, Response};
use manage_our_home_shared::dto::auth::{ActiveSession, SessionClient};

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{account_cookie, account_header, format_rgpd_datetime, service_unavailable_page};

#[derive(serde::Deserialize)]
pub struct PageQuery {
    error: Option<String>,
}

fn error_html(error: Option<&str>) -> String {
    let text = match error {
        Some("unavailable") => "Service momentanément indisponible, merci de réessayer.",
        _ => return String::new(),
    };
    format!(r#"<p class="notice error">{}</p>"#, html_escape(text))
}

/// The body of the page, from `GET /auth/sessions`'s answer.
fn page_body(sessions: &[ActiveSession], error: &str) -> String {
    let rows: String = sessions.iter().map(session_row).collect();
    format!(
        r#"<p><a href="/account">← Retour à mon compte</a></p>
<h1>Sessions actives</h1>
{error}
<p>Chaque connexion ouvre une session, sur un appareil ou un navigateur. Une session prend fin d'elle-même après 7 jours sans activité, et au plus tard 30 jours après la connexion.</p>
<p class="muted">Le service ne conserve ni l'adresse IP ni le navigateur d'une session : seulement ses dates et son origine, l'application ou le site web.</p>
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

/// Where a session was opened from, as its row reveals it; `None` is a
/// session opened before the origin was recorded (#754).
fn origin_label(client: Option<SessionClient>) -> &'static str {
    match client {
        Some(SessionClient::App) => "l'application",
        Some(SessionClient::Web) => "le site web",
        None => "origine inconnue",
    }
}

/// One session, a disclosure (#755): its dates and the "this session"
/// badge on the current one in the summary, its origin once opened.
fn session_row(session: &ActiveSession) -> String {
    let opened = html_escape(&format_rgpd_datetime(session.created_at));
    let last_seen = html_escape(&format_rgpd_datetime(session.last_seen_at));
    let end = if session.current {
        r#"<span class="badge">Cette session</span>"#
    } else {
        ""
    };
    format!(
        r#"<li class="list-row stacked" data-session="{id}">
<details>
<summary>Ouverte le {opened}<br/><span class="muted">Dernière activité le {last_seen}</span> {end}</summary>
<p>Ouverte depuis : {origin}</p>
</details>
</li>"#,
        id = session.id,
        origin = origin_label(session.client),
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
    let body = page_body(&sessions, &error_html(query.error.as_deref()));
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
    use uuid::Uuid;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn session(id: u128, current: bool) -> ActiveSession {
        ActiveSession {
            id: Uuid::from_u128(id),
            created_at: at("2026-09-28T07:30:00Z"),
            last_seen_at: at("2026-10-02T12:05:00Z"),
            current,
            client: None,
        }
    }

    fn session_from(id: u128, client: Option<SessionClient>) -> ActiveSession {
        ActiveSession {
            client,
            ..session(id, false)
        }
    }

    /// The `<summary>` of a row: what shows while it is closed.
    fn summary_of(row: &str) -> &str {
        let start = row.find("<summary>").expect("the row has a summary");
        let end = row.find("</summary>").expect("the summary is closed");
        &row[start..end]
    }

    /// What a row reveals once opened: after its summary, inside its
    /// `<details>`.
    fn detail_of(row: &str) -> &str {
        let start = row.find("</summary>").expect("the summary is closed");
        let end = row.find("</details>").expect("the disclosure is closed");
        &row[start..end]
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
        let body = page_body(&[session(1, true)], "");
        let row = row_of(&body, 1);
        assert!(row.contains("28/09/2026 à 09:30"), "{row}");
        assert!(row.contains("02/10/2026 à 14:05"), "{row}");
    }

    /// The current session is named, closed row included; no other row is.
    #[test]
    fn the_current_session_is_marked() {
        let body = page_body(&[session(1, true), session(2, false)], "");
        let current = row_of(&body, 1);
        assert!(summary_of(&current).contains("Cette session"), "{current}");
        let other = row_of(&body, 2);
        assert!(!other.contains("Cette session"), "{other}");
    }

    /// Each row is a disclosure without JavaScript (#755): closed, it
    /// shows the dates; the origin is in what it reveals.
    #[test]
    fn each_session_is_a_disclosure_whose_summary_shows_the_dates() {
        let body = page_body(&[session_from(1, Some(SessionClient::App))], "");
        let row = row_of(&body, 1);
        assert!(row.contains("<details>"), "{row}");
        let summary = summary_of(&row);
        assert!(summary.contains("28/09/2026 à 09:30"), "{summary}");
        assert!(summary.contains("02/10/2026 à 14:05"), "{summary}");
        assert!(!summary.contains("Ouverte depuis"), "{summary}");
        assert!(!body.contains("<script"), "{body}");
    }

    /// The origin, one wording per value of `ActiveSession.client` (#754):
    /// the app, the website, or unknown for a session opened before the
    /// origin was recorded.
    #[test]
    fn opening_a_session_reveals_where_it_was_opened_from() {
        let cases = [
            (Some(SessionClient::App), "Ouverte depuis : l'application"),
            (Some(SessionClient::Web), "Ouverte depuis : le site web"),
            (None, "Ouverte depuis : origine inconnue"),
        ];
        for (client, expected) in cases {
            let body = page_body(&[session_from(1, client)], "");
            let row = row_of(&body, 1);
            assert!(detail_of(&row).contains(expected), "{client:?}: {row}");
        }
    }

    /// No row ends its session on its own (#753): ending sessions is the
    /// global button's, all of them at once, or logging out. The page
    /// holds one form, the global one.
    #[test]
    fn no_row_has_a_revoke_button_of_its_own() {
        let body = page_body(&[session(1, true), session(2, false)], "");
        for id in [1, 2] {
            let row = row_of(&body, id);
            assert!(!row.contains("<form"), "{row}");
            assert!(!row.contains("<button"), "{row}");
        }
        assert_eq!(body.matches("<form").count(), 1, "{body}");
        assert!(!body.contains(r#"/revoke""#), "{body}");
    }

    /// The global button ends every session, this one included
    /// (arbitrated 2026-10-02), and says so before it is pressed.
    #[test]
    fn the_global_button_ends_every_session_this_one_included() {
        let body = page_body(&[session(1, true)], "");
        assert!(body.contains(r#"<form method="post" action="/account/sessions/revoke-all">"#));
        assert!(body.contains(">Déconnecter toutes les sessions</button>"));
        assert!(body.contains("celle-ci comprise"), "{body}");
        assert!(!body.contains("toutes les autres"), "{body}");
    }

    /// No IP and no user-agent are kept: the page says so, and that the
    /// dates and the origin (#754) are what tells sessions apart.
    #[test]
    fn the_page_says_no_device_information_is_kept() {
        let body = page_body(&[session(1, true)], "");
        assert!(body.contains("ni l'adresse IP ni le navigateur"), "{body}");
        assert!(
            body.contains("ses dates et son origine, l'application ou le site web"),
            "{body}"
        );
        assert!(!body.contains("seules ses dates"), "{body}");
    }

    #[test]
    fn the_banners_are_rendered_where_given() {
        let body = page_body(&[session(1, true)], &error_html(Some("unavailable")));
        assert!(
            body.contains("Service momentanément indisponible"),
            "{body}"
        );
        assert_eq!(error_html(Some("bogus")), "");
    }

    /// The banners of the per-session revocation went with it (#753).
    #[test]
    fn the_per_session_banners_are_gone() {
        assert_eq!(error_html(Some("session_not_found")), "");
    }
}
