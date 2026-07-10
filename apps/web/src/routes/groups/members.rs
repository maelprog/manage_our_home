//! `/groups/:id/members` — member list with roles, plus the invite /
//! change-role / remove flows. The permission bar is the backend's, taken
//! from `apps/api/src/groups/mod.rs` and mirrored in
//! `manage_our_home_shared::validation::groups`: only owner/admin see the
//! invite form (`can_manage_group`, API returns 403 otherwise), and the
//! role/remove controls on a member row follow `actor_can_act_on` (owner
//! acts on anyone but itself, admin only on standard members, standard on
//! no one — API also 403s if a forged form tries anyway).
//!
//! Error table: 404 (`get_group`) unknown/foreign group; 403 on
//! invite/role/remove without the required role; 400 `invalid_role` on a
//! role other than admin/standard.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::{
    CreateInvitationRequest, GroupDetailResponse, InvitationCreatedResponse,
};
use manage_our_home_shared::validation::groups::{actor_can_act_on, can_manage_group};
use uuid::Uuid;

use crate::app::shell;
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{cookie_of, header_with_groups, role_label};

pub(crate) fn group_not_found_page() -> Html<String> {
    let body = view! {
        <h1>"Groupe introuvable"</h1>
        <p>"Ce groupe n'existe pas ou vous n'en êtes pas membre."</p>
        <a class="button secondary" href="/groups">"Retour à mes groupes"</a>
    };
    Html(shell("Groupe introuvable", &body.to_html()))
}

pub(crate) fn service_unavailable_page() -> Html<String> {
    let body = view! {
        <h1>"Service momentanément indisponible"</h1>
        <p>"Merci de réessayer dans quelques instants."</p>
        <a class="button secondary" href="/groups">"Retour à mes groupes"</a>
    };
    Html(shell("Service indisponible", &body.to_html()))
}

/// `GET /groups/:id` against apps/api: `Ok(None)` is the 404 case
/// (unknown group, or caller not a member — RLS makes it invisible).
pub(crate) async fn fetch_group_detail(
    state: &AppState,
    cookie: Option<&str>,
    group_id: Uuid,
) -> Result<Option<GroupDetailResponse>, String> {
    let resp = api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{group_id}"),
        cookie,
        None,
    )
    .await?;
    if resp.status == reqwest::StatusCode::OK {
        Ok(serde_json::from_value(resp.body).ok())
    } else {
        Ok(None)
    }
}

fn notice_text(code: &str) -> Option<&'static str> {
    match code {
        "role_changed" => Some("Rôle mis à jour."),
        "member_removed" => Some("Membre retiré du groupe."),
        _ => None,
    }
}

fn error_text(code: &str) -> Option<&'static str> {
    match code {
        "forbidden" => Some("Vous n'avez pas les droits nécessaires pour cette action."),
        "invalid_role" => Some("Rôle invalide."),
        "unavailable" => Some("Service momentanément indisponible, merci de réessayer."),
        _ => None,
    }
}

fn page(
    header: &str,
    me: &MeResponse,
    group: &GroupDetailResponse,
    notice: Option<&str>,
    error: Option<&str>,
    invite_link: Option<&str>,
) -> String {
    let my_role = group
        .members
        .iter()
        .find(|m| m.user_id == me.user_id)
        .map(|m| m.role.clone())
        .unwrap_or_default();

    let rows = group
        .members
        .iter()
        .map(|m| {
            let controls = if actor_can_act_on(&my_role, &m.role) {
                let role_action = format!("/groups/{}/members/{}/role", group.id, m.user_id);
                let remove_action = format!("/groups/{}/members/{}/remove", group.id, m.user_id);
                Some(view! {
                    <span style="display:flex;gap:0.5rem;align-items:center;">
                        <form method="post" action=role_action style="flex-direction:row;gap:0.4rem;align-items:center;margin:0;">
                            <select name="role">
                                <option value="admin" selected=m.role == "admin">"Admin"</option>
                                <option value="standard" selected=m.role == "standard">"Membre"</option>
                            </select>
                            <button type="submit" class="secondary">"Changer le rôle"</button>
                        </form>
                        <form method="post" action=remove_action style="margin:0;">
                            <button type="submit" class="secondary">"Retirer"</button>
                        </form>
                    </span>
                })
            } else {
                None
            };
            view! {
                <li style="display:flex;justify-content:space-between;align-items:center;gap:0.75rem;padding:0.6rem 0;border-bottom:1px solid var(--border);flex-wrap:wrap;">
                    <span>
                        <strong>{m.display_name.clone()}</strong>
                        <span class="muted">{format!(" ({})", m.email)}</span>
                        " — "
                        <span class="muted">{role_label(&m.role)}</span>
                    </span>
                    {controls}
                </li>
            }
        })
        .collect::<Vec<_>>();

    let invite_section = can_manage_group(&my_role).then(|| {
        let invite_action = format!("/groups/{}/members/invite", group.id);
        view! {
            <h2 style="font-size:1.1rem;margin-top:1.5rem;">"Inviter un membre"</h2>
            <p class="muted">"L'invitation est valable 7 jours et à usage unique. Avec un email, le lien est envoyé directement ; sinon il s'affiche ici pour être partagé."</p>
            <form method="post" action=invite_action>
                <label>
                    "Email (optionnel)"
                    <input type="email" name="invited_email" />
                </label>
                <button type="submit">"Créer une invitation"</button>
            </form>
        }
    });

    let invite_link_block = invite_link.map(|link| {
        view! {
            <p class="notice success">
                "Invitation créée. Lien à partager : "
                <a href=link.to_string()>{link.to_string()}</a>
            </p>
        }
    });

    let body = view! {
        <div inner_html=header.to_string()></div>
        <h1>{format!("Membres — {}", group.name)}</h1>
        {notice.map(|n| view! { <p class="notice success">{n.to_string()}</p> })}
        {error.map(|e| view! { <p class="notice error">{e.to_string()}</p> })}
        {invite_link_block}
        <ul style="list-style:none;padding:0;margin:0;">{rows}</ul>
        {invite_section}
        <div class="links">
            <a href="/groups">"Retour à mes groupes"</a>
            <a href=format!("/groups/{}/settings", group.id)>"Paramètres du groupe"</a>
        </div>
    };
    shell(&format!("Membres — {}", group.name), &body.to_html())
}

