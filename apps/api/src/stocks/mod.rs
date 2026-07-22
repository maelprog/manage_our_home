pub mod items;

/// The full-record-edit / delete permission bar for stock items: only the
/// item's creator or a group admin/owner clears it. Reading, creating and a
/// **quantity-only** adjustment are open to any member (shared family
/// inventory) — those paths don't call this; see
/// `items::UpdateStockItemRequest::touches_full_record`.
pub(crate) fn can_modify(actor_role: &str, actor_is_creator: bool) -> bool {
    actor_is_creator || actor_role == "owner" || actor_role == "admin"
}
