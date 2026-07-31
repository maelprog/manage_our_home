//! Recipes screens (front epic F5, issue #20): the family recipe list with a
//! ranked suggestion view, manual create, per-recipe detail with a
//! log-a-meal action, and edit/delete behind a permission bar. Same SSR
//! pattern as `routes/stocks/*` and `routes/agenda/*` — plain
//! `<form method=post>` submissions, per-page error tables mapping
//! `apps/api/src/recipes/`'s exact status/error codes to French copy, PRG
//! (`?notice=`/`?error=` codes) after every mutation. Full spec (route
//! table, error tables, acceptance criteria, the score-vs-order and
//! grocery-list-hook decisions) in `docs/front-epic-5-recipes.md`.
//!
//! Permission bar (mirrors `apps/api/src/recipes/mod.rs::can_modify`): any
//! family member may create/read a recipe or log a meal, so those controls
//! render for everyone; the edit link and delete button render only for the
//! recipe's creator or a group admin/owner. The backend stays the authority
//! (a forged edit/delete/log is still 403'd, mapped defensively here).

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

use crate::app::{shell, Width};
use crate::family::{active_group_id_from_headers, resolve_active_group};
use crate::routes::groups::{cookie_of, header_with_groups};
use crate::state::AppState;

/// Resolved active-family context for a Recipes page: the family id every
/// `/groups/:gid/…` API call is scoped to, the caller's role in it (for the
/// permission bar), and the shared authenticated header. `None` means the
/// user has no group yet — callers redirect to `/groups/new`.
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

/// Mirror of `apps/api/src/recipes/mod.rs::can_modify`: the recipe's
/// creator, or a group owner/admin, may edit or delete it. Used to decide
/// whether to render the edit/delete controls (the backend still 403s a
/// forged request). Create/read/log-a-meal are *not* gated by this — they're
/// open to any member.
pub(crate) fn can_modify(role: &str, is_creator: bool) -> bool {
    is_creator || role == "owner" || role == "admin"
}

/// Fetches `cookie_of` for a Recipes handler (re-exported so the submodules
/// don't each reach into `routes::groups`).
pub(crate) fn recipes_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_of(headers)
}

// -- shared error/landing pages ---------------------------------------------

pub(crate) fn recipe_not_found_page() -> Html<String> {
    let body = view! {
        <h1>"Recette introuvable"</h1>
        <p>"Cette recette n'existe pas ou vous n'y avez pas accès."</p>
        <a class="btn secondary" href="/recipes">"Retour aux recettes"</a>
    };
    Html(shell(Width::Form, "Recette introuvable", &body.to_html()))
}

pub(crate) fn service_unavailable_page() -> Html<String> {
    let body = view! {
        <h1>"Service momentanément indisponible"</h1>
        <p>"Merci de réessayer dans quelques instants."</p>
        <a class="btn secondary" href="/recipes">"Retour aux recettes"</a>
    };
    Html(shell(Width::Form, "Service indisponible", &body.to_html()))
}

pub(crate) fn forbidden_page() -> Html<String> {
    let body = view! {
        <h1>"Action non autorisée"</h1>
        <p>"Vous n'avez pas les droits nécessaires sur cette recette."</p>
        <a class="btn secondary" href="/recipes">"Retour aux recettes"</a>
    };
    Html(shell(Width::Form, "Action non autorisée", &body.to_html()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creator_can_modify_regardless_of_role() {
        assert!(can_modify("standard", true));
    }

    #[test]
    fn owner_and_admin_can_modify_others_recipes() {
        assert!(can_modify("owner", false));
        assert!(can_modify("admin", false));
    }

    #[test]
    fn standard_member_cannot_modify_another_members_recipe() {
        assert!(!can_modify("standard", false));
    }
}
