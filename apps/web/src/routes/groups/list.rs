//! `/groups` — every group the user belongs to with their role, plus the
//! two entry points of issue #17's route table: "create group" and "join
//! via invite" (paste the invitation link or bare token).

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use leptos::prelude::*;
use manage_our_home_shared::validation::groups::parse_invitation_token;

use crate::app::shell;
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

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> impl IntoResponse {
    let (groups, header) = header_with_groups(&state, &headers, &me, "/groups").await;
    let notice = query.notice.as_deref().and_then(notice_text);
    let error = query.error.as_deref().and_then(error_text);

    let rows = groups
        .iter()
        .map(|g| {
            let members_href = format!("/groups/{}/members", g.group_id);
            let settings_href = format!("/groups/{}/settings", g.group_id);
            view! {
                <li style="display:flex;justify-content:space-between;align-items:center;gap:0.75rem;padding:0.6rem 0;border-bottom:1px solid var(--border);">
                    <span>
                        <strong>{g.name.clone()}</strong>
                        " — "
                        <span class="muted">{role_label(&g.role)}</span>
                    </span>
                    <span style="display:flex;gap:0.75rem;">
                        <a href=members_href>"Membres"</a>
                        <a href=settings_href>"Paramètres"</a>
                    </span>
                </li>
            }
        })
        .collect::<Vec<_>>();

    let body = view! {
        <div inner_html=header></div>
        <h1>"Mes groupes"</h1>
        {notice.map(|n| view! { <p class="notice success">{n}</p> })}
        {error.map(|e| view! { <p class="notice error">{e}</p> })}
        {if groups.is_empty() {
            Some(view! { <p class="muted">"Vous n'appartenez à aucun groupe pour le moment."</p> })
        } else {
            None
        }}
        <ul style="list-style:none;padding:0;margin:0 0 1.5rem 0;">{rows}</ul>
        <a class="button" href="/groups/new" style="display:block;margin-bottom:1.5rem;">
            "Créer un groupe"
        </a>
        <h2 style="font-size:1.1rem;">"Rejoindre via une invitation"</h2>
        <form method="post" action="/groups/join">
            <label>
                "Lien ou code d'invitation"
                <input type="text" name="invite" required=true placeholder="https://…/groups/invitations/…/accept" />
            </label>
            <button type="submit">"Rejoindre"</button>
        </form>
    };
    Html(shell("Mes groupes", &body.to_html()))
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
