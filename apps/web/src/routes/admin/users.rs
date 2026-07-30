//! `/admin/users` — a read-only table of every account (id/email/verified/
//! created/status), each linking to `/admin/users/:id`, the detail + deactivate
//! screen. `deactivate` is the immediate support action (revokes every session
//! and sets `deleted_at`), distinct from the self-service grace-period deletion
//! (F10) — the copy keeps them apart. The backend has no single-user GET, so the
//! detail page finds its user in the same `/admin/users` list the table renders
//! (mirrors how Messagerie derives one message from the paginated list). See
//! `docs/front-epic-9-user-admin.md`.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use manage_our_home_shared::dto::user_admin::{AdminUserResponse, AdminUsersResponse};
use uuid::Uuid;

use crate::app::{html_escape, shell};
use crate::layout::CurrentSuperAdmin;
use crate::state::{api_request_auth, AppState};

use super::{
    admin_cookie, admin_header, can_deactivate, forbidden_page, format_admin_datetime,
    format_admin_datetime_opt, service_unavailable_page, user_not_found_page, user_status_label,
};

#[derive(serde::Deserialize)]
pub struct ListQuery {
    notice: Option<String>,
    error: Option<String>,
}

fn notice_html(notice: Option<&str>) -> String {
    let text = match notice {
        Some("user_deactivated") => "Compte désactivé : toutes les sessions ont été révoquées.",
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
        status = html_escape(user_status_label(
            user.deleted_at,
            user.deletion_requested_at
        )),
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
        r#"{header}
<h1>Administration — Utilisateurs</h1>
<p class="muted">Tous les comptes, tous foyers confondus. Vue de support en lecture seule (hors désactivation).</p>
<nav class="actions"><a href="/admin/groups">Familles</a><a href="/admin/users">Utilisateurs</a></nav>
{notice}{error}
{table}"#,
    );
    Html(shell("Administration — Utilisateurs", &body)).into_response()
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

    let status = user_status_label(user.deleted_at, user.deletion_requested_at);

    // The confirm form renders only while the account is still active; once
    // deactivated, the backend 404s a second attempt, so we show a note instead.
    let action = if can_deactivate(user.deleted_at) {
        format!(
            r#"<section class="card">
<h2>Désactiver ce compte</h2>
<p class="muted">Action immédiate de support : révoque toutes les sessions actives et marque le compte comme supprimé. À distinguer de la suppression de compte en libre-service (avec délai de grâce) demandée par l'utilisateur.</p>
<form method="post" action="/admin/users/{id}/deactivate" onsubmit="return confirm('Désactiver définitivement ce compte ? Toutes les sessions seront révoquées.');">
<button type="submit" class="danger">Désactiver le compte</button>
</form>
</section>"#,
            id = user.id,
        )
    } else {
        r#"<p class="muted">Ce compte est déjà désactivé — aucune action possible.</p>"#.to_string()
    };

    let body = format!(
        r#"{header}
<p><a href="/admin/users">← Retour à la liste des utilisateurs</a></p>
<h1>{email}</h1>
<dl>
<dt>Identifiant</dt><dd><code>{id}</code></dd>
<dt>Email vérifié</dt><dd>{verified}</dd>
<dt>Inscrit le</dt><dd>{created}</dd>
<dt>Statut</dt><dd>{status}</dd>
<dt>Suppression demandée le</dt><dd>{requested}</dd>
<dt>Désactivé le</dt><dd>{deleted}</dd>
</dl>
{action}"#,
        email = html_escape(&user.email),
        id = html_escape(&user.id.to_string()),
        verified = verified_label(user.email_verified),
        created = html_escape(&format_admin_datetime(user.created_at)),
        status = html_escape(status),
        requested = html_escape(&format_admin_datetime_opt(user.deletion_requested_at)),
        deleted = html_escape(&format_admin_datetime_opt(user.deleted_at)),
    );
    Html(shell(&format!("Utilisateur — {}", user.email), &body)).into_response()
}

/// `POST /admin/users/:id/deactivate` — relays to
/// `POST /admin/users/:id/deactivate` on apps/api (revokes sessions + sets
/// `deleted_at` + audit row, all server-side). 204 → PRG to the list with a
/// success banner; 404 → not-found page (unknown or already deactivated); 403 →
/// forbidden (unreachable once gated; defensive); any other status → the list
/// with a service-unavailable banner; a transport error → the service page.
pub async fn deactivate(
    CurrentSuperAdmin(_me): CurrentSuperAdmin,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
) -> Response {
    let cookie = admin_cookie(&headers);
    match api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/admin/users/{user_id}/deactivate"),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            Redirect::to("/admin/users?notice=user_deactivated").into_response()
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
