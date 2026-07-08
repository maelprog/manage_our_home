//! Epic 9: Google Calendar import (one-way). See
//! `../../../docs/v1-scope.md` row 9 for the design decision (private ICS
//! feed URL rather than full OAuth2 + Calendar REST API) and
//! `migrations/0010_google_calendar_import.sql` for the schema.

pub mod imports;
pub mod parse;

/// Permission bar for a `calendar_imports` connection, deliberately
/// stricter than the "any member / creator-or-admin" bar every other epic
/// uses (Stocks, Recipes, Grocery list, Budget, Messagerie): the feed URL
/// is a bearer credential for a member's personal Google Calendar, not
/// household data, so *creating* or *deleting* a connection requires an
/// admin/owner role rather than just "any member" — a standard member
/// shouldn't be able to wire a third-party credential into the family's
/// data on their own. Once a connection exists, triggering an on-demand
/// import (a read of the feed, not a credential change) and listing/reading
/// existing connections follows the normal "any member" bar, matching
/// `can_read_import`.
pub(crate) fn can_configure(actor_role: &str) -> bool {
    actor_role == "owner" || actor_role == "admin"
}

#[cfg(test)]
mod tests {
    use super::can_configure;

    #[test]
    fn admin_and_owner_can_configure() {
        assert!(can_configure("admin"));
        assert!(can_configure("owner"));
    }

    #[test]
    fn standard_member_cannot_configure() {
        assert!(!can_configure("standard"));
    }
}
