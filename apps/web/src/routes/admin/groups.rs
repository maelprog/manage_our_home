//! `GET /admin/groups` — a table of every family across every tenant (id,
//! name, creation date, member count, whether it has an owner), for support
//! look-up when a user reports an issue. See `docs/front-epic-9-user-admin.md`.
//!
//! A group the account purge left without an owner (#323) links to
//! `/admin/groups/:id`, which lists its members — apps/api answers only for
//! such a group — and lets the superadmin designate one of the active ones
//! (`POST /admin/groups/:id/owner`). Same PRG as `/admin/users`.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::user_admin::{
    AdminGroupMember, AdminGroupMembersResponse, AdminGroupResponse, AdminGroupsResponse,
    DesignateOwnerRequest,
};
use manage_our_home_shared::validation::user_admin::can_be_designated_owner;
use uuid::Uuid;

use crate::app::{html_escape, shell, shell_with_header, Width};
use crate::layout::CurrentSuperAdmin;
use crate::routes::groups::role_label;
use crate::state::{api_request_auth, AppState};

use super::{
    admin_cookie, admin_header, format_admin_datetime, reauthenticate, service_unavailable_page,
};

#[derive(serde::Deserialize)]
pub struct PageQuery {
    notice: Option<String>,
    error: Option<String>,
}

