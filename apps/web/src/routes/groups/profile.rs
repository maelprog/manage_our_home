//! `/groups/:id/members/:user_id` — a member's profile (#466): their icon,
//! display name, role in the group and arrival date, from
//! `GET /groups/:id/members/:user_id`. Reached from every member name the
//! app links (`app::member_link`): the member list, a message's author, an
//! event's assignees.
//!
//! No e-mail: the API does not send it (arbitrated in the issue). A 404 —
//! the caller or the target is not a member of `:id`, or either id is
//! unknown — renders one "Membre introuvable" page whatever the cause.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use manage_our_home_shared::dto::groups::GroupMemberProfile;
use uuid::Uuid;

use crate::app::{html_escape, member_avatar, shell, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::members::service_unavailable_page;
use super::{cookie_of, header_with_groups, role_label};
use crate::routes::agenda::fmt_paris;

fn member_not_found_page() -> Response {
    let body = r#"<h1>Membre introuvable</h1>
<p>Ce membre n'existe pas ou ne fait pas partie de ce groupe.</p>
<a class="btn secondary" href="/groups">Retour à mes groupes</a>"#;
    (
        StatusCode::NOT_FOUND,
        Html(shell(Width::Form, "Membre introuvable", body)),
    )
        .into_response()
}

/// The page's content, without the shell. The arrival date is a Paris
/// calendar date, written as the account screens write theirs.
fn profile_body(group_id: Uuid, profile: &GroupMemberProfile) -> String {
    format!(
        r#"<h1>{avatar} {name}</h1>
<p><strong>Rôle :</strong> {role}</p>
<p><strong>Membre depuis le</strong> {joined}</p>
<div class="links">
<a href="/groups/{group_id}/members">Retour aux membres</a>
</div>"#,
        avatar = member_avatar(profile.user_id, &profile.display_name),
        name = html_escape(&profile.display_name),
        role = role_label(&profile.role),
        joined = fmt_paris(profile.joined_at, "%d/%m/%Y"),
    )
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((group_id, user_id)): Path<(Uuid, Uuid)>,
) -> Response {
    let cookie = cookie_of(&headers);
    let profile = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{group_id}/members/{user_id}"),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match serde_json::from_value::<GroupMemberProfile>(resp.body) {
                Ok(profile) => profile,
                Err(_) => return service_unavailable_page().into_response(),
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return member_not_found_page()
        }
        Ok(_) | Err(_) => return service_unavailable_page().into_response(),
    };
    let path = format!("/groups/{group_id}/members/{user_id}");
    let (_groups, header) = header_with_groups(&state, &headers, &me, &path).await;
    Html(shell_with_header(
        Width::Read,
        &profile.display_name,
        &header,
        &profile_body(group_id, &profile),
    ))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn profile(name: &str, role: &str) -> GroupMemberProfile {
        GroupMemberProfile {
            user_id: Uuid::from_u128(2),
            display_name: name.to_string(),
            role: role.to_string(),
            // 23:30 UTC on 4 March is already 5 March in Paris (UTC+1).
            joined_at: chrono::Utc.with_ymd_and_hms(2026, 3, 4, 23, 30, 0).unwrap(),
        }
    }

    #[test]
    fn the_profile_names_the_member_with_their_icon() {
        let body = profile_body(Uuid::from_u128(1), &profile("Alice", "standard"));
        assert!(body.contains("<h1>"), "{body}");
        assert!(body.contains("Alice"), "{body}");
        assert!(
            body.contains(&crate::app::member_avatar(Uuid::from_u128(2), "Alice")),
            "the icon is the member's own avatar: {body}"
        );
    }

    #[test]
    fn the_profile_says_the_role_in_french() {
        for (role, label) in [
            ("owner", "Propriétaire"),
            ("admin", "Admin"),
            ("standard", "Membre"),
        ] {
            let body = profile_body(Uuid::from_u128(1), &profile("Alice", role));
            assert!(body.contains(label), "{role}: {body}");
        }
    }

    /// The arrival date is a Paris calendar date, as the rest of the app
    /// writes dates, not the UTC one.
    #[test]
    fn the_profile_gives_the_arrival_date_in_paris() {
        let body = profile_body(Uuid::from_u128(1), &profile("Alice", "standard"));
        assert!(body.contains("05/03/2026"), "{body}");
        assert!(!body.contains("04/03/2026"), "{body}");
    }

    #[test]
    fn the_profile_leads_back_to_the_member_list() {
        let body = profile_body(Uuid::from_u128(1), &profile("Alice", "standard"));
        assert!(
            body.contains(&format!(r#"href="/groups/{}/members""#, Uuid::from_u128(1))),
            "{body}"
        );
    }

    /// A profile the API refuses is a 404 page, status included, whatever
    /// the cause.
    #[tokio::test]
    async fn a_missing_profile_is_a_404_page() {
        let resp = member_not_found_page();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("Membre introuvable"), "{body}");
    }

    #[test]
    fn the_display_name_is_escaped() {
        let body = profile_body(Uuid::from_u128(1), &profile("<b>x</b>", "standard"));
        assert!(!body.contains("<b>"), "{body}");
        assert!(body.contains("&lt;b&gt;"), "{body}");
    }
}
