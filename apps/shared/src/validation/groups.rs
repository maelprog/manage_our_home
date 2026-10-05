//! Pure, dependency-free validation and permission logic for the Groups
//! screens, shared between `apps/web` (client-side gating + inline form
//! feedback) and, where applicable, mirrored from `apps/api`'s enforcement
//! (`apps/api/src/groups/mod.rs`). Written test-first per CLAUDE.md's TDD
//! process — the UI must never disagree with the backend on what is
//! allowed, so the rules live here once.

use super::auth::is_bearer_token;

/// `name_required` when the group name is empty after trimming — the exact
/// rule `apps/api/src/groups/mod.rs::rename_group` enforces (422
/// `name_required`), also applied to creation client-side.
pub fn validate_group_name(name: &str) -> Result<(), &'static str> {
    if name.trim().is_empty() {
        return Err("name_required");
    }
    Ok(())
}

/// Extracts the invitation token from what a user pastes into the
/// "join via invite" form: either the bare token or the full invitation
/// link (`.../groups/invitations/<token>/accept`, the shape
/// `apps/api/src/groups/mod.rs::create_invitation` emails out). Surrounding
/// whitespace is tolerated. `None` when no token spelled as
/// [`is_bearer_token`] wants it can be found (#335).
pub fn parse_invitation_token(input: &str) -> Option<String> {
    let trimmed = input.trim();
    let candidate = if is_bearer_token(trimmed) {
        trimmed
    } else {
        // Full invitation link: take the path segment right after
        // `invitations`.
        let mut segments = trimmed.split('/');
        loop {
            if segments.next()? == "invitations" {
                break segments.next()?;
            }
        }
    };
    is_bearer_token(candidate).then(|| candidate.to_string())
}

/// True for the roles allowed to invite members, rename the group, and see
/// the invite form: `owner` and `admin` — the exact bar
/// `apps/api/src/groups/mod.rs::create_invitation`/`rename_group` enforce
/// (403 otherwise).
pub fn can_manage_group(role: &str) -> bool {
    role == "owner" || role == "admin"
}

/// Mirror of `apps/api/src/groups/mod.rs::actor_can_act_on` (AC #13), used
/// to decide whether to render role-change/remove controls for a member
/// row: the owner can act on anyone but itself; an admin only on standard
/// members; a standard member on no one.
pub fn actor_can_act_on(actor_role: &str, target_role: &str) -> bool {
    if target_role == "owner" {
        return false;
    }
    match actor_role {
        "owner" => true,
        "admin" => target_role != "admin",
        _ => false,
    }
}

/// Why a member became the owner of a group without asking for it (#323),
/// the value of `group_members.ownership_inherited_reason` (migration 0023):
/// the member is told by email and by a notice on the home page, which say
/// which of the three it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipReason {
    /// The previous owner's account was purged (`jobs::account_purge`).
    AccountPurged,
    /// The group had no owner left, and this member's account was just
    /// reactivated by the superadmin.
    MemberReactivated,
    /// The group had no owner left, and the superadmin designated this
    /// member.
    DesignatedBySupport,
}

impl OwnershipReason {
    /// The column's value, as the `CHECK` of migration 0023 spells it.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "account_purged" => Some(OwnershipReason::AccountPurged),
            "member_reactivated" => Some(OwnershipReason::MemberReactivated),
            "designated_by_support" => Some(OwnershipReason::DesignatedBySupport),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OwnershipReason::AccountPurged => "account_purged",
            OwnershipReason::MemberReactivated => "member_reactivated",
            OwnershipReason::DesignatedBySupport => "designated_by_support",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- validate_group_name ---------------------------------------------

    #[test]
    fn empty_or_whitespace_group_name_is_rejected() {
        assert_eq!(validate_group_name(""), Err("name_required"));
        assert_eq!(validate_group_name("   \t\n"), Err("name_required"));
    }

    #[test]
    fn non_empty_group_name_is_accepted() {
        assert_eq!(validate_group_name("Famille Dupont"), Ok(()));
        assert_eq!(validate_group_name("  Maison  "), Ok(()));
    }

