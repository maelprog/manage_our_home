//! Grocery-list screens (front epic F6, issue #21): the family's one shared
//! list with inline manual add, a generate-from-recipes/stocks button, and
//! per-row check-off, plus a per-item edit/delete screen behind a permission
//! bar. Same SSR pattern as `routes/recipes/*` and `routes/stocks/*` — plain
//! `<form method=post>` submissions, per-page error tables mapping
//! `apps/api/src/grocery_list/`'s exact status/error codes to French copy,
//! PRG (`?notice=`/`?error=` codes) after every mutation. Full spec (route
//! table, error tables, the inline-add / idempotent-generate / no-JS
//! check-off / Budget-hook decisions) in
//! `docs/front-epic-6-grocery-list.md`.
//!
//! The Budget price-on-checkout hook F6 deferred is now wired here (F7, #22):
//! `list::price` renders an inline "Renseigner le prix" form on each checked
//! item, posting to `POST /grocery-list/:id/price` (see
//! `docs/front-epic-7-budget.md`). It is purely additive — the check-off
//! behaviour itself is unchanged.
//!
//! Permission bar (mirrors `apps/api/src/grocery_list/mod.rs::can_modify`):
//! any family member may add, read, or **check off** an item, so those
//! controls render for everyone; the per-row edit link and the edit screen's
//! edit/delete controls render only for the item's creator or a group
//! admin/owner. The backend stays the authority (a forged edit/delete is
//! still 403'd, mapped defensively here).

pub mod edit;
pub mod list;

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

// The permission mirror lives in `apps/shared` (pure, TDD'd) alongside the
// other grocery-list logic; re-export it so the submodules import it from the
// local module like the Recipes/Stocks pages do.
pub(crate) use manage_our_home_shared::validation::grocery_list::can_modify;

/// Resolved active-family context for a Grocery-list page: the family id every
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

/// Fetches `cookie_of` for a Grocery-list handler (re-exported so the
/// submodules don't each reach into `routes::groups`).
pub(crate) fn grocery_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_of(headers)
}

// -- shared error/landing pages ---------------------------------------------

pub(crate) fn item_not_found_page() -> Html<String> {
    let body = view! {
        <h1>"Article introuvable"</h1>
        <p>"Cet article n'existe pas ou vous n'y avez pas accès."</p>
        <a class="btn secondary" href="/grocery-list">"Retour à la liste de courses"</a>
    };
    Html(shell("Article introuvable", &body.to_html()))
}

pub(crate) fn service_unavailable_page() -> Html<String> {
    let body = view! {
        <h1>"Service momentanément indisponible"</h1>
        <p>"Merci de réessayer dans quelques instants."</p>
        <a class="btn secondary" href="/grocery-list">"Retour à la liste de courses"</a>
    };
    Html(shell("Service indisponible", &body.to_html()))
}

pub(crate) fn forbidden_page() -> Html<String> {
    let body = view! {
        <h1>"Action non autorisée"</h1>
        <p>"Vous n'avez pas les droits nécessaires sur cet article."</p>
        <a class="btn secondary" href="/grocery-list">"Retour à la liste de courses"</a>
    };
    Html(shell("Action non autorisée", &body.to_html()))
}
