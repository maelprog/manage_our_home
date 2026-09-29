//! `/admin/users` — a read-only table of every account (id/email/verified/
//! created/status), each linking to `/admin/users/:id`, the detail +
//! deactivate/reactivate screen. `deactivate` is the immediate support action
//! (revokes every session and sets `deactivated_at`; the purge takes the
//! account 2 years later unless `reactivate` gives it back first, #256),
//! distinct from the self-service grace-period deletion (F10) — the copy keeps
//! them apart. A holder's pending reactivation request (#289) shows in the
//! status and on the detail screen, with its note, where it is granted by
//! `reactivate` or turned down by `refuse_reactivation`. The backend has no single-user GET, so the
//! detail page finds its user in the same `/admin/users` list the table renders
//! (mirrors how Messagerie derives one message from the paginated list). See
//! `docs/front-epic-9-user-admin.md`.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use manage_our_home_shared::dto::user_admin::{AdminUserResponse, AdminUsersResponse};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentSuperAdmin;
use crate::state::{api_request_auth, AppState};

use super::{
    admin_cookie, admin_header, can_deactivate, can_reactivate, can_refuse_reactivation,
    forbidden_page, format_admin_datetime, format_admin_datetime_opt, purge_outlook,
    service_unavailable_page, user_not_found_page, user_status_label, PurgeOutlook,
};

/// Inline `onsubmit`s of the deactivate and refuse forms. Named constants
/// because infra/Caddyfile's CSP allows them by hash (`csp.rs`).
pub(crate) const CONFIRM_DEACTIVATE: &str =
    "return confirm('Désactiver ce compte ? Toutes les sessions seront révoquées.');";
pub(crate) const CONFIRM_REFUSE_REACTIVATION: &str =
    "return confirm('Refuser cette demande ? Le compte reste désactivé.');";

#[derive(serde::Deserialize)]
pub struct ListQuery {
    notice: Option<String>,
    error: Option<String>,
}

fn notice_html(notice: Option<&str>) -> String {
    let text = match notice {
        Some("user_deactivated") => "Compte désactivé : toutes les sessions ont été révoquées.",
        Some("user_reactivated") => "Compte réactivé : son titulaire peut de nouveau se connecter.",
        Some("reactivation_refused") => {
            "Demande de réactivation refusée : le compte reste désactivé."
        }
        _ => return String::new(),
    };
    format!(r#"<p class="notice success">{}</p>"#, html_escape(text))
}

fn error_html(error: Option<&str>) -> String {
    let text = match error {
        Some("unavailable") => "Service momentanément indisponible, merci de réessayer.",
        _ => return String::new(),
    };
    format!(r#"<p class="notice error">{}</p>"#, html_escape(text))
}

fn verified_label(verified: bool) -> &'static str {
    if verified {
        "Oui"
    } else {
        "Non"
    }
}

/// The status cell and line: [`user_status_label`], plus the pending
/// reactivation request when there is one (#289).
fn status_text(user: &AdminUserResponse) -> String {
    let label = user_status_label(
        user.deleted_at,
        user.deactivated_at,
        user.deletion_requested_at,
    );
    if can_refuse_reactivation(
        user.deleted_at,
        user.deactivated_at,
        user.reactivation_requested_at,
    ) {
        format!("{label} — réactivation demandée")
    } else {
        label.to_string()
    }
}

fn user_row(user: &AdminUserResponse) -> String {
    format!(
        r#"<tr>
<td>{email}</td>
<td>{verified}</td>
<td>{created}</td>
<td>{status}</td>
<td><a href="/admin/users/{id}">Détails</a></td>
</tr>"#,
        email = html_escape(&user.email),
        verified = verified_label(user.email_verified),
        created = html_escape(&format_admin_datetime(user.created_at)),
        status = html_escape(&status_text(user)),
        id = user.id,
    )
}

/// Fetches the full account list. `Ok(None)` means a transport failure (caller
/// renders the service-unavailable page); any non-200 degrades to an empty list
/// rather than leaking JSON (the route is already superadmin-gated).
async fn fetch_users(state: &AppState, cookie: Option<&str>) -> Result<Vec<AdminUserResponse>, ()> {
    match api_request_auth(state, reqwest::Method::GET, "/admin/users", cookie, None).await {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            Ok(serde_json::from_value::<AdminUsersResponse>(resp.body)
                .map(|r| r.users)
                .unwrap_or_default())
        }
        Ok(_) => Ok(Vec::new()),
        Err(_) => Err(()),
    }
}

