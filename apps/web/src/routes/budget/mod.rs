//! Budget screens (front epic F7, issue #22): the family's grocery-derived
//! spend — a monthly summary + entry list (`/budget`), a manual price-entry
//! form (`/budget/new`), and a per-entry edit/delete screen behind a permission
//! bar (`/budget/:id`). Same SSR pattern as `routes/grocery_list/*` and
//! `routes/recipes/*` — plain `<form method=post>` submissions, per-page error
//! tables mapping `apps/api/src/budget/`'s exact status/error codes to French
//! copy, PRG (`?notice=`/`?error=` codes) after every mutation. Full spec
//! (route table, error tables, the strict-scope / euros-formatting /
//! price-on-checkout / idempotent-upsert decisions) in
//! `docs/front-epic-7-budget.md`.
//!
//! Budget is **tied to the grocery list**, not a standalone expense tracker:
//! the only two ways an entry is created are a manual price entry here and the
//! price-on-checkout action wired into `routes/grocery_list/list.rs` (the hook
//! F6 deferred to F7).
//!
//! Permission bar (mirrors `apps/api/src/budget/mod.rs::can_modify`): any
//! family member may create, read, or set a price, so those controls render for
//! everyone; the per-row edit link and the edit screen's edit/delete controls
//! render only for the entry's creator or a group admin/owner. The backend
//! stays the authority (a forged edit/delete is still 403'd, mapped
//! defensively here).

pub mod edit;
pub mod list;
pub mod new;

use axum::http::HeaderMap;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::GroupSummary;
use uuid::Uuid;

use crate::app::shell;
use crate::family::{active_group_id_from_headers, resolve_active_group};
use crate::routes::groups::{cookie_of, header_with_groups};
use crate::state::AppState;

// The permission mirror lives in `apps/shared` (pure, TDD'd) alongside the
// other budget logic; re-export it so the submodules import it from the local
// module like the Grocery-list/Recipes/Stocks pages do.
pub(crate) use manage_our_home_shared::validation::budget::can_modify;

/// Resolved active-family context for a Budget page: the family id every
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

/// Fetches `cookie_of` for a Budget handler (re-exported so the submodules
/// don't each reach into `routes::groups`).
pub(crate) fn budget_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_of(headers)
}

// -- shared error/landing pages ---------------------------------------------

pub(crate) fn entry_not_found_page() -> axum::response::Html<String> {
    let body = view! {
        <h1>"Dépense introuvable"</h1>
        <p>"Cette dépense n'existe pas ou vous n'y avez pas accès."</p>
        <a class="button secondary" href="/budget">"Retour au budget"</a>
    };
    axum::response::Html(shell("Dépense introuvable", &body.to_html()))
}

pub(crate) fn service_unavailable_page() -> axum::response::Html<String> {
    let body = view! {
        <h1>"Service momentanément indisponible"</h1>
        <p>"Merci de réessayer dans quelques instants."</p>
        <a class="button secondary" href="/budget">"Retour au budget"</a>
    };
    axum::response::Html(shell("Service indisponible", &body.to_html()))
}

pub(crate) fn forbidden_page() -> axum::response::Html<String> {
    let body = view! {
        <h1>"Action non autorisée"</h1>
        <p>"Vous n'avez pas les droits nécessaires sur cette dépense."</p>
        <a class="button secondary" href="/budget">"Retour au budget"</a>
    };
    axum::response::Html(shell("Action non autorisée", &body.to_html()))
}
