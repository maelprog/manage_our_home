//! Wire shapes specific to the RGPD self-service endpoints (front epic F10,
//! issue #25). The export document itself has no DTO on purpose: `apps/web`
//! streams `GET /account/export`'s JSON straight to the browser as a download
//! (see `apps/api/src/rgpd/export.rs` for its structure), so nothing here needs
//! to know its fields — and adding a mirror would be one more place to drift as
//! categories are added.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A group that blocks the caller's account deletion, as listed in
/// `delete_account`'s 409 body: `{"error": "owner_of_groups", "groups": [{"id",
/// "name"}]}` (`apps/api/src/auth/mod.rs`). This is the one error body in the
/// API that carries more than `{"error": "..."}` — the UI needs the names to
/// point the user at each group's settings screen.
///
/// The field is `id`: the API's own `BlockingGroup` names it `group_id`
/// (`SELECT g.id as group_id`), but that struct is only the query target — the
/// response body is built by hand as `json!({"id": g.group_id, "name": g.name})`,
/// so `id` is the wire name this must mirror.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockingGroup {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerOfGroupsError {
    pub error: String,
    #[serde(default)]
    pub groups: Vec<BlockingGroup>,
}