pub async fn get(
    CurrentSuperAdmin(me): CurrentSuperAdmin,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    let cookie = admin_cookie(&headers);
    let header = admin_header(&state, &headers, &me, "/admin/users").await;

    let users = match fetch_users(&state, cookie.as_deref()).await {
        Ok(users) => users,
        Err(()) => return service_unavailable_page().into_response(),
    };

    let notice = notice_html(query.notice.as_deref());
    let error = error_html(query.error.as_deref());

    let table = if users.is_empty() {
        r#"<p class="muted">Aucun utilisateur pour le moment.</p>"#.to_string()
    } else {
        let rows = users.iter().map(user_row).collect::<String>();
        format!(
            r#"<div class="table-wrap"><table>
<thead><tr><th>Email</th><th>Email vérifié</th><th>Inscrit le</th><th>Statut</th><th></th></tr></thead>
<tbody>{rows}</tbody>
</table></div>"#
        )
    };

    let body = format!(
        r#"<h1>Administration — Utilisateurs</h1>
<p class="muted">Tous les comptes, tous foyers confondus. Vue de support en lecture seule (hors désactivation et réactivation).</p>
<nav class="actions"><a href="/admin/groups">Familles</a><a href="/admin/users">Utilisateurs</a></nav>
{notice}{error}
{table}"#,
    );
    Html(shell_with_header(
        Width::Full,
        "Administration — Utilisateurs",
        &header,
        &body,
    ))
    .into_response()
}

