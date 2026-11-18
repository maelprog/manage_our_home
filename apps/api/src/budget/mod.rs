pub mod alerts;
pub mod categories;
pub mod entries;

use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::groups::access::{min_role_for, Feature};
use crate::groups::require_role;

/// Who reaches the budget at all: the family's `budget` threshold (#865),
/// owners and admins by default (#531). One function for the API and the
/// web pages, so the tab a member under the threshold is not shown and the
/// routes they are refused cannot drift apart.
pub(crate) use manage_our_home_shared::validation::budget::can_access_budget;

/// Past `can_access_budget`, the entry's creator or a group admin/owner may
/// edit or delete it. With the default threshold only an admin or an owner
/// gets this far, who passes either way; once the owner lowers the budget's
/// threshold to `standard` (#865), a standard member edits the entries they
/// wrote down and no other.
pub(crate) fn can_modify(actor_role: &str, actor_is_creator: bool) -> bool {
    actor_is_creator || actor_role == "owner" || actor_role == "admin"
}

/// `require_role`, then the budget's own bar, the family's threshold
/// (`can_access_budget`): a non-member is refused as everywhere else (403),
/// a member under the threshold gets 404 — the answer for a group's data
/// the caller is not to know about, so that the routes do not tell a child
/// that there is a budget, nor whether an id is an entry of it. Returns the
/// caller's role. `tx` is scoped to `group_id` (`scoped_tx`).
pub(crate) async fn require_budget_access(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    user_id: Uuid,
) -> AppResult<String> {
    let role = require_role(tx, group_id, user_id).await?;
    let min_role = min_role_for(tx, group_id, Feature::Budget).await?;
    if !can_access_budget(&role, &min_role) {
        return Err(AppError::NotFound);
    }
    Ok(role)
}
