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

use crate::validation::groups::Accent;

/// The accent of a family when an older API omits it: the default one, the
/// accent the theme had before families could choose (#537).
fn default_accent() -> String {
    Accent::default().as_str().to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateGroupRequest {
    pub name: String,
}

/// `POST /groups` (201) and `PATCH /groups/:id` (200) response.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

/// `GroupSummary::ownership_notice`: why and when the group came to the
/// caller. `reason` is a `validation::groups::OwnershipReason` value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnershipNotice {
    pub reason: String,
    pub inherited_at: DateTime<Utc>,
}

/// One element of `GroupDetailResponse::members`.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
pub struct TransferOwnershipRequest {
    pub new_owner_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeRoleRequest {
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaveGroupRequest {
    pub new_owner_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInvitationRequest {
    pub invited_email: Option<String>,
}

/// `POST /groups/:id/invitations` (201) response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvitationCreatedResponse {
    pub id: Uuid,
    pub token: String,
    /// The link to share, absolute (#462): the same as the email carries,
    /// built from apps/api's `FRONTEND_BASE_URL`.
    pub accept_url: String,
}

/// `POST /groups/invitations/:token/accept` (200) response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptInvitationResponse {
    pub group_id: Uuid,
}
