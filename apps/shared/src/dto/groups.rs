//! Request/response shapes for the Groups endpoints
//! (`apps/api/src/groups/mod.rs`), shared with `apps/web`'s SSR client.
//! `apps/api` currently builds its list/detail responses with
//! `serde_json::json!` — these structs are kept field-for-field identical
//! to those literals so there is exactly one documented wire shape; only
//! the fields `apps/web` consumes are declared (serde ignores extras like
//! `created_at` on deserialize).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateGroupRequest {
    pub name: String,
}

/// `POST /groups` (201) and `PATCH /groups/:id` (200) response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupResponse {
    pub id: Uuid,
    pub name: String,
}

/// One element of the `GET /groups` array: a group the caller belongs to,
/// with the caller's role in it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupSummary {
    pub group_id: Uuid,
    pub name: String,
    pub role: String,
}

/// One element of `GroupDetailResponse::members`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMember {
    pub user_id: Uuid,
    pub role: String,
    pub display_name: String,
    pub email: String,
}

/// `GET /groups/:id` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupDetailResponse {
    pub id: Uuid,
    pub name: String,
    pub members: Vec<GroupMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameGroupRequest {
    pub name: String,
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
    pub token: Uuid,
}

/// `POST /groups/invitations/:token/accept` (200) response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptInvitationResponse {
    pub group_id: Uuid,
}
