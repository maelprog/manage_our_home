pub mod attachments;
pub mod change_notices;
pub mod events;
pub mod national_days;
pub mod recurrence;
pub mod reminders;
pub mod school_holidays;
pub mod thumbnail;

use uuid::Uuid;

use crate::groups::access::role_meets;

/// Any member of the group may create/read the events they can see. Only
/// the event's creator or a member whose role meets `min_role`, the
/// family's `agenda_edit` threshold (#866, `admin` by default), may update
/// or delete it — plus, on a private event (#620), any of its
/// participants: such an event is only ever visible to them
/// (`events_isolation`, migration 0044), and one of them unable to correct
/// it would leave nobody but its creator who could.
pub(crate) fn can_modify(
    actor_role: &str,
    min_role: &str,
    actor_is_creator: bool,
    actor_is_private_participant: bool,
) -> bool {
    actor_is_creator || role_meets(actor_role, min_role) || actor_is_private_participant
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

    /// Every role × threshold × authorship (#866), written out rather than
    /// derived: the creator always may; anyone else when their role meets
    /// the family's threshold, the owner whatever it says.
    const CASES: [(&str, &str, bool, bool); 18] = [
        ("standard", "standard", false, true),
        ("standard", "admin", false, false),
        ("standard", "owner", false, false),
        ("admin", "standard", false, true),
        ("admin", "admin", false, true),
        ("admin", "owner", false, false),
        ("owner", "standard", false, true),
        ("owner", "admin", false, true),
        ("owner", "owner", false, true),
        ("standard", "standard", true, true),
        ("standard", "admin", true, true),
        ("standard", "owner", true, true),
        ("admin", "standard", true, true),
        ("admin", "admin", true, true),
        ("admin", "owner", true, true),
        ("owner", "standard", true, true),
        ("owner", "admin", true, true),
        ("owner", "owner", true, true),
    ];

    #[test]
    fn can_modify_follows_the_threshold_and_spares_the_creator() {
        for (role, min_role, creator, expected) in CASES {
            assert_eq!(
                can_modify(role, min_role, creator, false),
                expected,
                "{role} under {min_role}, creator: {creator}"
            );
        }
    }

    #[test]
    fn a_participant_of_a_private_event_may_modify_it_whatever_the_threshold() {
        for min_role in ["standard", "admin", "owner"] {
            assert!(can_modify("standard", min_role, false, true), "{min_role}");
        }
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
