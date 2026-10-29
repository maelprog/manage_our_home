//! `/groups/:id/settings` — rename, leave, transfer ownership, delete.
//! Permission bar per `apps/api/src/groups/mod.rs`: rename is
//! owner/admin (403 otherwise); transfer and delete are owner-only;
//! anyone can leave, but an owner leaving must name a successor (422
//! `new_owner_id_required`) and the last remaining member must delete the
//! group instead (409 `last_member_must_delete_group`). Transfer also
//! 404s on a non-member target and 422s on `cannot_transfer_to_self`
//! (both unreachable through this UI's member-only select, still mapped).
//!
//! Since #530 the page also lists the members with their roles and, for
//! owners and admins, the invitation by e-mail and a « Gérer » per member
//! they may act on — the controls themselves stay on `/groups/:id/members`.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::{GroupDetailResponse, GroupMember, RenameGroupRequest};
use manage_our_home_shared::validation::groups::{
    actor_can_act_on, can_manage_group, validate_group_name,
};
use uuid::Uuid;

use crate::app::{html_escape, member_link, shell_with_header, Icon, Width};
use crate::icons::{icon, Icon as Glyph};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::members::{fetch_group_detail, group_not_found_page, service_unavailable_page};
use super::{cookie_of, header_with_groups, role_badge, role_label};

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

/// One member of the « Membres » card: their icon and name as a link to
/// their profile (#466), their e-mail, their role in a pastille, and
/// « Gérer » when the caller may act on them (`actor_can_act_on`, the API's
/// own bar). « Gérer » leads to the member's row on the members page, where
/// the role select and « Retirer » are.
fn member_row(group_id: Uuid, my_role: &str, m: &GroupMember) -> String {
    let manage = if actor_can_act_on(my_role, &m.role) {
        format!(
            r#"<a class="btn secondary" href="/groups/{group_id}/members#member-{id}">Gérer</a>"#,
            id = m.user_id,
        )
    } else {
        String::new()
    };
    format!(
        r#"<li class="list-row"><span>{link} <span class="muted">{email}</span></span><span class="actions">{badge}{manage}</span></li>"#,
        link = member_link(
            group_id,
            m.user_id,
            &m.display_name,
            Icon::avatar(m.has_avatar)
        ),
        email = html_escape(&m.email),
        badge = role_badge(&m.role),
    )
}

/// « Membres »: every member, then — for owners and admins only — the
/// invitation by e-mail. It posts to the members page's route, which sends
/// the e-mail and shows the link once (`members::invite`).
fn members_card(group: &GroupDetailResponse, my_role: &str) -> String {
    let rows: String = group
        .members
        .iter()
        .map(|m| member_row(group.id, my_role, m))
        .collect();
    let invite = if can_manage_group(my_role) {
        format!(
            r#"<form method="post" action="/groups/{id}/members/invite" class="quick-add">
<label>E-mail à inviter<input type="email" name="invited_email" required></label>
<button type="submit">{icon} Inviter</button>
</form>
<p class="muted">L'invitation est valable 7 jours. Pour un lien à partager sans e-mail : <a href="/groups/{id}/members">membres du groupe</a>.</p>"#,
            id = group.id,
            icon = icon(Glyph::Members),
        )
    } else {
        String::new()
    };
    format!(
        r#"<section class="card">
<h2>Membres</h2>
<ul class="list">{rows}</ul>
{invite}
</section>"#
    )
}

/// The other members, as the options of a successor `<select>`.
fn successor_options(others: &[&GroupMember]) -> String {
    others
        .iter()
        .map(|m| {
            format!(
                r#"<option value="{id}">{name} — {role}</option>"#,
                id = m.user_id,
                name = html_escape(&m.display_name),
                role = role_label(&m.role),
            )
        })
        .collect()
}

