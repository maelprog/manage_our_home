//! A family's access policy (#864): from which role its members reach each
//! restricted feature, read by every member and set by the owner alone.
//! The table and its RLS come from #863 (`0062_group_access_policies.sql`).
//! The budget and the Google Calendar connections apply their threshold
//! since #865 (`budget::require_budget_access`,
//! `google_calendar::imports`); since #866 the `can_modify` of stocks,
//! recipes, the grocery list, the agenda and the messagerie admits, besides
//! the creator, a member whose role meets the threshold of `stocks_edit`,
//! `recipes_edit`, `grocery_edit`, `agenda_edit` or `messagerie_moderate`.
//!
//! A feature with no row has the default threshold, `admin`: the bar those
//! checks hard-coded before #866, and the one the budget and Google
//! Calendar held before #865.

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use manage_our_home_shared::dto::groups::{AccessPolicies, AccessPolicy};
use serde_json::json;
use uuid::Uuid;

use super::require_role;
use crate::audit;
use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::AppState;

/// The closed list of features, the default threshold and the rule that
/// compares a role with a threshold live in `apps/shared` (#865), so that
/// `apps/web` hides exactly what this API refuses.
pub use manage_our_home_shared::validation::groups::{role_meets, Feature, DEFAULT_MIN_ROLE};

/// The `&'static str` of a `group_role`, `None` off the list.
fn known_role(role: &str) -> Option<&'static str> {
    ["standard", "admin", "owner"]
        .into_iter()
        .find(|known| *known == role)
}