/// The banner for a `?notice=` or `?error=` code; empty for none or an
/// unknown one.
fn banner(query: &PageQuery) -> String {
    let (class, text) = match (query.notice.as_deref(), query.error.as_deref()) {
        (Some("owner_designated"), _) => (
            "success",
            "Propriétaire désigné : le membre en est averti par email et à sa prochaine connexion.",
        ),
        (_, Some("group_has_owner")) => (
            "error",
            "Ce groupe a déjà un propriétaire : il n'y a personne à désigner.",
        ),
        (_, Some("owner_must_be_active")) => (
            "error",
            "Seul un membre actif peut être désigné : ce compte est désactivé, ou sa suppression est demandée.",
        ),
        (_, Some("not_a_member")) => ("error", "Ce compte n'est pas membre du groupe."),
        (_, Some("unavailable")) => (
            "error",
            "Service momentanément indisponible, merci de réessayer.",
        ),
        _ => return String::new(),
    };
    format!(r#"<p class="notice {class}">{}</p>"#, html_escape(text))
}

/// The "Propriétaire" cell: a group without an owner says so and links to
/// the screen that designates one.
fn owner_cell(group: &AdminGroupResponse) -> String {
    if group.has_owner {
        "Oui".to_string()
    } else {
        format!(
            r#"<a href="/admin/groups/{id}">Aucun — désigner</a>"#,
            id = group.id
        )
    }
}

fn group_row(group: &AdminGroupResponse) -> String {
    format!(
        r#"<tr>
<td><code>{id}</code></td>
<td>{name}</td>
<td>{created}</td>
<td style="text-align:right;">{count}</td>
<td>{owner}</td>
</tr>"#,
        id = html_escape(&group.id.to_string()),
        name = html_escape(&group.name),
        created = html_escape(&format_admin_datetime(group.created_at)),
        count = group.member_count,
        owner = owner_cell(group),
    )
}

/// One member of a group without an owner, with the form that designates
/// them — only for an active member ([`can_be_designated_owner`]); the
/// others say why not.
fn member_row(group_id: Uuid, member: &AdminGroupMember) -> String {
    let action = if can_be_designated_owner(member.deactivated, member.pending_deletion) {
        format!(
            r#"<form method="post" action="/admin/groups/{group_id}/owner">
<input type="hidden" name="user_id" value="{user_id}"/>
<button type="submit" class="secondary sm">Désigner propriétaire</button>
</form>"#,
            user_id = member.user_id,
        )
    } else if member.deactivated {
        r#"<span class="muted">Compte désactivé</span>"#.to_string()
    } else {
        r#"<span class="muted">Compte en suppression demandée</span>"#.to_string()
    };
    format!(
        r#"<tr>
<td>{name}</td>
<td>{email}</td>
<td>{role}</td>
<td>{joined}</td>
<td>{action}</td>
</tr>"#,
        name = html_escape(&member.display_name),
        email = html_escape(&member.email),
        role = role_label(&member.role),
        joined = html_escape(&format_admin_datetime(member.joined_at)),
    )
}

pub async fn get(
    CurrentSuperAdmin(me): CurrentSuperAdmin,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Response {
    let cookie = admin_cookie(&headers);
    let header = admin_header(&state, &headers, &me, "/admin/groups").await;

    // A transport error takes down the page; a 401 is a session too old for
    // the admin routes (#226), sent to log in again; any other non-200
    // renders an empty table rather than leaking the JSON body (the route is
    // already superadmin-gated, so a 403 here is unreachable — defensive).
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
        Ok(resp) if resp.status == reqwest::StatusCode::UNAUTHORIZED => {
            return reauthenticate(&state, &headers).await;
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
<thead><tr><th scope="col">Identifiant</th><th scope="col">Nom</th><th scope="col">Créée le</th><th scope="col" style="text-align:right;">Membres</th><th scope="col">Propriétaire</th></tr></thead>
<tbody>{rows}</tbody>
</table></div>"#
        )
    };

    let body = format!(
        r#"<h1>Administration — Familles</h1>
{banner}<p class="muted">Toutes les familles, tous foyers confondus. Vue de support ; seule action : désigner un propriétaire à une famille qui n'en a plus.</p>
<nav class="actions"><a href="/admin/groups">Familles</a><a href="/admin/users">Utilisateurs</a></nav>
{table}"#,
        banner = banner(&query),
    );
    Html(shell_with_header(
        Width::Full,
        "Administration — Familles",
        &header,
        &body,
    ))
    .into_response()
}

fn group_has_owner_page() -> Html<String> {
    Html(shell(
        Width::Form,
        "Famille avec propriétaire",
        r#"<h1>Cette famille a un propriétaire</h1>
<p>La liste de ses membres n'est ouverte au support que pour une famille restée sans propriétaire.</p>
<a class="btn secondary" href="/admin/groups">Retour aux familles</a>"#,
    ))
}

fn group_not_found_page() -> Html<String> {
    Html(shell(
        Width::Form,
        "Famille introuvable",
        r#"<h1>Famille introuvable</h1>
<p>Cette famille n'existe pas, ou plus.</p>
<a class="btn secondary" href="/admin/groups">Retour aux familles</a>"#,
    ))
}

/// `GET /admin/groups/:id` — the members of a group left without an owner,
/// and the designation form (#323).
pub async fn detail(
    CurrentSuperAdmin(me): CurrentSuperAdmin,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Query(query): Query<PageQuery>,
) -> Response {
    let cookie = admin_cookie(&headers);
    let path = format!("/admin/groups/{group_id}");
    let header = admin_header(&state, &headers, &me, &path).await;

    let group = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/admin/groups/{group_id}/members"),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match serde_json::from_value::<AdminGroupMembersResponse>(resp.body) {
                Ok(group) => group,
                Err(_) => return service_unavailable_page().into_response(),
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNAUTHORIZED => {
            return reauthenticate(&state, &headers).await;
        }
        Ok(resp) if resp.status == reqwest::StatusCode::CONFLICT => {
            return group_has_owner_page().into_response();
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return group_not_found_page().into_response();
        }
        Ok(_) | Err(_) => return service_unavailable_page().into_response(),
    };

    let rows: String = group
        .members
        .iter()
        .map(|m| member_row(group.id, m))
        .collect();
    let members = if group.members.is_empty() {
        r#"<p class="muted">Cette famille n'a plus aucun membre.</p>"#.to_string()
    } else {
        format!(
            r#"<div class="table-wrap"><table>
<thead><tr><th scope="col">Nom</th><th scope="col">Email</th><th scope="col">Rôle</th><th scope="col">Membre depuis</th><th scope="col">Propriétaire</th></tr></thead>
<tbody>{rows}</tbody>
</table></div>"#
        )
    };
    let body = format!(
        r#"<h1>Famille sans propriétaire — {name}</h1>
{banner}<p class="muted">Le propriétaire de cette famille a été supprimé sans qu'aucun membre puisse en hériter. Désignez-en un parmi les membres actifs : il en sera averti par email et à sa prochaine connexion.</p>
<nav class="actions"><a href="/admin/groups">Familles</a><a href="/admin/users">Utilisateurs</a></nav>
{members}"#,
        name = html_escape(&group.name),
        banner = banner(&query),
    );
    Html(shell_with_header(
        Width::Full,
        "Famille sans propriétaire",
        &header,
        &body,
    ))
    .into_response()
}

