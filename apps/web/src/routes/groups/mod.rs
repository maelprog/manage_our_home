//! Groups screens (issue #17): list/create/join, member management,
//! settings, and the active-family switcher. Same SSR pattern as
//! `routes/auth/*` — plain `<form method=post>` submissions, per-page
//! error tables mapping apps/api's exact status/error codes to French
//! copy, PRG (`?notice=`/`?error=` codes) after mutating actions so a
//! refresh never replays them.

pub mod avatar;
pub mod invitations;
pub mod list;
pub mod members;
pub mod new;
pub mod ownership;
pub mod profile;
pub mod settings;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::GroupSummary;
use manage_our_home_shared::validation::groups::can_manage_group;
use uuid::Uuid;

use crate::app::{app_header, nav_link_is_current};
use crate::family::{active_group_id_from_headers, resolve_active_group, set_active_group_cookie};
use crate::state::{api_request_auth, fetch_groups, AppState};

pub(crate) fn cookie_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// French label for a backend role code, used everywhere a role is shown.
pub(crate) fn role_label(role: &str) -> &'static str {
    match role {
        "owner" => "Propriétaire",
        "admin" => "Admin",
        _ => "Standard",
    }
}

/// A member's role as a pastille (#530): the two roles that manage the
/// family in the accent's tint, the standard one in the neutral badge. The
/// word says the role; the tint only repeats it.
pub(crate) fn role_badge(role: &str) -> String {
    let tint = if can_manage_group(role) {
        " accent"
    } else {
        ""
    };
    format!(
        r#"<span class="badge{tint}">{label}</span>"#,
        label = role_label(role)
    )
}

/// Fetches the caller's groups and renders the shared authenticated
/// header (nav + family switcher) for a Groups page.
///
/// Every authenticated page goes through here, so this is also where the
/// Messagerie tab's unread count is fetched (#463), with the Stocks tab's
/// count of articles past or close to their date (#564) — the former
/// except on the messagerie itself, which advances the read marker *after* its header is
/// built: the count would be the one the page is clearing.
pub(crate) async fn header_with_groups(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    redirect_to: &str,
) -> (Vec<GroupSummary>, String) {
    let (groups, header, _) = header_parts(state, headers, me, redirect_to).await;
    (groups, header)
}

/// `header_with_groups`, plus the caller's unread notifications (#535):
/// the count the sidebar's Notifications entry wears, which the home's
/// bell and `/notifications` show as well. `None` when it could not be
/// read — the header then shows no count, and neither does the page.
pub(crate) async fn header_parts(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    redirect_to: &str,
) -> (Vec<GroupSummary>, String, Option<u64>) {
    let cookie = cookie_of(headers);
    let groups = fetch_groups(state, cookie.as_deref())
        .await
        .unwrap_or_default();
    let preferred = active_group_id_from_headers(headers);
    let active = resolve_active_group(&groups, preferred);
    // The page's colour (#538): `app::document` writes it on `<html>`.
    crate::family::record_active_accent(active);
    let messages = async {
        match active {
            Some(g) if !nav_link_is_current("/messagerie", redirect_to) => {
                fetch_unread_count(state, cookie.as_deref(), g.group_id).await
            }
            _ => 0,
        }
    };
    let expiring = async {
        match active {
            Some(g) => fetch_expiring_count(state, cookie.as_deref(), g.group_id).await,
            None => 0,
        }
    };
    // The three counts in parallel (#564): each is a round trip to apps/api
    // that every authenticated page waits for.
    let (unread, notifications, expiring) = tokio::join!(
        messages,
        fetch_unread_notifications(state, cookie.as_deref()),
        expiring
    );
    let header = app_header(
        me,
        active,
        redirect_to,
        unread,
        notifications.unwrap_or(0),
        expiring,
    );
    (groups, header, notifications)
}

/// `GET /notifications/unread_count` (#534). `None` on any failure, as
/// `fetch_unread_count` reads 0: the page renders without the count.
async fn fetch_unread_notifications(state: &AppState, cookie: Option<&str>) -> Option<u64> {
    match api_request_auth(
        state,
        reqwest::Method::GET,
        "/notifications/unread_count",
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => resp.body["unread_count"].as_u64(),
        _ => None,
    }
}

/// `GET /groups/:id/messages/unread_count` (#463). Any failure — apps/api
/// unreachable, a refusal, a body that does not parse — counts as 0: the
/// tab loses its pastille, the page still renders.
async fn fetch_unread_count(state: &AppState, cookie: Option<&str>, group_id: Uuid) -> u64 {
    let path = format!("/groups/{group_id}/messages/unread_count");
    match api_request_auth(state, reqwest::Method::GET, &path, cookie, None).await {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            resp.body["unread_count"].as_u64().unwrap_or(0)
        }
        _ => 0,
    }
}

/// `GET /groups/:id/stocks/expiring_count` (#564), the Stocks tab's
/// pastille. Any failure counts as 0, as `fetch_unread_count`.
async fn fetch_expiring_count(state: &AppState, cookie: Option<&str>, group_id: Uuid) -> u64 {
    let path = format!("/groups/{group_id}/stocks/expiring_count");
    match api_request_auth(state, reqwest::Method::GET, &path, cookie, None).await {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            resp.body["expiring_count"].as_u64().unwrap_or(0)
        }
        _ => 0,
    }
}

#[derive(serde::Deserialize)]
pub struct SwitchForm {
    group_id: Uuid,
    #[serde(default)]
    redirect_to: String,
}

/// `POST /groups/switch` — the family switcher. Persists the choice in
/// the `active_group_id` cookie *only if* the caller actually belongs to
/// the posted group (re-checked against `GET /groups`, so a forged form
/// can't pin a foreign id), then redirects back to where the switcher was
/// used. Only local paths are honored as redirect targets.
pub async fn switch(
    crate::layout::CurrentUser(_me): crate::layout::CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<SwitchForm>,
) -> Response {
    let target = if form.redirect_to.starts_with('/') && !form.redirect_to.starts_with("//") {
        form.redirect_to.clone()
    } else {
        "/".to_string()
    };

    let cookie = cookie_of(&headers);
    let groups = fetch_groups(&state, cookie.as_deref())
        .await
        .unwrap_or_default();
    if !groups.iter().any(|g| g.group_id == form.group_id) {
        return Redirect::to(&target).into_response();
    }

    let mut response_headers = HeaderMap::new();
    if let Ok(v) = set_active_group_cookie(form.group_id).parse() {
        response_headers.insert(axum::http::header::SET_COOKIE, v);
    }
    (response_headers, Redirect::to(&target)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #530: the three roles of `group_members.role`, under the names the
    /// mock-ups give them.
    #[test]
    fn each_role_has_its_french_name() {
        assert_eq!(role_label("owner"), "Propriétaire");
        assert_eq!(role_label("admin"), "Admin");
        assert_eq!(role_label("standard"), "Standard");
    }

    /// The two roles that manage the family wear the accent's tint, the
    /// standard one the neutral badge; the word carries the role either way
    /// (WCAG 1.4.1).
    #[test]
    fn a_role_badge_is_tinted_for_the_roles_that_manage() {
        assert_eq!(
            role_badge("owner"),
            r#"<span class="badge accent">Propriétaire</span>"#
        );
        assert_eq!(
            role_badge("admin"),
            r#"<span class="badge accent">Admin</span>"#
        );
        assert_eq!(
            role_badge("standard"),
            r#"<span class="badge">Standard</span>"#
        );
    }
}
