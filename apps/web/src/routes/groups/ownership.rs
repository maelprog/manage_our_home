//! The notice that tells a member they became the owner of a group without
//! asking for it (#323) — the previous owner's account was purged, the group
//! had no owner left and a member's account was reactivated, or the
//! superadmin designated them. The home page shows it, first thing after
//! logging in, until the member acknowledges it; the acknowledgement is
//! kept (`group_members.ownership_notice_seen_at`), so it does not come back
//! on the next visit. An email says the same (`apps/api`, purge job).

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use manage_our_home_shared::dto::groups::GroupSummary;
use manage_our_home_shared::validation::groups::OwnershipReason;
use uuid::Uuid;

use crate::app::html_escape;
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::cookie_of;

/// One `<section class="notice">` per group whose ownership the caller has
/// not acknowledged yet, each with the form that acknowledges it. Empty when
/// there is none.
pub(crate) fn ownership_notices(groups: &[GroupSummary]) -> String {
    groups
        .iter()
        .filter_map(|g| g.ownership_notice.as_ref().map(|n| (g, n)))
        .map(|(g, notice)| {
            let why = match OwnershipReason::parse(&notice.reason) {
                Some(OwnershipReason::AccountPurged) => {
                    "Le groupe vous revient : le compte de son ancien propriétaire a été supprimé, et vous étiez le premier des membres restants dans l'ordre de succession que prévoient les conditions d'utilisation."
                }
                Some(OwnershipReason::MemberReactivated) => {
                    "Le groupe n'avait plus de propriétaire : à la réactivation d'un compte de ses membres, la propriété vous est revenue, dans l'ordre de succession que prévoient les conditions d'utilisation."
                }
                Some(OwnershipReason::DesignatedBySupport) => {
                    "Le groupe n'avait plus de propriétaire : l'administrateur du service vous a désigné parmi ses membres actifs."
                }
                None => "La propriété de ce groupe vous a été confiée.",
            };
            format!(
                r#"<section class="notice">
<h2>Vous êtes propriétaire du groupe « {name} »</h2>
<p>{why}</p>
<p>Vous pouvez désormais inviter des membres, nommer des administrateurs, transférer la propriété ou supprimer le groupe : <a href="/groups/{id}/settings">paramètres du groupe</a>.</p>
<form method="post" action="/groups/{id}/ownership-notice">
<button type="submit" class="secondary">J'en ai pris connaissance</button>
</form>
</section>"#,
                name = html_escape(&g.name),
                id = g.group_id,
            )
        })
        .collect()
}

/// `POST /groups/:id/ownership-notice` — records the acknowledgement
/// (`POST /groups/:id/ownership-notice/seen` on apps/api), then back to the
/// home page. A failure lands there too: the notice is simply shown again.
pub async fn acknowledge(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
) -> Response {
    let cookie = cookie_of(&headers);
    let _ = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{group_id}/ownership-notice/seen"),
        cookie.as_deref(),
        None,
    )
    .await;
    Redirect::to("/").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use manage_our_home_shared::dto::groups::OwnershipNotice;

    fn group(id: u128, name: &str, reason: Option<&str>) -> GroupSummary {
        GroupSummary {
            group_id: Uuid::from_u128(id),
            name: name.to_string(),
            role: "owner".to_string(),
            ownership_notice: reason.map(|r| OwnershipNotice {
                reason: r.to_string(),
                inherited_at: Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap(),
            }),
        }
    }

    #[test]
    fn nothing_to_acknowledge_renders_nothing() {
        assert_eq!(ownership_notices(&[]), "");
        assert_eq!(ownership_notices(&[group(1, "Famille", None)]), "");
    }

    #[test]
    fn each_inherited_group_gets_its_notice_and_its_acknowledgement() {
        let html = ownership_notices(&[
            group(1, "Famille Martin", Some("account_purged")),
            group(2, "Sans nouvelle", None),
            group(3, "Coloc", Some("designated_by_support")),
        ]);
        assert_eq!(html.matches(r#"<section class="notice">"#).count(), 2);
        assert!(html.contains("Famille Martin"), "{html}");
        assert!(html.contains("Coloc"), "{html}");
        assert!(!html.contains("Sans nouvelle"), "{html}");
        for id in [1u128, 3] {
            let action = format!(
                r#"action="/groups/{}/ownership-notice""#,
                Uuid::from_u128(id)
            );
            assert!(html.contains(&action), "{html}");
        }
        assert_eq!(html.matches("J'en ai pris connaissance").count(), 2);
    }

    #[test]
    fn the_notice_says_why_the_group_came_to_the_member() {
        let purged = ownership_notices(&[group(1, "F", Some("account_purged"))]);
        assert!(
            purged.contains("le compte de son ancien propriétaire a été supprimé"),
            "{purged}"
        );
        let reactivated = ownership_notices(&[group(1, "F", Some("member_reactivated"))]);
        assert!(reactivated.contains("réactivation"), "{reactivated}");
        let designated = ownership_notices(&[group(1, "F", Some("designated_by_support"))]);
        assert!(
            designated.contains("l'administrateur du service vous a désigné"),
            "{designated}"
        );
    }

    /// A reason this build does not know still tells the member the news,
    /// without a cause it cannot vouch for.
    #[test]
    fn an_unknown_reason_still_announces_the_ownership() {
        let html = ownership_notices(&[group(1, "Famille", Some("later_reason"))]);
        assert!(html.contains("propriétaire"), "{html}");
        assert!(!html.contains("later_reason"), "{html}");
    }

    #[test]
    fn the_group_name_is_escaped() {
        let html = ownership_notices(&[group(1, "<b>F</b>", Some("account_purged"))]);
        assert!(html.contains("&lt;b&gt;F&lt;/b&gt;"), "{html}");
        assert!(!html.contains("<b>F</b>"), "{html}");
    }

    #[test]
    fn the_notice_points_at_the_group_settings() {
        let html = ownership_notices(&[group(1, "F", Some("account_purged"))]);
        assert!(
            html.contains(&format!(
                r#"href="/groups/{}/settings""#,
                Uuid::from_u128(1)
            )),
            "{html}"
        );
    }
}