/// « Nom et zone sensible »: rename (owners and admins), transfer (the
/// owner, when someone can take it), leave (everyone; the owner names a
/// successor), delete (the owner). The labels are the ones the e2e suite
/// finds the controls by (DESIGN.md, contrainte 5).
fn name_card(group: &GroupDetailResponse, my_role: &str, others: &[&GroupMember]) -> String {
    let id = group.id;
    let is_owner = my_role == "owner";
    let options = successor_options(others);

    let rename = if can_manage_group(my_role) {
        format!(
            r#"<form method="post" action="/groups/{id}/settings/rename">
<label>Nom du groupe<input type="text" name="name" required value="{name}"></label>
<button type="submit">Renommer</button>
</form>"#,
            name = html_escape(&group.name),
        )
    } else {
        String::new()
    };

    let transfer = if is_owner && !others.is_empty() {
        format!(
            r#"<h3>Transférer la propriété</h3>
<p class="muted">Le nouveau propriétaire prend la main sur le groupe ; vous devenez admin.</p>
<form method="post" action="/groups/{id}/settings/transfer">
<label>Nouveau propriétaire<select name="new_owner_id" required>{options}</select></label>
<button type="submit" class="secondary">Transférer</button>
</form>"#
        )
    } else {
        String::new()
    };

    let leave = if is_owner && !others.is_empty() {
        format!(
            r#"<h3>Quitter le groupe</h3>
<p class="muted">En tant que propriétaire, vous devez d'abord désigner un successeur.</p>
<form method="post" action="/groups/{id}/settings/leave">
<label>Successeur<select name="new_owner_id" required>{options}</select></label>
<button type="submit" class="secondary danger">Quitter le groupe</button>
</form>"#
        )
    } else {
        format!(
            r#"<h3>Quitter le groupe</h3>
<form method="post" action="/groups/{id}/settings/leave">
<button type="submit" class="secondary danger">Quitter le groupe</button>
</form>"#
        )
    };

    let delete = if is_owner {
        format!(
            r#"<h3>Supprimer le groupe</h3>
<p class="muted">Supprime définitivement le groupe, ses membres et ses invitations.</p>
<form method="post" action="/groups/{id}/settings/delete">
<button type="submit" class="secondary danger">Supprimer le groupe</button>
</form>"#
        )
    } else {
        String::new()
    };

    format!(
        r#"<section class="card">
<h2>Nom et zone sensible</h2>
{rename}
{transfer}
{leave}
{delete}
</section>"#
    )
}

