//! Superadmin support screens (front epic F9, issue #24): read-only look-up of
//! every family (`/admin/groups`) and every account (`/admin/users`), plus the
//! immediate `deactivate` support action on a user (`/admin/users/:id`). Same
//! SSR pattern as the family-scoped epics — plain `<form method=post>`, PRG with
//! `?notice=`/`?error=` codes, per-page error tables mapping
//! `apps/api/src/user_admin/`'s exact status codes to French copy. Full spec in
//! `docs/front-epic-9-user-admin.md`.
//!
//! Independent of any family/group context (cross-family visibility is the one
//! gated exception to the RLS boundary — see `architecture.md`). Every handler
//! extracts `CurrentSuperAdmin`, which redirects a non-superadmin to `/` before
//! the body runs, so the whole tree is gated client-side; the backend's
//! `SuperAdminUser` extractor stays the authority for the `/admin/*` API calls.
//! Every successful admin action already writes an `audit_log` row server-side,
//! so there is no separate audit UI here (issue #24 note).

pub mod groups;
pub mod users;

use axum::http::HeaderMap;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::MeResponse;

use crate::app::shell;
use crate::routes::groups::{cookie_of, header_with_groups};
use crate::state::AppState;

// Re-export the pure, TDD'd helpers from `apps/shared` so the submodules import
// them from the local module like the other epics do.
pub(crate) use manage_our_home_shared::validation::user_admin::{
    can_deactivate, format_admin_datetime, format_admin_datetime_opt, user_status_label,
};

/// The caller's session cookie, forwarded to the authenticated `/admin/*` API
/// calls (re-exported so the submodules don't reach into `routes::groups`).
pub(crate) fn admin_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_of(headers)
}

/// Renders the shared authenticated header (nav + family switcher) for an admin
/// page. The `/admin` nav link itself only renders for a superadmin (see
/// `app::app_header`), and these handlers are already gated by
/// `CurrentSuperAdmin`, so the header is consistent with the rest of the app.
pub(crate) async fn admin_header(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    redirect_to: &str,
) -> String {
    let (_groups, header) = header_with_groups(state, headers, me, redirect_to).await;
    header
}

// -- shared error pages -----------------------------------------------------

pub(crate) fn service_unavailable_page() -> axum::response::Html<String> {
    let body = view! {
        <h1>"Service momentanément indisponible"</h1>
        <p>"Merci de réessayer dans quelques instants."</p>
        <a class="button secondary" href="/admin/users">"Retour à l'administration"</a>
    };
    axum::response::Html(shell("Service indisponible", &body.to_html()))
}

pub(crate) fn user_not_found_page() -> axum::response::Html<String> {
    let body = view! {
        <h1>"Utilisateur introuvable"</h1>
        <p>"Ce compte n'existe pas ou a déjà été désactivé."</p>
        <a class="button secondary" href="/admin/users">"Retour à la liste des utilisateurs"</a>
    };
    axum::response::Html(shell("Utilisateur introuvable", &body.to_html()))
}

pub(crate) fn forbidden_page() -> axum::response::Html<String> {
    let body = view! {
        <h1>"Action non autorisée"</h1>
        <p>"Vous n'avez pas les droits nécessaires pour cette action."</p>
        <a class="button secondary" href="/admin/users">"Retour à l'administration"</a>
    };
    axum::response::Html(shell("Action non autorisée", &body.to_html()))
}
