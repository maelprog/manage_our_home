pub mod items;

/// Same permission bar as Agenda events: any member may create/read stock
/// items (shared family inventory), but only the item's creator or a group
/// admin/owner may update or delete it.
pub(crate) fn can_modify(actor_role: &str, actor_is_creator: bool) -> bool {
    actor_is_creator || actor_role == "owner" || actor_role == "admin"
}
