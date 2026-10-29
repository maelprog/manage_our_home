pub mod categories;
pub mod entries;

use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::groups::require_role;

/// Who reaches the budget at all (#531): owners and admins. One function for
/// the API and the web pages, so the tab a standard member is not shown and
/// the routes they are refused cannot drift apart.
pub(crate) use manage_our_home_shared::validation::budget::can_access_budget;

/// Past `can_access_budget`, the entry's creator or a group admin/owner may
/// edit or delete it. Since #531 only an admin or an owner gets this far, who
/// passes either way; the creator arm stays so the rule still holds should
/// the access bar ever widen.
pub(crate) fn can_modify(actor_role: &str, actor_is_creator: bool) -> bool {
    actor_is_creator || actor_role == "owner" || actor_role == "admin"
}

/// `require_role`, then the budget's own bar: a non-member is refused as
/// everywhere else (403), a standard member gets 404 — the answer for a
/// group's data the caller is not to know about, so that the routes do not
/// tell a child that there is a budget, nor whether an id is an entry of it.
/// Returns the caller's role.
pub(crate) async fn require_budget_access(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    user_id: Uuid,
) -> AppResult<String> {
    let role = require_role(tx, group_id, user_id).await?;
    if !can_access_budget(&role) {
        return Err(AppError::NotFound);
    }
    Ok(role)
}
