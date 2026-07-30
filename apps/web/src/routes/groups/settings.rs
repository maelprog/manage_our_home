//! `/groups/:id/settings` — rename, leave, transfer ownership, delete.
//! Permission bar per `apps/api/src/groups/mod.rs`: rename is
//! owner/admin (403 otherwise); transfer and delete are owner-only;
//! anyone can leave, but an owner leaving must name a successor (422
//! `new_owner_id_required`) and the last remaining member must delete the
//! group instead (409 `last_member_must_delete_group`). Transfer also
//! 404s on a non-member target and 422s on `cannot_transfer_to_self`
//! (both unreachable through this UI's member-only select, still mapped).

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::{GroupDetailResponse, RenameGroupRequest};
use manage_our_home_shared::validation::groups::{can_manage_group, validate_group_name};
use uuid::Uuid;

use crate::app::shell;
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::members::{fetch_group_detail, group_not_found_page, service_unavailable_page};
use super::{cookie_of, header_with_groups, role_label};

fn notice_text(code: &str) -> Option<&'static str> {
    match code {
        "renamed" => Some("Groupe renommé."),
        "ownership_transferred" => Some("Propriété transférée."),
        _ => None,
    }
}

fn error_text(code: &str) -> Option<&'static str> {
    match code {
        "forbidden" => Some("Vous n'avez pas les droits nécessaires pour cette action."),
        "name_required" => Some("Le nom du groupe ne peut pas être vide."),
        "last_member" => Some(
            "Vous êtes le dernier membre : quitter n'est pas possible, supprimez le groupe à la place.",
        ),
        "new_owner_required" => {
            Some("En tant que propriétaire, vous devez désigner un successeur pour quitter le groupe.")
        }
        "target_not_member" => Some("Ce membre ne fait pas (ou plus) partie du groupe."),
        "cannot_transfer_to_self" => Some("Vous êtes déjà propriétaire de ce groupe."),
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
) -> String {
    let my_role = group
        .members
        .iter()
        .find(|m| m.user_id == me.user_id)
        .map(|m| m.role.clone())
        .unwrap_or_default();
    let is_owner = my_role == "owner";
    let others = group
        .members
        .iter()
        .filter(|m| m.user_id != me.user_id)
        .collect::<Vec<_>>();

    let rename_section = can_manage_group(&my_role).then(|| {
        let action = format!("/groups/{}/settings/rename", group.id);
        view! {
            <h2>"Renommer le groupe"</h2>
            <form method="post" action=action>
                <label>
                    "Nom du groupe"
                    <input type="text" name="name" required=true value=group.name.clone() />
                </label>
                <button type="submit">"Renommer"</button>
            </form>
        }
    });

    let successor_options = others
        .iter()
        .map(|m| {
            view! {
                <option value=m.user_id.to_string()>
                    {format!("{} — {}", m.display_name, role_label(&m.role))}
                </option>
            }
        })
        .collect::<Vec<_>>();

    let transfer_section = (is_owner && !others.is_empty()).then(|| {
        let action = format!("/groups/{}/settings/transfer", group.id);
        view! {
            <h2>"Transférer la propriété"</h2>
            <p class="muted">"Le nouveau propriétaire prend la main sur le groupe ; vous devenez admin."</p>
            <form method="post" action=action>
                <label>
                    "Nouveau propriétaire"
                    <select name="new_owner_id" required=true>{successor_options.clone()}</select>
                </label>
                <button type="submit">"Transférer"</button>
            </form>
        }
    });

    // Cross-link only, no controls: the Google Calendar screens (front epic F11)
    // live under `/agenda/imports` because what they produce is agenda data, but
    // "connecter un agenda" is the kind of thing people come looking for in the
    // family settings, so the door is signposted from here too.
    let calendar_imports_section = view! {
        <h2>"Agendas Google"</h2>
        <p class="muted">"Les agendas Google connectés à cette famille se gèrent depuis l'agenda : leurs événements y sont recopiés à la demande."</p>
        <p><a href="/agenda/imports">"Gérer les agendas Google"</a></p>
    };

    let leave_action = format!("/groups/{}/settings/leave", group.id);
    let leave_section = if is_owner && !others.is_empty() {
        view! {
            <h2>"Quitter le groupe"</h2>
            <p class="muted">"En tant que propriétaire, vous devez d'abord désigner un successeur."</p>
            <form method="post" action=leave_action>
                <label>
                    "Successeur"
                    <select name="new_owner_id" required=true>{successor_options}</select>
                </label>
                <button type="submit" class="secondary">"Quitter le groupe"</button>
            </form>
        }
        .into_any()
    } else {
        view! {
            <h2>"Quitter le groupe"</h2>
            <form method="post" action=leave_action>
                <button type="submit" class="secondary">"Quitter le groupe"</button>
            </form>
        }
        .into_any()
    };

    let delete_section = is_owner.then(|| {
        let action = format!("/groups/{}/settings/delete", group.id);
        view! {
            <h2>"Supprimer le groupe"</h2>
            <p class="muted">"Supprime définitivement le groupe, ses membres et ses invitations."</p>
            <form method="post" action=action>
                <button type="submit" class="secondary danger">"Supprimer le groupe"</button>
            </form>
        }
    });

    let body = view! {
        <div inner_html=header.to_string()></div>
        <h1>{format!("Paramètres — {}", group.name)}</h1>
        <p class="muted">{format!("Votre rôle : {}", role_label(&my_role))}</p>
        {notice.map(|n| view! { <p class="notice success">{n.to_string()}</p> })}
        {error.map(|e| view! { <p class="notice error">{e.to_string()}</p> })}
        {rename_section}
        {transfer_section}
        {calendar_imports_section}
        {leave_section}
        {delete_section}
        <div class="links">
            <a href="/groups">"Retour à mes groupes"</a>
            <a href=format!("/groups/{}/members", group.id)>"Membres du groupe"</a>
        </div>
    };
    shell(&format!("Paramètres — {}", group.name), &body.to_html())
}

