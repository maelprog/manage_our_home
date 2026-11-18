//! Request/response shapes for the Groups endpoints
//! (`apps/api/src/groups/mod.rs`), shared with `apps/web`'s SSR client.
//! `apps/api` currently builds its list/detail responses with
//! `serde_json::json!` — these structs are kept field-for-field identical
//! to those literals so there is exactly one documented wire shape; only
//! the fields `apps/web` consumes are declared (serde ignores extras like
//! `created_at` on deserialize).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::validation::groups::{Accent, Feature, DEFAULT_MIN_ROLE};

/// The accent of a family when an older API omits it: the default one, the
/// accent the theme had before families could choose (#537).
fn default_accent() -> String {
    Accent::default().as_str().to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateGroupRequest {
    pub name: String,
}

/// `POST /groups` (201) and `PATCH /groups/:id` (200) response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GroupResponse {
    pub id: Uuid,
    pub name: String,
    /// The family's accent (#537), an `Accent` identifier.
    #[serde(default = "default_accent")]
    pub accent: String,
    /// The family's school-holiday zone (#561), a `SchoolZone` letter, or
    /// `None` for no zone.
    #[serde(default)]
    pub school_zone: Option<String>,
    /// Whether the family keeps the Alsace-Moselle public holidays (#561).
    #[serde(default)]
    pub alsace_moselle: bool,
}

/// One element of the `GET /groups` array: a group the caller belongs to,
/// with the caller's role in it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GroupSummary {
    pub group_id: Uuid,
    pub name: String,
    pub role: String,
    /// The family's accent (#537), an `Accent` identifier, chosen by its
    /// owner or admins.
    #[serde(default = "default_accent")]
    pub accent: String,
    /// The caller became this group's owner without asking for it and has
    /// not acknowledged it yet (#323); `None` otherwise.
    #[serde(default)]
    pub ownership_notice: Option<OwnershipNotice>,
    /// The family's restricted features with their effective threshold
    /// (#865), as `GET /groups/:id/access-policies` answers them; read
    /// through [`GroupSummary::min_role`].
    #[serde(default)]
    pub access_policies: Vec<AccessPolicy>,
}

impl GroupSummary {
    /// The threshold of `feature` in this family: the one the API sent, or
    /// [`DEFAULT_MIN_ROLE`] when it sent none for it.
    pub fn min_role(&self, feature: Feature) -> &str {
        self.access_policies
            .iter()
            .find(|p| p.feature == feature.as_str())
            .map_or(DEFAULT_MIN_ROLE, |p| p.min_role.as_str())
    }
}

/// `GroupSummary::ownership_notice`: why and when the group came to the
/// caller. `reason` is a `validation::groups::OwnershipReason` value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OwnershipNotice {
    pub reason: String,
    pub inherited_at: DateTime<Utc>,
}

/// One element of `GroupDetailResponse::members`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GroupMember {
    pub user_id: Uuid,
    pub role: String,
    pub display_name: String,
    pub email: String,
    /// Whether the member has a profile photo (#468), served by
    /// `GET /groups/:id/members/:user_id/avatar`. `false` when an older API
    /// omits it: the initial is then shown, as before photos existed.
    #[serde(default)]
    pub has_avatar: bool,
}

/// `GET /groups/:id` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GroupDetailResponse {
    pub id: Uuid,
    pub name: String,
    /// The family's school-holiday zone (#561), a `SchoolZone` letter;
    /// `None` when it has none, or when an older API omits it: no school
    /// holidays are then shown.
    #[serde(default)]
    pub school_zone: Option<String>,
    /// Whether the family keeps the Alsace-Moselle public holidays (#561);
    /// `false` when an older API omits it.
    #[serde(default)]
    pub alsace_moselle: bool,
    pub members: Vec<GroupMember>,
}

/// `GET /groups/:id/members/:user_id` response (#466): another member's
/// profile, as a member of the same group sees it. No e-mail (arbitrated in
/// the issue): the profile is reachable from every name on the messagerie
/// and the agenda, wider than the member list that does show it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GroupMemberProfile {
    pub user_id: Uuid,
    pub display_name: String,
    pub role: String,
    pub joined_at: DateTime<Utc>,
    /// Whether the member has a profile photo (#468), as in `GroupMember`.
    #[serde(default)]
    pub has_avatar: bool,
}

/// `PATCH /groups/:id`: the settings to change, each optional — the name,
/// the accent (#537), the calendar settings (#561), in any combination.
/// Owner or admin only.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RenameGroupRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// An `Accent` identifier; anything else is refused with 400.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    /// `None`: kept (key omitted); `Some(None)`: cleared (`null`, no school
    /// holidays); `Some(Some(_))`: a `SchoolZone` letter, anything else is
    /// refused with 400.
    #[serde(
        default,
        deserialize_with = "deserialize_some",
        skip_serializing_if = "Option::is_none"
    )]
    pub school_zone: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alsace_moselle: Option<bool>,
}

/// Serde's blanket `Option<T>` impl reads an explicit JSON `null` and a
/// missing key as the same `None`; this `deserialize_with` only runs when the
/// key is present, so `null` becomes `Some(None)` (as in `dto/stocks.rs`).
fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TransferOwnershipRequest {
    pub new_owner_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ChangeRoleRequest {
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LeaveGroupRequest {
    pub new_owner_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateInvitationRequest {
    pub invited_email: Option<String>,
}

/// `POST /groups/:id/invitations` (201) response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct InvitationCreatedResponse {
    pub id: Uuid,
    pub token: String,
    /// The link to share, absolute (#462): the same as the email carries,
    /// built from apps/api's `FRONTEND_BASE_URL`.
    pub accept_url: String,
}

/// One restricted feature of a family and its threshold (#864): `feature`,
/// one of `budget`, `google_calendar`, `stocks_edit`, `recipes_edit`,
/// `grocery_edit`, `agenda_edit`, `messagerie_moderate`; `min_role`, the
/// lowest role admitted, `standard`, `admin` or `owner`. The owner is
/// always admitted, whatever the threshold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AccessPolicy {
    pub feature: String,
    pub min_role: String,
}

/// `GET /groups/:id/access-policies` and `PUT`'s response (#864): the seven
/// features, each with its effective threshold (`admin` when the owner has
/// not set it). `PUT`'s body: the features to change, at least one, each
/// named once; the others keep theirs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AccessPolicies {
    pub policies: Vec<AccessPolicy>,
}

/// `POST /groups/invitations/:token/accept` (200) response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AcceptInvitationResponse {
    pub group_id: Uuid,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(policies: &[(&str, &str)]) -> GroupSummary {
        GroupSummary {
            group_id: Uuid::nil(),
            name: "F".to_string(),
            role: "standard".to_string(),
            accent: default_accent(),
            ownership_notice: None,
            access_policies: policies
                .iter()
                .map(|(feature, min_role)| AccessPolicy {
                    feature: feature.to_string(),
                    min_role: min_role.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn min_role_reads_the_familys_threshold() {
        let g = summary(&[("google_calendar", "owner"), ("budget", "standard")]);
        assert_eq!(g.min_role(Feature::Budget), "standard");
        assert_eq!(g.min_role(Feature::GoogleCalendar), "owner");
    }

    #[test]
    fn min_role_falls_back_to_the_default_threshold() {
        // An API that sends no policy, or not this feature's: `admin`, the
        // bar a family has until its owner sets another (#864).
        assert_eq!(summary(&[]).min_role(Feature::Budget), "admin");
        assert_eq!(
            summary(&[("stocks_edit", "standard")]).min_role(Feature::Budget),
            "admin"
        );
    }
}
