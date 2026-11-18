//! Epic 9: Google Calendar import (one-way). See
//! `../../../docs/v1-scope.md` row 9 for the design decision (private ICS
//! feed URL rather than full OAuth2 + Calendar REST API) and
//! `migrations/0010_google_calendar_import.sql` for the schema.

pub mod imports;
pub mod parse;

/// Permission bar for a `calendar_imports` connection, deliberately
/// stricter by default than the "any member / creator-or-admin" bar most
/// other epics use (Stocks, Recipes, Grocery list, Messagerie): the feed URL
/// is a bearer credential for a member's personal Google Calendar, not
/// household data, so *creating* or *deleting* a connection requires the
/// family's `google_calendar` threshold (#865) — admin by default, so that a
/// standard member does not wire a third-party credential into the family's
/// data on their own unless the owner lowered it. Once a connection exists,
/// triggering an on-demand import (a read of the feed, not a credential
/// change) and listing/reading existing connections follows the normal "any
/// member" bar, matching `can_read_import`. One function for the API and
/// the web pages (`apps/shared`), tested there.
pub(crate) use manage_our_home_shared::validation::google_calendar::can_configure;