#[derive(serde::Deserialize)]
pub struct SettingsQuery {
    notice: Option<String>,
    error: Option<String>,
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Query(query): Query<SettingsQuery>,
) -> Response {
    let cookie = cookie_of(&headers);
    let group = match fetch_group_detail(&state, cookie.as_deref(), group_id).await {
        Ok(Some(group)) => group,
        Ok(None) => return group_not_found_page().into_response(),
        Err(_) => return service_unavailable_page().into_response(),
    };
    let redirect_to = format!("/groups/{group_id}/settings");
    let (_groups, header) = header_with_groups(&state, &headers, &me, &redirect_to).await;
    let notice = query.notice.as_deref().and_then(notice_text);
    let error = query.error.as_deref().and_then(error_text);
    Html(page(&header, &me, &group, notice, error)).into_response()
}

#[derive(serde::Deserialize)]
pub struct RenameForm {
    name: String,
}

pub async fn rename(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Form(form): Form<RenameForm>,
) -> Response {
    let settings_url = format!("/groups/{group_id}/settings");
    if validate_group_name(&form.name).is_err() {
        return Redirect::to(&format!("{settings_url}?error=name_required")).into_response();
    }

    let cookie = cookie_of(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::PATCH,
        &format!("/groups/{group_id}"),
        cookie.as_deref(),
        Some(serde_json::json!(RenameGroupRequest {
            name: form.name.trim().to_string(),
        })),
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            format!("{settings_url}?notice=renamed")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{settings_url}?error=forbidden")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
            format!("{settings_url}?error=name_required")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return group_not_found_page().into_response()
        }
        Ok(_) | Err(_) => format!("{settings_url}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}

#[derive(serde::Deserialize)]
pub struct TransferForm {
    new_owner_id: Uuid,
}

pub async fn transfer(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Form(form): Form<TransferForm>,
) -> Response {
    let settings_url = format!("/groups/{group_id}/settings");
    let cookie = cookie_of(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{group_id}/transfer-ownership"),
        cookie.as_deref(),
        Some(serde_json::json!({ "new_owner_id": form.new_owner_id })),
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            format!("{settings_url}?notice=ownership_transferred")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{settings_url}?error=forbidden")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            format!("{settings_url}?error=target_not_member")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
            format!("{settings_url}?error=cannot_transfer_to_self")
        }
        Ok(_) | Err(_) => format!("{settings_url}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}

#[derive(serde::Deserialize)]
pub struct LeaveForm {
    /// Empty string when the plain (non-owner) leave form posts — mapped
    /// to `None` before hitting the API.
    #[serde(default)]
    new_owner_id: String,
}

pub async fn leave(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Form(form): Form<LeaveForm>,
) -> Response {
    let settings_url = format!("/groups/{group_id}/settings");
    let new_owner_id = form.new_owner_id.trim().parse::<Uuid>().ok();

    let cookie = cookie_of(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{group_id}/leave"),
        cookie.as_deref(),
        Some(serde_json::json!({ "new_owner_id": new_owner_id })),
    )
    .await;

    let target = match result {
        // The stale `active_group_id` cookie is fine: every page resolves
        // it against the fresh `GET /groups` list and falls back.
        Ok(resp) if resp.status == reqwest::StatusCode::OK => "/groups?notice=left".to_string(),
        Ok(resp) if resp.status == reqwest::StatusCode::CONFLICT => {
            format!("{settings_url}?error=last_member")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
            format!("{settings_url}?error=new_owner_required")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{settings_url}?error=forbidden")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return group_not_found_page().into_response()
        }
        Ok(_) | Err(_) => format!("{settings_url}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}

pub async fn delete(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
) -> Response {
    let settings_url = format!("/groups/{group_id}/settings");
    let cookie = cookie_of(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &format!("/groups/{group_id}"),
        cookie.as_deref(),
        None,
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            "/groups?notice=group_deleted".to_string()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{settings_url}?error=forbidden")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return group_not_found_page().into_response()
        }
        Ok(_) | Err(_) => format!("{settings_url}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}
