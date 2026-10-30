//! RGPD self-service screens (front epic F10, issue #25): the account hub
//! (`/account`), the data-export download (`/account/export`, Art. 20
//! portability) and the account-deletion flow (`/account/delete`, Art. 17
//! erasure) — plus, outside this module because they must stay reachable without
//! a session, the public legal documents (`crate::routes::privacy` and
//! `crate::routes::legal`).
//!
//! Same SSR pattern as the other epics: plain `<form method=post>`, PRG with
//! `?notice=`/`?error=` codes, per-page error tables mapping the exact status
//! codes of `apps/api`'s `delete_account` / `cancel_delete_account` / `rgpd`
//! handlers to French copy, pure logic mirrored in
//! `manage_our_home_shared::validation::rgpd` and TDD'd. Full spec in
//! `docs/front-epic-10-rgpd.md`.
//!
//! Independent of any family/group context (these are account-level screens),
//! but the deletion flow is where the Groups epic resurfaces: the backend blocks
//! erasure while the caller still owns a group (409 `owner_of_groups`), and this
//! module turns that into actionable guidance pointing at each blocking group's
//! settings screen rather than a raw error.
//!
//! Self-service deletion is a **grace-period** flow (`deletion_requested_at`,
//! purged 30 days later by `apps/api/src/jobs/account_purge.rs`) and is
//! deliberately *not* the superadmin's immediate `deactivate` action (F9) — the
//! copy on both sides keeps them apart.

pub mod age;
pub mod appearance;
pub mod avatar;
pub mod deactivated;
pub mod delete;
pub mod export;
pub mod notifications;
pub mod profile;
pub mod sessions;
pub mod terms;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};
use manage_our_home_shared::dto::auth::MeResponse;

use crate::app::{html_escape, shell, shell_with_header, Width};
use crate::family::{active_group_id_from_headers, resolve_active_group};
use crate::layout::CurrentUser;
use crate::routes::groups::members::fetch_group_detail;
use crate::routes::groups::{cookie_of, header_with_groups};
use crate::state::AppState;

// Re-export the pure, TDD'd helpers from `apps/shared` so the submodules import
// them from the local module like the other epics do.
pub(crate) use manage_our_home_shared::validation::rgpd::{
    can_cancel_deletion, can_request_deletion, deletion_deadline, export_filename,
    format_rgpd_date, format_rgpd_datetime, validate_deletion_confirmation, GRACE_PERIOD_DAYS,
};

/// The caller's session cookie, forwarded to the authenticated `/account/*` API
/// calls (re-exported so the submodules don't reach into `routes::groups`).
pub(crate) fn account_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_of(headers)
}

/// Renders the shared authenticated header (nav + family switcher) for an
/// account page, so these screens sit inside the app like any other.
pub(crate) async fn account_header(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    redirect_to: &str,
) -> String {
    let (_groups, header) = header_with_groups(state, headers, me, redirect_to).await;
    header
}

/// The pending-deletion panel, shown on `/account` **and** `/account/delete` so
/// the state is never hidden behind one route: when the request was made, when
/// the purge happens, and the cancel action. Rendered from
/// `MeResponse::deletion_requested_at`, which `GET /auth/me` reads live, so it
/// appears/disappears on the next page load after requesting/cancelling.
pub(crate) fn pending_deletion_panel(me: &MeResponse) -> String {
    let Some(requested_at) = me.deletion_requested_at else {
        return String::new();
    };
    format!(
        r#"<section class="notice error">
<h2>Suppression de compte programmée</h2>
<p>Demandée le {requested}. Sauf annulation, vos identifiants, vos appartenances aux familles et vos autres données personnelles seront supprimés et votre compte anonymisé le <strong>{deadline}</strong> (délai de grâce de {days} jours).</p>
<p>Votre compte reste utilisable normalement jusque-là, et cette demande est annulable à tout moment.</p>
<form method="post" action="/account/delete/cancel">
<button type="submit" class="secondary">Annuler la demande de suppression</button>
</form>
</section>"#,
        requested = html_escape(&format_rgpd_datetime(requested_at)),
        deadline = html_escape(&format_rgpd_date(deletion_deadline(requested_at))),
        days = GRACE_PERIOD_DAYS,
    )
}

#[derive(serde::Deserialize)]
pub struct HubQuery {
    notice: Option<String>,
    error: Option<String>,
}

fn notice_html(notice: Option<&str>) -> String {
    let text = match notice {
        Some("deletion_requested") => {
            "Demande de suppression enregistrée. Vous pouvez encore l'annuler ci-dessous."
        }
        Some("deletion_cancelled") => "Demande de suppression annulée : votre compte reste actif.",
        Some(code) => match avatar::notice_message(code) {
            Some(text) => text,
            None => return String::new(),
        },
        None => return String::new(),
    };
    format!(r#"<p class="notice success">{}</p>"#, html_escape(text))
}

fn error_html(error: Option<&str>) -> String {
    let text = match error {
        Some("no_pending_deletion") => {
            "Aucune demande de suppression en cours : il n'y a rien à annuler."
        }
        Some("unavailable") => "Service momentanément indisponible, merci de réessayer.",
        Some(code) => match avatar::error_message(code) {
            Some(text) => text,
            None => return String::new(),
        },
        None => return String::new(),
    };
    format!(r#"<p class="notice error">{}</p>"#, html_escape(text))
}

/// `GET /account` — the profile (#529): who you are, your families and the
/// active one, the profile photo, then the account's settings — the RGPD
/// export and deletion among them, so both rights stay two clicks from any
/// page ("Mon compte", then the row). A pending deletion shows its panel at
/// the top instead of the deletion row. The three public legal documents are
/// not listed here: every page links them from the shell's footer since #419.
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HubQuery>,
) -> Response {
    let cookie = account_cookie(&headers);
    let (groups, header) = header_with_groups(&state, &headers, &me, "/account").await;
    let has_photo = avatar::has_own_photo(&state, cookie.as_deref()).await;
    let active =
        resolve_active_group(&groups, active_group_id_from_headers(&headers)).map(|g| g.group_id);
    // One `GET /groups/:id` per family for its member count: a person
    // belongs to a handful at most, and a failed read shows the role alone.
    let mut families = Vec::with_capacity(groups.len());
    for g in groups {
        let members = fetch_group_detail(&state, cookie.as_deref(), g.group_id)
            .await
            .ok()
            .flatten()
            .map(|detail| detail.members.len());
        families.push((g, members));
    }

    let body = format!(
        r#"{head}
{notice}{error}{terms}{pending}
{families}
{photo}
{settings}"#,
        head = profile::profile_head(&me, has_photo),
        notice = notice_html(query.notice.as_deref()),
        error = error_html(query.error.as_deref()),
        // #319: a new version of the CGU is announced here as on the home
        // page, until acknowledged.
        terms = terms::update_notice(&me),
        pending = pending_deletion_panel(&me),
        families = profile::families_section(&families, active),
        // #468: the profile photo.
        photo = avatar::section(&me, has_photo),
        settings = profile::settings_section(&me, crate::theme::current()),
    );
    Html(shell_with_header(Width::Read, "Mon compte", &header, &body)).into_response()
}

// -- shared error page ------------------------------------------------------

pub(crate) fn service_unavailable_page() -> Html<String> {
    let body = r#"<h1>Service momentanément indisponible</h1>
<p>Merci de réessayer dans quelques instants.</p>
<a class="btn secondary" href="/account">Retour à mon compte</a>"#;
    Html(shell(Width::Form, "Service indisponible", body))
}