/// The family settings (#530, maquettes « Mobile — Famille » et « PC —
/// Famille »): the family's name under « Paramètres de la famille » and
/// your role, then the members and their roles with the invitation, then
/// the name and the sensitive zone; beside them on a PC, under them on a
/// phone, the column where « Couleur de la famille » (#538) goes first.
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
    let others = group
        .members
        .iter()
        .filter(|m| m.user_id != me.user_id)
        .collect::<Vec<_>>();

    let notice = notice
        .map(|n| format!(r#"<p class="notice success">{n}</p>"#))
        .unwrap_or_default();
    let error = error
        .map(|e| format!(r#"<p class="notice error">{e}</p>"#))
        .unwrap_or_default();

    // Cross-link only, no controls: the Google Calendar screens (front epic
    // F11) live under `/agenda/imports` because what they produce is agenda
    // data, but "connecter un agenda" is the kind of thing people come
    // looking for in the family settings, so the door is signposted here too.
    let body = format!(
        r#"<hgroup>
<p class="eyebrow">Paramètres de la famille</p>
<h1>{name}</h1>
<p class="muted">Votre rôle : {role}</p>
</hgroup>
{notice}{error}
<div class="settings">
<div>
{members}
{name_card}
</div>
<div>
<section class="card">
<h2>Agendas Google</h2>
<p class="muted">Les agendas Google connectés à cette famille se gèrent depuis l'agenda : leurs événements y sont recopiés à la demande.</p>
<p><a href="/agenda/imports">Gérer les agendas Google</a></p>
</section>
</div>
</div>
<div class="links">
<a href="/groups">Retour à mes groupes</a>
<a href="/groups/{id}/members">Membres du groupe</a>
</div>"#,
        name = html_escape(&group.name),
        role = role_badge(&my_role),
        members = members_card(group, &my_role),
        name_card = name_card(group, &my_role, &others),
        id = group.id,
    );
    shell_with_header(
        Width::Full,
        &format!("Paramètres — {}", group.name),
        header,
        &body,
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    const G: Uuid = Uuid::from_u128(1);
    const OWNER: Uuid = Uuid::from_u128(2);
    const ADMIN: Uuid = Uuid::from_u128(3);
    const STANDARD: Uuid = Uuid::from_u128(4);

    fn me(id: Uuid) -> MeResponse {
        MeResponse {
            user_id: id,
            email: "me@example.test".into(),
            display_name: "Me".into(),
            email_verified: true,
            is_superadmin: false,
            has_password: true,
            deletion_requested_at: None,
            terms_accepted_version: None,
        }
    }

    fn member(id: Uuid, name: &str, role: &str) -> GroupMember {
        GroupMember {
            user_id: id,
            role: role.to_string(),
            display_name: name.to_string(),
            email: format!("{}@example.test", name.to_lowercase()),
            has_avatar: false,
        }
    }

    fn family() -> GroupDetailResponse {
        GroupDetailResponse {
            id: G,
            name: "Famille Martin".to_string(),
            members: vec![
                member(OWNER, "Alex", "owner"),
                member(ADMIN, "Sam", "admin"),
                member(STANDARD, "Chloé", "standard"),
            ],
        }
    }

    fn page_as(id: Uuid) -> String {
        page("", &me(id), &family(), None, None)
    }

    fn manage_link(user: Uuid) -> String {
        format!(r#"href="/groups/{G}/members#member-{user}">Gérer</a>"#)
    }

    /// The title is the family's name, under « Paramètres de la famille »,
    /// with the caller's role beside it.
    #[test]
    fn the_page_is_titled_by_the_family_and_says_your_role() {
        let html = page_as(ADMIN);
        assert!(
            html.contains(r#"<p class="eyebrow">Paramètres de la famille</p>"#),
            "{html}"
        );
        assert!(html.contains("<h1>Famille Martin</h1>"), "{html}");
        assert!(
            html.contains(r#"Votre rôle : <span class="badge accent">Admin</span>"#),
            "{html}"
        );
    }

    #[test]
    fn the_family_name_is_escaped() {
        let mut group = family();
        group.name = "<b>F</b>".to_string();
        let html = page("", &me(OWNER), &group, None, None);
        assert!(html.contains("&lt;b&gt;F&lt;/b&gt;"), "{html}");
        assert!(!html.contains("<b>F</b>"), "{html}");
    }

    /// Every member, linked to their profile, with their e-mail and their
    /// role in a pastille.
    #[test]
    fn every_member_is_listed_with_a_role_pastille() {
        let html = page_as(STANDARD);
        for (id, name, role) in [
            (OWNER, "Alex", "owner"),
            (ADMIN, "Sam", "admin"),
            (STANDARD, "Chloé", "standard"),
        ] {
            assert!(
                html.contains(&crate::app::member_link(
                    G,
                    id,
                    name,
                    crate::app::Icon::Initial
                )),
                "{name}: {html}"
            );
            assert!(
                html.contains(&format!("{}@example.test", name.to_lowercase())),
                "{name}: {html}"
            );
            assert!(
                html.contains(&super::super::role_badge(role)),
                "{role}: {html}"
            );
        }
    }

    /// The acceptance criterion of #530: a standard member sees neither
    /// « Gérer » nor the invitation.
    #[test]
    fn a_standard_member_sees_neither_manage_nor_the_invitation() {
        let html = page_as(STANDARD);
        assert!(!html.contains(">Gérer<"), "{html}");
        assert!(!html.contains("/members/invite"), "{html}");
        assert!(!html.contains("invited_email"), "{html}");
    }

    /// « Gérer » follows the API's bar (`actor_can_act_on`): the owner on
    /// everyone but themselves…
    #[test]
    fn the_owner_manages_everyone_but_themselves() {
        let html = page_as(OWNER);
        assert!(html.contains(&manage_link(ADMIN)), "{html}");
        assert!(html.contains(&manage_link(STANDARD)), "{html}");
        assert!(!html.contains(&manage_link(OWNER)), "{html}");
    }

    /// … an admin on the standard members only.
    #[test]
    fn an_admin_manages_the_standard_members_only() {
        let html = page_as(ADMIN);
        assert!(html.contains(&manage_link(STANDARD)), "{html}");
        assert!(!html.contains(&manage_link(OWNER)), "{html}");
        assert!(!html.contains(&manage_link(ADMIN)), "{html}");
    }

    /// Owners and admins invite by e-mail, from the members' card, through
    /// the members page's own route.
    #[test]
    fn owners_and_admins_invite_by_email() {
        for who in [OWNER, ADMIN] {
            let html = page_as(who);
            assert!(
                html.contains(&format!(r#"action="/groups/{G}/members/invite""#)),
                "{html}"
            );
            assert!(
                html.contains(r#"<input type="email" name="invited_email" required>"#),
                "{html}"
            );
            assert!(html.contains("</svg> Inviter</button>"), "{html}");
        }
    }

    /// The rest of the bar, unchanged by the redesign: rename for owners
    /// and admins, transfer and delete for the owner, leave for everyone —
    /// the owner naming a successor.
    #[test]
    fn the_controls_follow_the_permission_bar() {
        let rename = format!(r#"action="/groups/{G}/settings/rename""#);
        let transfer = format!(r#"action="/groups/{G}/settings/transfer""#);
        let delete = format!(r#"action="/groups/{G}/settings/delete""#);
        let leave = format!(r#"action="/groups/{G}/settings/leave""#);

        let owner = page_as(OWNER);
        for action in [&rename, &transfer, &delete, &leave] {
            assert!(owner.contains(action.as_str()), "{action}: {owner}");
        }
        assert!(
            owner.contains("vous devez d'abord désigner un successeur"),
            "{owner}"
        );
        assert_eq!(
            owner.matches(r#"name="new_owner_id""#).count(),
            2,
            "{owner}"
        );

        let admin = page_as(ADMIN);
        assert!(admin.contains(&rename), "{admin}");
        assert!(admin.contains(&leave), "{admin}");
        assert!(!admin.contains(&transfer), "{admin}");
        assert!(!admin.contains(&delete), "{admin}");

        let standard = page_as(STANDARD);
        assert!(standard.contains(&leave), "{standard}");
        for action in [&rename, &transfer, &delete] {
            assert!(!standard.contains(action.as_str()), "{action}: {standard}");
        }
        assert!(!standard.contains("new_owner_id"), "{standard}");
    }

    /// An owner alone in the family has no one to hand it to: no transfer,
    /// and leaving asks for no successor (apps/api answers 409, mapped).
    #[test]
    fn an_owner_alone_has_no_transfer_and_no_successor_to_name() {
        let group = GroupDetailResponse {
            id: G,
            name: "Solo".to_string(),
            members: vec![member(OWNER, "Alex", "owner")],
        };
        let html = page("", &me(OWNER), &group, None, None);
        assert!(!html.contains("/settings/transfer"), "{html}");
        assert!(!html.contains("new_owner_id"), "{html}");
        assert!(html.contains("/settings/leave"), "{html}");
    }

    /// A successor is picked among the others, named with their role.
    #[test]
    fn a_successor_is_one_of_the_other_members() {
        let html = page_as(OWNER);
        assert!(
            html.contains(&format!(r#"<option value="{ADMIN}">Sam — Admin</option>"#)),
            "{html}"
        );
        assert!(
            !html.contains(&format!(r#"<option value="{OWNER}">"#)),
            "{html}"
        );
    }
}
