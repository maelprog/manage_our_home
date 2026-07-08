pub mod items;

/// Same permission bar as Stocks/Recipes/Agenda: any member may create,
/// read, or check off a grocery item, but only the item's creator or a
/// group admin/owner may edit or delete it.
pub(crate) fn can_modify(actor_role: &str, actor_is_creator: bool) -> bool {
    actor_is_creator || actor_role == "owner" || actor_role == "admin"
}
