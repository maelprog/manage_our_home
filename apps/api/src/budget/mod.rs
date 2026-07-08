pub mod entries;

/// Same permission bar as Stocks/Recipes/Grocery list: any member may create
/// or read a budget entry, but only the entry's creator or a group
/// admin/owner may edit or delete it.
pub(crate) fn can_modify(actor_role: &str, actor_is_creator: bool) -> bool {
    actor_is_creator || actor_role == "owner" || actor_role == "admin"
}
