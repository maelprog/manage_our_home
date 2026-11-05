pub mod attachments;
pub mod events;
pub mod national_days;
pub mod recurrence;
pub mod reminders;
pub mod school_holidays;

use uuid::Uuid;

/// Any member of the group may create/read the events they can see. Only
/// the event's creator or a group admin/owner may update or delete it,
/// mirroring the actor/target role checks already used in
/// `groups::actor_can_act_on` — plus, on a private event (#620), any of its
/// participants: such an event is only ever visible to them
/// (`events_isolation`, migration 0044), and one of them unable to correct
/// it would leave nobody but its creator who could.
pub(crate) fn can_modify(
    actor_role: &str,
    actor_is_creator: bool,
    actor_is_private_participant: bool,
) -> bool {
    actor_is_creator
        || actor_role == "owner"
        || actor_role == "admin"
        || actor_is_private_participant
}

/// The participants of an event (#620): its creator and the members of its
/// "pour qui" (`event_assignees`). The same rule the `events` policy applies
/// in SQL (`event_is_assigned_to_current_user`, migration 0044); this one
/// decides the writes the policy would otherwise refuse with an error.
pub(crate) fn is_participant(user: Uuid, created_by: Uuid, assignees: &[Uuid]) -> bool {
    user == created_by || assignees.contains(&user)
}

/// Whether a write may leave the event with the privacy it asks for (#620).
///
/// Making an event private hides it from everyone but its participants, the
/// author of the write included: one who would not be a participant
/// afterwards is refused, rather than writing a row the policy's check
/// rejects. An event that already was private is left to the rule above —
/// a participant may well take themselves off its "pour qui", and lose it.
pub(crate) fn may_set_privacy(
    was_private: bool,
    will_be_private: bool,
    actor_participant_after: bool,
) -> bool {
    was_private || !will_be_private || actor_participant_after
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uid(n: u8) -> Uuid {
        Uuid::from_u128(u128::from(n))
    }

    // -- can_modify -------------------------------------------------------

    #[test]
    fn the_creator_may_modify() {
        assert!(can_modify("standard", true, false));
    }

    #[test]
    fn an_admin_or_owner_may_modify_a_shared_event() {
        assert!(can_modify("admin", false, false));
        assert!(can_modify("owner", false, false));
    }

    #[test]
    fn a_plain_member_may_not_modify_someone_elses_shared_event() {
        assert!(!can_modify("standard", false, false));
    }

    #[test]
    fn a_participant_of_a_private_event_may_modify_it() {
        assert!(can_modify("standard", false, true));
    }

    // -- is_participant ---------------------------------------------------

    #[test]
    fn the_creator_is_a_participant_even_off_the_assignees() {
        assert!(is_participant(uid(1), uid(1), &[uid(2)]));
    }

    #[test]
    fn an_assignee_is_a_participant() {
        assert!(is_participant(uid(2), uid(1), &[uid(3), uid(2)]));
    }

    #[test]
    fn anyone_else_is_not_a_participant() {
        assert!(!is_participant(uid(4), uid(1), &[uid(2), uid(3)]));
        assert!(!is_participant(uid(4), uid(1), &[]));
    }

    // -- may_set_privacy --------------------------------------------------

    #[test]
    fn making_an_event_private_needs_the_actor_among_its_participants() {
        assert!(may_set_privacy(false, true, true));
        assert!(!may_set_privacy(false, true, false));
    }

    #[test]
    fn a_shared_event_staying_shared_is_always_allowed() {
        assert!(may_set_privacy(false, false, false));
    }

    #[test]
    fn a_private_event_may_be_left_by_a_participant() {
        assert!(may_set_privacy(true, true, false));
    }

    #[test]
    fn a_private_event_may_be_made_shared_again() {
        assert!(may_set_privacy(true, false, false));
        assert!(may_set_privacy(true, false, true));
    }
}
