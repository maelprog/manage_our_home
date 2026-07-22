//! Stocks screens (front epic #4, issue #19): the family inventory list with
//! a low-stock indicator, manual add, and per-item edit/quantity-adjust/delete
//! behind a permission bar. Same SSR pattern as `routes/agenda/*` and
//! `routes/groups/*` — plain `<form method=post>` submissions, per-page error
//! tables mapping `apps/api/src/stocks/`'s exact status/error codes to French
//! copy, PRG (`?notice=`/`?error=` codes) after every mutation.
//!
//! Permission bar (#19 + follow-up #39): the backend runs a two-tier bar. A
//! **quantity-only** PATCH is open to any family member (shared inventory), so
//! the adjust form renders for everyone; the full-record edit and delete stay
//! behind `can_modify` (creator or group admin/owner), so the edit link and
//! delete button render only for those users. The backend stays the authority
//! (a forged full-edit/delete is still 403'd, mapped defensively here).

pub mod detail;
pub mod edit;
pub mod list;
pub mod new;

use axum::http::HeaderMap;
use axum::response::Html;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::GroupSummary;
use uuid::Uuid;

use crate::app::shell;
use crate::family::{active_group_id_from_headers, resolve_active_group};
use crate::routes::groups::{cookie_of, header_with_groups};
use crate::state::AppState;

/// Resolved active-family context for a Stocks page: the family id every
/// `/groups/:gid/…` API call is scoped to, the caller's role in it (for the
/// permission bar), and the shared authenticated header. `None` means the user
/// has no group yet — callers redirect to `/groups/new`.
pub(crate) struct FamilyContext {
    pub gid: Uuid,
    pub role: String,
    pub header: String,
}

pub(crate) async fn family_context(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    redirect_to: &str,
) -> Option<FamilyContext> {
    let (groups, header) = header_with_groups(state, headers, me, redirect_to).await;
    let preferred = active_group_id_from_headers(headers);
    let active: Option<&GroupSummary> = resolve_active_group(&groups, preferred);
    active.map(|g| FamilyContext {
        gid: g.group_id,
        role: g.role.clone(),
        header,
    })
}

/// Mirror of `apps/api/src/stocks/mod.rs::can_modify`: the item's creator, or a
/// group owner/admin, may edit the full record or delete it. Used to decide
/// whether to render the edit/delete controls (the backend still 403s a forged
/// request). Quantity adjustment is *not* gated by this — it's open to any
/// member (issue #39).
pub(crate) fn can_modify(role: &str, is_creator: bool) -> bool {
    is_creator || role == "owner" || role == "admin"
}

/// Fetches `cookie_of` for a Stocks handler (re-exported so the submodules
/// don't each reach into `routes::groups`).
pub(crate) fn stocks_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_of(headers)
}

// -- shared error/landing pages ---------------------------------------------

pub(crate) fn item_not_found_page() -> Html<String> {
    let body = view! {
        <h1>"Article introuvable"</h1>
        <p>"Cet article n'existe pas ou vous n'y avez pas accès."</p>
        <a class="button secondary" href="/stocks">"Retour aux stocks"</a>
    };
    Html(shell("Article introuvable", &body.to_html()))
}

pub(crate) fn service_unavailable_page() -> Html<String> {
    let body = view! {
        <h1>"Service momentanément indisponible"</h1>
        <p>"Merci de réessayer dans quelques instants."</p>
        <a class="button secondary" href="/stocks">"Retour aux stocks"</a>
    };
    Html(shell("Service indisponible", &body.to_html()))
}

pub(crate) fn forbidden_page() -> Html<String> {
    let body = view! {
        <h1>"Action non autorisée"</h1>
        <p>"Vous n'avez pas les droits nécessaires sur cet article."</p>
        <a class="button secondary" href="/stocks">"Retour aux stocks"</a>
    };
    Html(shell("Action non autorisée", &body.to_html()))
}

/// Formats an `f64` quantity/threshold for display and form pre-fill without a
/// trailing `.0` on whole numbers (`2.0` → `"2"`, `0.5` → `"0.5"`).
pub(crate) fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        // Trim trailing zeros from a fixed rendering.
        let s = format!("{n}");
        s
    }
}
