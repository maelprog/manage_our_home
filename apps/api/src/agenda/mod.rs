pub mod attachments;
pub mod events;
pub mod recurrence;
pub mod reminders;

/// Any member of the group may create/read events (v1 doesn't need
/// per-event ACLs — same permission bar as the rest of the family
/// calendar). Only the event's creator or a group admin/owner may update
/// or delete it, mirroring the actor/target role checks already used in
/// `groups::actor_can_act_on`.
pub(crate) fn can_modify(actor_role: &str, actor_is_creator: bool) -> bool {
    actor_is_creator || actor_role == "owner" || actor_role == "admin"
}