#[derive(serde::Deserialize)]
pub struct MembersQuery {
    notice: Option<String>,
    error: Option<String>,
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Query(query): Query<MembersQuery>,
) -> Response {
    let cookie = cookie_of(&headers);
    let group = match fetch_group_detail(&state, cookie.as_deref(), group_id).await {
        Ok(Some(group)) => group,
        Ok(None) => return group_not_found_page().into_response(),
        Err(_) => return service_unavailable_page().into_response(),
    };
    let redirect_to = format!("/groups/{group_id}/members");
    let (_groups, header) = header_with_groups(&state, &headers, &me, &redirect_to).await;
    let notice = query.notice.as_deref().and_then(notice_text);
    let error = query.error.as_deref().and_then(error_text);
    Html(page(&header, &me, &group, notice, error, None)).into_response()
}

#[derive(serde::Deserialize)]
pub struct InviteForm {
    #[serde(default)]
    invited_email: String,
}

pub async fn invite(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Form(form): Form<InviteForm>,
) -> Response {
    let members_url = format!("/groups/{group_id}/members");
    let email = form.invited_email.trim();
    let invited_email = (!email.is_empty()).then(|| email.to_string());

    let cookie = cookie_of(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{group_id}/invitations"),
        cookie.as_deref(),
        Some(serde_json::json!(CreateInvitationRequest { invited_email })),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            // Render directly (no PRG) so the single-use link can be shown
            // exactly once without ever living in a URL.
            let Ok(invitation) = serde_json::from_value::<InvitationCreatedResponse>(resp.body)
            else {
                return Redirect::to(&format!("{members_url}?error=unavailable")).into_response();
            };
            let group = match fetch_group_detail(&state, cookie.as_deref(), group_id).await {
                Ok(Some(group)) => group,
                Ok(None) => return group_not_found_page().into_response(),
                Err(_) => return service_unavailable_page().into_response(),
            };
            let (_groups, header) = header_with_groups(&state, &headers, &me, &members_url).await;
            let link = format!("/groups/invitations/{}/accept", invitation.token);
            Html(page(&header, &me, &group, None, None, Some(&link))).into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            Redirect::to(&format!("{members_url}?error=forbidden")).into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            group_not_found_page().into_response()
        }
        Ok(_) | Err(_) => Redirect::to(&format!("{members_url}?error=unavailable")).into_response(),
    }
}

#[derive(serde::Deserialize)]
pub struct ChangeRoleForm {
    role: String,
}

pub async fn change_role(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((group_id, user_id)): Path<(Uuid, Uuid)>,
    Form(form): Form<ChangeRoleForm>,
) -> Response {
    let members_url = format!("/groups/{group_id}/members");
    let cookie = cookie_of(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{group_id}/members/{user_id}/role"),
        cookie.as_deref(),
        Some(serde_json::json!({ "role": form.role })),
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            format!("{members_url}?notice=role_changed")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{members_url}?error=forbidden")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            format!("{members_url}?error=invalid_role")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return group_not_found_page().into_response()
        }
        Ok(_) | Err(_) => format!("{members_url}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}

pub async fn remove(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((group_id, user_id)): Path<(Uuid, Uuid)>,
) -> Response {
    let members_url = format!("/groups/{group_id}/members");
    let cookie = cookie_of(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &format!("/groups/{group_id}/members/{user_id}"),
        cookie.as_deref(),
        None,
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            format!("{members_url}?notice=member_removed")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{members_url}?error=forbidden")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return group_not_found_page().into_response()
        }
        Ok(_) | Err(_) => format!("{members_url}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}