/// `GET /admin/users/:id` — one account's detail plus, when it is still active,
/// the deactivate confirmation form. The user is found in the same list the
/// table renders (no single-user API); an unknown id → the not-found page.
pub async fn detail(
    CurrentSuperAdmin(me): CurrentSuperAdmin,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
) -> Response {
    let cookie = admin_cookie(&headers);
    let header = admin_header(&state, &headers, &me, &format!("/admin/users/{user_id}")).await;

    let users = match fetch_users(&state, cookie.as_deref()).await {
        Ok(users) => users,
        Err(()) => return service_unavailable_page().into_response(),
    };
    let Some(user) = users.into_iter().find(|u| u.id == user_id) else {
        return user_not_found_page().into_response();
    };

    let status = status_text(&user);

    // Each form renders only when the backend would accept it (#256): the
    // deactivate one for an active account, the reactivate one for a
    // deactivated account the purge has not taken yet. A purged account
    // gets a note instead.
    let action = if can_deactivate(user.deleted_at, user.deactivated_at) {
        format!(
            r#"<section class="card">
<h2>Désactiver ce compte</h2>
<p class="muted">Action immédiate de support : révoque toutes les sessions actives ; son titulaire ne peut plus ouvrir qu'une page d'où demander la réactivation. Rien n'est effacé ; le compte peut être réactivé, et sans réactivation il est purgé au bout de 2 ans, son titulaire prévenu par email 30 jours avant. À distinguer de la suppression de compte en libre-service (avec délai de grâce) demandée par l'utilisateur.</p>
<form method="post" action="/admin/users/{id}/deactivate" onsubmit="{CONFIRM_DEACTIVATE}">
<button type="submit" class="danger">Désactiver le compte</button>
</form>
</section>"#,
            id = user.id,
        )
    } else if can_refuse_reactivation(
        user.deleted_at,
        user.deactivated_at,
        user.reactivation_requested_at,
    ) {
        // #289: the holder asked. Reactivating grants the request; refusing
        // leaves the account deactivated and lets the purge clock run again.
        let message = match user.reactivation_message.as_deref() {
            Some(m) => format!(r#"<p class="multiline">{}</p>"#, html_escape(m)),
            None => r#"<p class="muted">Aucun message.</p>"#.to_string(),
        };
        format!(
            r#"<section class="card">
<h2>Demande de réactivation</h2>
<p>Le titulaire a demandé la réactivation de son compte le {requested}. {purge}</p>
{message}
<div class="actions">
<form method="post" action="/admin/users/{id}/reactivate">
<button type="submit">Réactiver le compte</button>
</form>
<form method="post" action="/admin/users/{id}/reactivation-request/refuse" onsubmit="{CONFIRM_REFUSE_REACTIVATION}">
<button type="submit" class="danger">Refuser la demande</button>
</form>
</div>
</section>"#,
            requested = html_escape(&format_admin_datetime_opt(user.reactivation_requested_at)),
            purge = html_escape(
                match purge_outlook(
                    user.deletion_requested_at,
                    user.reactivation_requested_at,
                    user.reactivation_refused_at,
                ) {
                    PurgeOutlook::DeletionRequested { .. } => {
                        "Sa suppression, demandée avant, a lieu 30 jours après sa demande : la demande de réactivation ne la suspend pas."
                    }
                    PurgeOutlook::Suspended => {
                        "Tant que la demande est en attente, la purge au bout de 2 ans de désactivation est suspendue."
                    }
                    PurgeOutlook::Runs { .. } => {
                        "Une demande a déjà été refusée depuis la désactivation : celle-ci ne suspend pas la purge au bout de 2 ans."
                    }
                }
            ),
            id = user.id,
        )
    } else if can_reactivate(user.deleted_at, user.deactivated_at) {
        format!(
            r#"<section class="card">
<h2>Réactiver ce compte</h2>
<p class="muted">Le compte est désactivé. Le réactiver rouvre la connexion (les sessions révoquées le restent) et annule la purge prévue au bout de 2 ans.</p>
<form method="post" action="/admin/users/{id}/reactivate">
<button type="submit">Réactiver le compte</button>
</form>
</section>"#,
            id = user.id,
        )
    } else {
        r#"<p class="muted">Ce compte a été purgé — aucune action possible.</p>"#.to_string()
    };

    let body = format!(
        r#"<p><a href="/admin/users">← Retour à la liste des utilisateurs</a></p>
<h1>{email}</h1>
<dl>
<dt>Identifiant</dt><dd><code>{id}</code></dd>
<dt>Email vérifié</dt><dd>{verified}</dd>
<dt>Inscrit le</dt><dd>{created}</dd>
<dt>Statut</dt><dd>{status}</dd>
<dt>Suppression demandée le</dt><dd>{requested}</dd>
<dt>Désactivé le</dt><dd>{deactivated}</dd>
<dt>Réactivation refusée le</dt><dd>{refused}</dd>
<dt>Purgé le</dt><dd>{deleted}</dd>
</dl>
{action}"#,
        email = html_escape(&user.email),
        id = html_escape(&user.id.to_string()),
        verified = verified_label(user.email_verified),
        created = html_escape(&format_admin_datetime(user.created_at)),
        status = html_escape(&status),
        requested = html_escape(&format_admin_datetime_opt(user.deletion_requested_at)),
        deactivated = html_escape(&format_admin_datetime_opt(user.deactivated_at)),
        refused = html_escape(&format_admin_datetime_opt(user.reactivation_refused_at)),
        deleted = html_escape(&format_admin_datetime_opt(user.deleted_at)),
    );
    Html(shell_with_header(
        Width::Read,
        &format!("Utilisateur — {}", user.email),
        &header,
        &body,
    ))
    .into_response()
}

/// `POST /admin/users/:id/deactivate` — relays to
/// `POST /admin/users/:id/deactivate` on apps/api (revokes sessions + sets
/// `deactivated_at` + audit row, all server-side). 204 → PRG to the list with
/// a success banner; 404 → not-found page (unknown, already deactivated or
/// purged); 403 → forbidden (unreachable once gated; defensive); any other
/// status → the list with a service-unavailable banner; a transport error →
/// the service page.
pub async fn deactivate(
    CurrentSuperAdmin(_me): CurrentSuperAdmin,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
) -> Response {
    relay_action(&state, &headers, user_id, "deactivate", "user_deactivated").await
}

/// `POST /admin/users/:id/reactivate` — relays to the same apps/api route
/// (clears `deactivated_at`, grants a pending request + audit row, #256,
/// #289), with the same status mapping as [`deactivate`]; a 404 there means
/// unknown, not deactivated, or purged.
pub async fn reactivate(
    CurrentSuperAdmin(_me): CurrentSuperAdmin,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
) -> Response {
    relay_action(&state, &headers, user_id, "reactivate", "user_reactivated").await
}

/// `POST /admin/users/:id/reactivation-request/refuse` — relays to the same
/// apps/api route (deletes the pending request + audit row, #289), with the
/// same status mapping as [`deactivate`]; a 404 there means no pending
/// request, or an account reactivated or purged since.
pub async fn refuse_reactivation(
    CurrentSuperAdmin(_me): CurrentSuperAdmin,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
) -> Response {
    relay_action(
        &state,
        &headers,
        user_id,
        "reactivation-request/refuse",
        "reactivation_refused",
    )
    .await
}

async fn relay_action(
    state: &AppState,
    headers: &HeaderMap,
    user_id: Uuid,
    action: &str,
    notice: &str,
) -> Response {
    let cookie = admin_cookie(headers);
    match api_request_auth(
        state,
        reqwest::Method::POST,
        &format!("/admin/users/{user_id}/{action}"),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            Redirect::to(&format!("/admin/users?notice={notice}")).into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            user_not_found_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            forbidden_page().into_response()
        }
        Ok(_) => Redirect::to("/admin/users?error=unavailable").into_response(),
        Err(_) => service_unavailable_page().into_response(),
    }
}
