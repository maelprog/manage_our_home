//! `/groups` — every group the user belongs to with their role, plus the
//! two entry points of issue #17's route table: "create group" and "join
//! via invite" (paste the invitation link or bare token).

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use leptos::prelude::*;
use manage_our_home_shared::dto::groups::GroupSummary;
use manage_our_home_shared::validation::groups::parse_invitation_token;

use crate::app::{shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::AppState;

use super::{header_with_groups, role_label};

#[derive(serde::Deserialize)]
pub struct ListQuery {
    notice: Option<String>,
    error: Option<String>,
}

fn notice_text(code: &str) -> Option<&'static str> {
    match code {
        "group_created" => Some("Groupe créé."),
        "joined" => Some("Vous avez rejoint le groupe."),
        "left" => Some("Vous avez quitté le groupe."),
        "group_deleted" => Some("Groupe supprimé."),
        _ => None,
    }
}

fn error_text(code: &str) -> Option<&'static str> {
    match code {
        "invalid_invite" => {
            Some("Invitation invalide : collez le lien d'invitation complet ou son code.")
        }
        _ => None,
    }
}

/// One group of the list. Its name leads to its members (#466), as the
/// "Membres" link beside it does.
fn group_row(g: &GroupSummary) -> impl IntoView {
    let members_href = format!("/groups/{}/members", g.group_id);
    let settings_href = format!("/groups/{}/settings", g.group_id);
    view! {
        <li class="list-row">
            <span>
                <a href=members_href.clone()><strong>{g.name.clone()}</strong></a>
                " — "
                <span class="muted">{role_label(&g.role)}</span>
            </span>
            <span class="actions">
                <a href=members_href>"Membres"</a>
                <a href=settings_href>"Paramètres"</a>
            </span>
        </li>
    }
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> impl IntoResponse {
    let (groups, header) = header_with_groups(&state, &headers, &me, "/groups").await;
    let notice = query.notice.as_deref().and_then(notice_text);
    let error = query.error.as_deref().and_then(error_text);

    let rows = groups.iter().map(group_row).collect::<Vec<_>>();

    let body = view! {
        <h1>"Mes groupes"</h1>
        {notice.map(|n| view! { <p class="notice success">{n}</p> })}
        {error.map(|e| view! { <p class="notice error">{e}</p> })}
        {if groups.is_empty() {
            Some(view! { <p class="muted">"Vous n'appartenez à aucun groupe pour le moment."</p> })
        } else {
            None
        }}
        <ul class="list">{rows}</ul>
        <div class="actions">
            <a class="btn" href="/groups/new">"Créer un groupe"</a>
        </div>
        <h2 id="join">"Rejoindre via une invitation"</h2>
        <form method="post" action="/groups/join">
            <label>
                "Lien ou code d'invitation"
                <input type="text" name="invite" required=true placeholder="https://…/groups/invitations/…/accept" />
            </label>
            <button type="submit">"Rejoindre"</button>
        </form>
    };
    Html(shell_with_header(
        Width::Read,
        "Mes groupes",
        &header,
        &body.to_html(),
    ))
}

#[derive(serde::Deserialize)]
pub struct JoinForm {
    invite: String,
}

/// `POST /groups/join` — parses the pasted invitation (full link or bare
/// token, `parse_invitation_token`) and bounces to the confirm page. The
/// API isn't called yet: acceptance is a deliberate second step so the
/// user sees what they're joining before their membership is created.
pub async fn join(CurrentUser(_me): CurrentUser, Form(form): Form<JoinForm>) -> Response {
    match parse_invitation_token(&form.invite) {
        Some(token) => Redirect::to(&format!("/groups/invitations/{token}/accept")).into_response(),
        None => Redirect::to("/groups?error=invalid_invite").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #466: the group's name is a link to its members.
    #[test]
    fn a_group_name_leads_to_its_members() {
        let g = GroupSummary {
            group_id: uuid::Uuid::from_u128(1),
            name: "Foyer".to_string(),
            role: "owner".to_string(),
            accent: "sarcelle".to_string(),
            ownership_notice: None,
        };
        let html = group_row(&g).to_html();
        assert!(
            html.contains(&format!(
                r#"<a href="/groups/{}/members"><strong>Foyer</strong></a>"#,
                g.group_id
            )),
            "{html}"
        );
    }
}