/// `PUT /groups/:id/access-policies`' body, checked: at least one setting,
/// each feature known and named once, each threshold a `group_role`. The
/// error is the code the route answers with 400.
pub fn validate_changes(
    policies: &[AccessPolicy],
) -> Result<Vec<(Feature, &'static str)>, &'static str> {
    if policies.is_empty() {
        return Err("policies_required");
    }
    let mut changes: Vec<(Feature, &'static str)> = Vec::with_capacity(policies.len());
    for policy in policies {
        let feature = Feature::parse(&policy.feature).ok_or("unknown_feature")?;
        let min_role = known_role(&policy.min_role).ok_or("invalid_role")?;
        if changes.iter().any(|(seen, _)| *seen == feature) {
            return Err("duplicate_feature");
        }
        changes.push((feature, min_role));
    }
    Ok(changes)
}

/// The seven features with their effective threshold, from the rows the
/// family has (`(feature, min_role)`): a feature with no row gets
/// [`DEFAULT_MIN_ROLE`]. A row naming a feature off the list is ignored.
pub fn effective_policies(rows: &[(String, String)]) -> Vec<AccessPolicy> {
    Feature::ALL
        .into_iter()
        .map(|feature| AccessPolicy {
            feature: feature.as_str().to_string(),
            min_role: rows
                .iter()
                .find(|(f, _)| f == feature.as_str())
                .map_or(DEFAULT_MIN_ROLE, |(_, r)| r.as_str())
                .to_string(),
        })
        .collect()
}

/// The threshold of `feature` in `group_id`, read in `tx`, which the caller
/// has scoped to that family: the owner's setting, or [`DEFAULT_MIN_ROLE`].
pub async fn min_role_for(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    feature: Feature,
) -> AppResult<String> {
    let min_role = sqlx::query_scalar!(
        r#"SELECT min_role::text as "min_role!" FROM group_access_policies
           WHERE group_id = $1 AND feature = $2"#,
        group_id,
        feature.as_str()
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(min_role.unwrap_or_else(|| DEFAULT_MIN_ROLE.to_string()))
}

async fn read_policies(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
) -> AppResult<AccessPolicies> {
    let rows = sqlx::query!(
        r#"SELECT feature, min_role as "min_role: String" FROM group_access_policies
           WHERE group_id = $1"#,
        group_id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|row| (row.feature, row.min_role))
    .collect::<Vec<_>>();
    Ok(AccessPolicies {
        policies: effective_policies(&rows),
    })
}

/// The effective policy of `group_id` for `GET /groups` (#865), read in
/// `tx`, a transaction scoped to the caller alone (`user_scoped_tx`), whose
/// membership of `group_id` the caller has checked. The table's RLS admits
/// the rows of the family in `app.group_id` only, so the setting is moved
/// to this family first, for the rest of `tx`.
pub(crate) async fn policies_of_member_group(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
) -> AppResult<Vec<AccessPolicy>> {
    sqlx::query("SELECT set_config('app.group_id', $1, true)")
        .bind(group_id.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(read_policies(tx, group_id).await?.policies)
}

/// `GET /groups/:id/access-policies`: the seven restricted features, each
/// with its effective threshold, the default included. Any member; a
/// non-member gets 403, like the other member routes of a group.
#[utoipa::path(
    get,
    path = "/groups/{id}/access-policies",
    tag = "groups",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
    ),
    responses(
        (status = 200, description = "Every restricted feature with its effective threshold", body = AccessPolicies),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn get_access_policies(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    let policies = read_policies(&mut tx, group_id).await?;
    tx.commit().await?;
    Ok(Json(policies))
}

/// `PUT /groups/:id/access-policies`: sets the threshold of one or more
/// features; a feature left out keeps its own. Owner only, the bar of
/// `transfer_ownership` and `delete_group`: 403 for an admin, a standard
/// member and a non-member. The body is checked first, as on the
/// neighbouring routes: 400 `policies_required` (empty list),
/// `unknown_feature`, `duplicate_feature` or `invalid_role`. Answers the
/// whole policy as `GET` does, and leaves one `access_policies_changed`
/// row in the audit log with the settings sent.
#[utoipa::path(
    put,
    path = "/groups/{id}/access-policies",
    tag = "groups",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
    ),
    request_body = AccessPolicies,
    responses(
        (status = 200, description = "Policy updated; every restricted feature with its effective threshold", body = AccessPolicies),
        (status = 400, description = "`policies_required`, `unknown_feature`, `duplicate_feature` or `invalid_role`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn put_access_policies(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<AccessPolicies>,
) -> AppResult<impl IntoResponse> {
    let changes =
        validate_changes(&body.policies).map_err(|code| AppError::BadRequest(code.into()))?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let role = require_role(&mut tx, group_id, auth.user_id).await?;
    if role != "owner" {
        return Err(AppError::Forbidden);
    }

    let (features, min_roles): (Vec<&str>, Vec<&str>) = changes
        .iter()
        .map(|(feature, min_role)| (feature.as_str(), *min_role))
        .unzip();
    sqlx::query!(
        r#"INSERT INTO group_access_policies (group_id, feature, min_role)
           SELECT $1, f, r::group_role FROM UNNEST($2::text[], $3::text[]) AS t(f, r)
           ON CONFLICT (group_id, feature) DO UPDATE SET min_role = EXCLUDED.min_role"#,
        group_id,
        &features as &[&str],
        &min_roles as &[&str],
    )
    .execute(&mut *tx)
    .await?;

    audit::record(
        &mut tx,
        Some(auth.user_id),
        "access_policies_changed",
        "group",
        &group_id.to_string(),
        json!({
            "policies": changes
                .iter()
                .map(|(feature, min_role)| json!({ "feature": feature.as_str(), "min_role": min_role }))
                .collect::<Vec<_>>(),
        }),
    )
    .await?;

    let policies = read_policies(&mut tx, group_id).await?;
    tx.commit().await?;
    Ok(Json(policies))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROLES: [&str; 3] = ["standard", "admin", "owner"];

    #[test]
    fn role_meets_every_role_against_every_threshold() {
        // (actor, threshold, expected): 3 roles x 3 thresholds.
        let cases = [
            ("standard", "standard", true),
            ("standard", "admin", false),
            ("standard", "owner", false),
            ("admin", "standard", true),
            ("admin", "admin", true),
            ("admin", "owner", false),
            ("owner", "standard", true),
            ("owner", "admin", true),
            ("owner", "owner", true),
        ];
        for (actor, min_role, expected) in cases {
            assert_eq!(
                role_meets(actor, min_role),
                expected,
                "{actor} against {min_role}"
            );
        }
    }

    #[test]
    fn the_owner_passes_even_a_threshold_off_the_list() {
        assert!(role_meets("owner", "superuser"));
        assert!(role_meets("owner", ""));
        assert!(!role_meets("admin", "superuser"));
        assert!(!role_meets("standard", ""));
    }

    #[test]
    fn a_role_off_the_list_reaches_nothing() {
        for min_role in ROLES {
            assert!(!role_meets("superuser", min_role), "{min_role}");
            assert!(!role_meets("", min_role), "{min_role}");
            assert!(!role_meets("Owner", min_role), "{min_role}");
        }
    }

    #[test]
    fn features_round_trip_and_match_the_migration_list() {
        // The list of 0062's `CHECK`, read from the migration itself rather
        // than copied here, so that the two cannot drift apart.
        let migration = include_str!("../../migrations/0062_group_access_policies.sql");
        let check = migration
            .split_once("CHECK (feature IN (")
            .and_then(|(_, rest)| rest.split_once("))"))
            .expect("0062 carries the feature CHECK")
            .0;
        let from_migration: Vec<&str> = check
            .split(',')
            .map(|name| name.trim().trim_matches('\''))
            .collect();
        let names: Vec<&str> = Feature::ALL.iter().map(|f| f.as_str()).collect();
        assert_eq!(names, from_migration);
        for feature in Feature::ALL {
            assert_eq!(Feature::parse(feature.as_str()), Some(feature));
        }
        assert_eq!(Feature::parse("Budget"), None);
        assert_eq!(Feature::parse("stocks"), None);
        assert_eq!(Feature::parse(""), None);
    }

    fn policy(feature: &str, min_role: &str) -> AccessPolicy {
        AccessPolicy {
            feature: feature.to_string(),
            min_role: min_role.to_string(),
        }
    }

    #[test]
    fn validate_changes_accepts_known_features_and_roles() {
        assert_eq!(
            validate_changes(&[policy("budget", "owner"), policy("stocks_edit", "standard")]),
            Ok(vec![
                (Feature::Budget, "owner"),
                (Feature::StocksEdit, "standard")
            ])
        );
        for min_role in ROLES {
            assert!(
                validate_changes(&[policy("agenda_edit", min_role)]).is_ok(),
                "{min_role}"
            );
        }
    }

    #[test]
    fn validate_changes_refuses_what_is_off_the_lists() {
        assert_eq!(validate_changes(&[]), Err("policies_required"));
        assert_eq!(
            validate_changes(&[policy("budget", "owner"), policy("nope", "owner")]),
            Err("unknown_feature")
        );
        for bad in ["superuser", "Owner", ""] {
            assert_eq!(
                validate_changes(&[policy("budget", bad)]),
                Err("invalid_role"),
                "{bad}"
            );
        }
        assert_eq!(
            validate_changes(&[policy("budget", "owner"), policy("budget", "standard")]),
            Err("duplicate_feature")
        );
    }

    #[test]
    fn effective_policies_fill_the_default_in_the_api_order() {
        let all_default = effective_policies(&[]);
        assert_eq!(all_default.len(), 7);
        for (p, feature) in all_default.iter().zip(Feature::ALL) {
            assert_eq!(p.feature, feature.as_str());
            assert_eq!(p.min_role, DEFAULT_MIN_ROLE);
        }

        let rows = vec![
            ("messagerie_moderate".to_string(), "owner".to_string()),
            ("budget".to_string(), "standard".to_string()),
            ("off_the_list".to_string(), "standard".to_string()),
        ];
        let set = effective_policies(&rows);
        let pairs: Vec<(&str, &str)> = set
            .iter()
            .map(|p| (p.feature.as_str(), p.min_role.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("budget", "standard"),
                ("google_calendar", "admin"),
                ("stocks_edit", "admin"),
                ("recipes_edit", "admin"),
                ("grocery_edit", "admin"),
                ("agenda_edit", "admin"),
                ("messagerie_moderate", "owner"),
            ]
        );
    }
}