    // -- parse_invitation_token --------------------------------------------

    const TOKEN: &str = "Zq3_k9XvT2mQ8pLw4rYb7nHc1sDf6gJ0aEuIoVtBy5M";

    #[test]
    fn bare_token_is_parsed() {
        assert_eq!(parse_invitation_token(TOKEN).as_deref(), Some(TOKEN));
    }

    #[test]
    fn token_with_surrounding_whitespace_is_parsed() {
        assert_eq!(
            parse_invitation_token(&format!("  {TOKEN}\n")).as_deref(),
            Some(TOKEN)
        );
    }

    #[test]
    fn full_invitation_link_is_parsed() {
        let link = format!("https://mondomaine.com/groups/invitations/{TOKEN}/accept");
        assert_eq!(parse_invitation_token(&link).as_deref(), Some(TOKEN));
    }

    /// An invitation issued before #335 carried a UUID: its row no longer
    /// opens anything, so its link is refused here rather than by the api.
    #[test]
    fn former_uuid_token_is_rejected() {
        let uuid = "b6f1a4c2-3d5e-4f60-9a71-8b2c3d4e5f60";
        assert_eq!(parse_invitation_token(uuid), None);
        assert_eq!(
            parse_invitation_token(&format!(
                "https://mondomaine.com/groups/invitations/{uuid}/accept"
            )),
            None
        );
    }

    #[test]
    fn garbage_input_is_rejected() {
        assert_eq!(parse_invitation_token(""), None);
        assert_eq!(parse_invitation_token("not-a-token"), None);
        assert_eq!(
            parse_invitation_token("https://mondomaine.com/groups/invitations//accept"),
            None
        );
        assert_eq!(
            parse_invitation_token(&format!(
                "https://mondomaine.com/groups/invitations/{TOKEN}x/accept"
            )),
            None
        );
    }

    // -- can_manage_group ---------------------------------------------------

    #[test]
    fn owner_and_admin_can_manage_group() {
        assert!(can_manage_group("owner"));
        assert!(can_manage_group("admin"));
    }

    #[test]
    fn standard_or_unknown_role_cannot_manage_group() {
        assert!(!can_manage_group("standard"));
        assert!(!can_manage_group(""));
        assert!(!can_manage_group("superadmin"));
    }

    // -- actor_can_act_on (mirror of apps/api's rule, AC #13) ----------------

    #[test]
    fn owner_can_act_on_admin_and_standard_but_not_owner() {
        assert!(actor_can_act_on("owner", "admin"));
        assert!(actor_can_act_on("owner", "standard"));
        assert!(!actor_can_act_on("owner", "owner"));
    }

    #[test]
    fn admin_can_act_on_standard_only() {
        assert!(actor_can_act_on("admin", "standard"));
        assert!(!actor_can_act_on("admin", "admin"));
        assert!(!actor_can_act_on("admin", "owner"));
    }

    #[test]
    fn standard_cannot_act_on_anyone() {
        assert!(!actor_can_act_on("standard", "standard"));
        assert!(!actor_can_act_on("standard", "admin"));
        assert!(!actor_can_act_on("standard", "owner"));
    }

    // -- OwnershipReason (#323) ---------------------------------------------

    #[test]
    fn the_three_ownership_reasons_round_trip() {
        for (text, reason) in [
            ("account_purged", OwnershipReason::AccountPurged),
            ("member_reactivated", OwnershipReason::MemberReactivated),
            (
                "designated_by_support",
                OwnershipReason::DesignatedBySupport,
            ),
        ] {
            assert_eq!(OwnershipReason::parse(text), Some(reason));
            assert_eq!(reason.as_str(), text);
        }
    }

    #[test]
    fn anything_else_is_not_an_ownership_reason() {
        for text in ["", "purged", "Account_Purged", " account_purged"] {
            assert_eq!(OwnershipReason::parse(text), None, "{text:?}");
        }
    }
}
