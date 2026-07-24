//! Response shapes for the superadmin support endpoints
//! (`apps/api/src/user_admin/admin.rs`), shared with `apps/web`'s SSR client
//! (front epic F9, issue #24). `apps/api` returns each list wrapped in a
//! single-key object (`{"groups": [...]}` / `{"users": [...]}`) built with
//! `serde_json::json!` around the `AdminGroupResponse` / `AdminUserResponse`
//! serialize structs — these mirror those field-for-field so there is exactly
//! one documented wire shape. Only the fields `apps/web` consumes are declared
//! (serde ignores extras on deserialize).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One row of `GET /admin/groups` — a family across every tenant, with its
/// member count (the one gated exception to the RLS boundary, see
/// `apps/api/src/user_admin/admin.rs::list_groups`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminGroupResponse {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub member_count: i64,
}

/// `GET /admin/groups` response envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminGroupsResponse {
    pub groups: Vec<AdminGroupResponse>,
}

/// One row of `GET /admin/users` — an account for support look-up. `deleted_at`
/// is set by a superadmin `deactivate` or a completed self-service deletion;
/// `deletion_requested_at` marks a pending grace-period deletion request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminUserResponse {
    pub id: Uuid,
    pub email: String,
    pub email_verified: bool,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub deletion_requested_at: Option<DateTime<Utc>>,
}

/// `GET /admin/users` response envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminUsersResponse {
    pub users: Vec<AdminUserResponse>,
}
