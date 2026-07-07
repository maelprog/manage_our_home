pub mod crud;
pub mod meal_history;
pub mod suggestions;

/// Same permission bar as Stocks/Agenda: any member may create/read a
/// recipe or log a meal, but only the recipe's creator or a group
/// admin/owner may update or delete it.
pub(crate) fn can_modify(actor_role: &str, actor_is_creator: bool) -> bool {
    actor_is_creator || actor_role == "owner" || actor_role == "admin"
}
