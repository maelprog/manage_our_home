//! Messagerie screens (front epic F8, issue #23): the family's one chat thread
//! (`/messagerie`) — a live view with a composer and inline edit/delete behind
//! a permission bar, plus a windowed history page reached by a cursor URL. Same
//! SSR pattern as `routes/grocery_list/*` and `routes/budget/*` — plain
//! `<form method=post>` submissions, per-page error tables mapping
//! `apps/api/src/messagerie/`'s exact status/error codes to French copy, PRG
//! (`?notice=`) after every mutation. Full spec (route table, error tables, and
//! the WS-signal / no-JS-baseline / reconnect / pagination decisions) in
//! `docs/front-epic-8-messagerie.md`.
//!
//! The one thing no other front epic has: a **WebSocket**. It is push-only and
//! only an enhancement — with JS off, the thread is what the last page load
//! rendered and a reload shows new messages. When JS is on, the inline script
//! opens `GET /groups/:id/messages/ws` **against `apps/api` directly** (browser
//! → API, via `API_PUBLIC_BASE_URL`), treats every frame as a bare "something
//! changed" signal, and re-fetches the current URL to swap `#thread`'s HTML with
//! a freshly server-rendered fragment — so the escaping, the permission bar and
//! the copy have exactly one implementation (Rust), and the socket needs no
//! client-side model of a message.
//!
//! Permission bar (mirrors `apps/api/src/messagerie/mod.rs::can_modify`): any
//! member may read the thread, post, and open the socket, so those render for
//! everyone; a row's `Modifier`/`Supprimer` controls render only for the
//! message's author or a group admin/owner. The backend stays the authority (a
//! forged edit/delete is still 403'd, mapped defensively here).

pub mod thread;

use axum::http::HeaderMap;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::GroupSummary;
use uuid::Uuid;

use crate::app::{shell, Width};
use crate::family::{active_group_id_from_headers, resolve_active_group};
use crate::routes::groups::{cookie_of, header_with_groups};
use crate::state::AppState;

// The permission mirror lives in `apps/shared` (pure, TDD'd) alongside the rest
// of the messagerie logic; re-export it so the submodule imports it from the
// local module like the Grocery-list/Budget pages do.
pub(crate) use manage_our_home_shared::validation::messagerie::can_modify;

/// Resolved active-family context for a Messagerie page: the family id every
/// `/groups/:gid/…` API call is scoped to, the caller's role (for the
/// permission bar), their own user id (to decide `is_author`), and the shared
/// authenticated header. `None` means the user has no group yet — callers
/// redirect to `/groups/new`.
pub(crate) struct FamilyContext {
    pub gid: Uuid,
    pub role: String,
    pub user_id: Uuid,
    pub header: String,
    /// The browser-facing API base URL (`API_PUBLIC_BASE_URL`), used to build
    /// the WebSocket URL the inline script opens — the browser can't reach the
    /// internal one. See `docs/front-epic-8-messagerie.md`.
    pub api_public_base_url: String,
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
        user_id: me.user_id,
        header,
        api_public_base_url: state.api_public_base_url.clone(),
    })
}

/// Fetches `cookie_of` for a Messagerie handler (re-exported so the submodule
/// doesn't reach into `routes::groups`).
pub(crate) fn messagerie_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_of(headers)
}

// -- shared error/landing pages ---------------------------------------------

pub(crate) fn message_not_found_page() -> axum::response::Html<String> {
    let body = view! {
        <h1>"Message introuvable"</h1>
        <p>"Ce message n'existe pas ou plus (il a peut-être été supprimé depuis un autre appareil)."</p>
        <a class="btn secondary" href="/messagerie">"Retour à la messagerie"</a>
    };
    axum::response::Html(shell(Width::Form, "Message introuvable", &body.to_html()))
}

pub(crate) fn service_unavailable_page() -> axum::response::Html<String> {
    let body = view! {
        <h1>"Service momentanément indisponible"</h1>
        <p>"Merci de réessayer dans quelques instants."</p>
        <a class="btn secondary" href="/messagerie">"Retour à la messagerie"</a>
    };
    axum::response::Html(shell(Width::Form, "Service indisponible", &body.to_html()))
}

pub(crate) fn forbidden_page() -> axum::response::Html<String> {
    let body = view! {
        <h1>"Action non autorisée"</h1>
        <p>"Vous n'avez pas les droits nécessaires sur ce message."</p>
        <a class="btn secondary" href="/messagerie">"Retour à la messagerie"</a>
    };
    axum::response::Html(shell(Width::Form, "Action non autorisée", &body.to_html()))
}