#[derive(serde::Deserialize)]
pub struct DesignateForm {
    user_id: Uuid,
}

/// `POST /admin/groups/:id/owner` — relays to the same apps/api route, then
/// back to the list with a notice; a refusal comes back to the group's page
/// with its code, or to the list when the group has an owner since.
pub async fn designate_owner(
    CurrentSuperAdmin(_me): CurrentSuperAdmin,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Form(form): Form<DesignateForm>,
) -> Response {
    let cookie = admin_cookie(&headers);
    let body = serde_json::to_value(DesignateOwnerRequest {
        user_id: form.user_id,
    })
    .ok();
    match api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/admin/groups/{group_id}/owner"),
        cookie.as_deref(),
        body,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            Redirect::to("/admin/groups?notice=owner_designated").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNAUTHORIZED => {
            reauthenticate(&state, &headers).await
        }
        Ok(resp) if resp.status == reqwest::StatusCode::CONFLICT => {
            Redirect::to("/admin/groups?error=group_has_owner").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            group_not_found_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
            let code = match resp.body.get("error").and_then(|e| e.as_str()) {
                Some("not_a_member") => "not_a_member",
                _ => "owner_must_be_active",
            };
            Redirect::to(&format!("/admin/groups/{group_id}?error={code}")).into_response()
        }
        Ok(_) => {
            Redirect::to(&format!("/admin/groups/{group_id}?error=unavailable")).into_response()
        }
        Err(_) => service_unavailable_page().into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn group(has_owner: bool) -> AdminGroupResponse {
        AdminGroupResponse {
            id: Uuid::from_u128(7),
            name: "Famille".to_string(),
            created_at: Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap(),
            member_count: 2,
            has_owner,
        }
    }

    fn member(deactivated: bool, pending_deletion: bool) -> AdminGroupMember {
        AdminGroupMember {
            user_id: Uuid::from_u128(9),
            display_name: "<Alice>".to_string(),
            email: "alice@example.test".to_string(),
            role: "admin".to_string(),
            joined_at: Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap(),
            deactivated,
            pending_deletion,
        }
    }

    #[test]
    fn a_group_with_an_owner_offers_nothing_to_designate() {
        let cell = owner_cell(&group(true));
        assert!(cell.contains("Oui"), "{cell}");
        assert!(!cell.contains("href"), "{cell}");
    }

    #[test]
    fn a_group_without_an_owner_links_to_its_designation_screen() {
        let cell = owner_cell(&group(false));
        assert!(cell.contains("Aucun"), "{cell}");
        assert!(
            cell.contains(&format!(r#"href="/admin/groups/{}""#, Uuid::from_u128(7))),
            "{cell}"
        );
    }

    #[test]
    fn an_active_member_gets_the_designation_form() {
        let row = member_row(Uuid::from_u128(7), &member(false, false));
        assert!(
            row.contains(&format!(
                r#"action="/admin/groups/{}/owner""#,
                Uuid::from_u128(7)
            )),
            "{row}"
        );
        assert!(
            row.contains(&format!(r#"name="user_id" value="{}""#, Uuid::from_u128(9))),
            "{row}"
        );
        assert!(row.contains("&lt;Alice&gt;"), "{row}");
    }

    #[test]
    fn a_member_who_is_not_active_gets_no_form_and_the_reason() {
        let deactivated = member_row(Uuid::from_u128(7), &member(true, false));
        assert!(!deactivated.contains("<form"), "{deactivated}");
        assert!(deactivated.contains("désactivé"), "{deactivated}");
        let pending = member_row(Uuid::from_u128(7), &member(false, true));
        assert!(!pending.contains("<form"), "{pending}");
        assert!(pending.contains("suppression demandée"), "{pending}");
    }
}
