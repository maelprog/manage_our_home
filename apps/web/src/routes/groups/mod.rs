//! Groups screens (issue #17): list/create/join, member management,
//! settings, and the active-family switcher. Same SSR pattern as
//! `routes/auth/*` — plain `<form method=post>` submissions, per-page
//! error tables mapping apps/api's exact status/error codes to French
//! copy, PRG (`?notice=`/`?error=` codes) after mutating actions so a
//! refresh never replays them.

pub mod invitations;
pub mod list;
pub mod members;
pub mod new;
pub mod settings;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::GroupSummary;
use uuid::Uuid;

use crate::app::app_header;
use crate::family::{active_group_id_from_headers, resolve_active_group, set_active_group_cookie};
use crate::state::{fetch_groups, AppState};

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
        _ => "Membre",
    }
}

/// Fetches the caller's groups and renders the shared authenticated
/// header (nav + family switcher) for a Groups page.
pub(crate) async fn header_with_groups(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    redirect_to: &str,
) -> (Vec<GroupSummary>, String) {
    let cookie = cookie_of(headers);
    let groups = fetch_groups(state, cookie.as_deref())
        .await
        .unwrap_or_default();
    let preferred = active_group_id_from_headers(headers);
    let active = resolve_active_group(&groups, preferred);
    let header = app_header(me, &groups, active, redirect_to);
    (groups, header)
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
