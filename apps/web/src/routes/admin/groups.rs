//! `GET /admin/groups` — a read-only table of every family across every tenant
//! (id, name, creation date, member count), for support look-up when a user
//! reports an issue. No mutations here, so no PRG/notice. See
//! `docs/front-epic-9-user-admin.md`.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};
use manage_our_home_shared::dto::user_admin::{AdminGroupResponse, AdminGroupsResponse};

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentSuperAdmin;
use crate::state::{api_request_auth, AppState};

use super::{admin_cookie, admin_header, format_admin_datetime, service_unavailable_page};

fn group_row(group: &AdminGroupResponse) -> String {
    format!(
        r#"<tr>
<td><code>{id}</code></td>
<td>{name}</td>
<td>{created}</td>
<td style="text-align:right;">{count}</td>
</tr>"#,
        id = html_escape(&group.id.to_string()),
        name = html_escape(&group.name),
        created = html_escape(&format_admin_datetime(group.created_at)),
        count = group.member_count,
    )
}

pub async fn get(
    CurrentSuperAdmin(me): CurrentSuperAdmin,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let cookie = admin_cookie(&headers);
    let header = admin_header(&state, &headers, &me, "/admin/groups").await;

    // A transport error takes down the page; any non-200 renders an empty table
    // rather than leaking the JSON body (the route is already superadmin-gated,
    // so a 403 here is unreachable — defensive).
    let groups: Vec<AdminGroupResponse> = match api_request_auth(
        &state,
        reqwest::Method::GET,
        "/admin/groups",
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<AdminGroupsResponse>(resp.body)
                .map(|r| r.groups)
                .unwrap_or_default()
        }
        Ok(_) => Vec::new(),
        Err(_) => return service_unavailable_page().into_response(),
    };

    let table = if groups.is_empty() {
        r#"<p class="muted">Aucune famille pour le moment.</p>"#.to_string()
    } else {
        let rows = groups.iter().map(group_row).collect::<String>();
        format!(
            r#"<div class="table-wrap"><table>
<thead><tr><th>Identifiant</th><th>Nom</th><th>Créée le</th><th style="text-align:right;">Membres</th></tr></thead>
<tbody>{rows}</tbody>
</table></div>"#
        )
    };

    let body = format!(
        r#"<h1>Administration — Familles</h1>
<p class="muted">Toutes les familles, tous foyers confondus. Vue de support en lecture seule.</p>
<nav class="actions"><a href="/admin/groups">Familles</a><a href="/admin/users">Utilisateurs</a></nav>
{table}"#,
    );
    Html(shell_with_header(
        Width::Full,
        "Administration — Familles",
        &header,
        &body,
    ))
    .into_response()
}
