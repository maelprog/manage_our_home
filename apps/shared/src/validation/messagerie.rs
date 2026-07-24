//! Pure, dependency-light logic for the Messagerie screens (front epic F8,
//! issue #23), shared by `apps/web`'s SSR pages. Written test-first per
//! `.claude/CLAUDE.md`'s TDD process. Everything the thread needs beyond
//! rendering lives here so the UI and the fixed backend
//! (`apps/api/src/messagerie/`) can never drift:
//!
//! - `validate_content` mirrors `messages.rs::validate_content` exactly (trim,
//!   non-empty, ≤ 4000 **chars** — not bytes), so the composer rejects locally
//!   with the copy a defensively-mapped backend 400 would produce.
//! - `can_modify` mirrors `apps/api/src/messagerie/mod.rs::can_modify`: the
//!   message's author, or a group owner/admin, may edit or delete it. Reading
//!   and posting are open to any member and are *not* gated here.
//! - `clamp_limit` / `older_page_query` mirror the cursor-pagination contract
//!   (`before_created_at` + `before_id`, default 50, max 100) — the cursor keeps
//!   microsecond precision because Postgres `timestamptz` does.
//! - `format_message_time` renders a UTC instant in **Europe/Paris**, the fixed
//!   v1 display timezone F3 established.
//! - `message_ws_url` turns the browser-facing API base URL into the push
//!   channel's `ws://`/`wss://` URL (a relative base stays relative — the
//!   inline script prefixes `location.host`).
//! - `author_name` resolves a `created_by` id against the family's members,
//!   falling back for an author who has since left (their messages remain).

use chrono::{DateTime, SecondsFormat, Utc};
use chrono_tz::Europe::Paris;
use uuid::Uuid;

use crate::dto::groups::GroupMember;

/// Max message length, mirroring `apps/api/src/messagerie/messages.rs`'s
/// `MAX_CONTENT_CHARS`. Counted in `char`s, after trimming.
pub const MAX_CONTENT_CHARS: usize = 4000;

/// Backend page size defaults, mirroring `DEFAULT_PAGE_LIMIT`/`MAX_PAGE_LIMIT`.
const DEFAULT_PAGE_LIMIT: i64 = 50;
const MAX_PAGE_LIMIT: i64 = 100;

/// Why a message was rejected before it was sent. Same two arms, same order, as
/// the backend's `validate_content`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentError {
    Required,
    TooLong,
}

impl ContentError {
    /// The backend error code this maps to (the front pre-validates, but the
    /// inline message and a defensively-mapped 400 must read identically).
    pub fn code(&self) -> &'static str {
        match self {
            ContentError::Required => "content_required",
            ContentError::TooLong => "content_too_long",
        }
    }
}

/// Mirrors `apps/api/src/messagerie/messages.rs::validate_content`: trims, then
/// rejects empty content and content longer than [`MAX_CONTENT_CHARS`] *chars*
/// (multi-byte characters count as one, same as the backend's
/// `chars().count()`). Returns the trimmed content to send on the wire.
pub fn validate_content(content: &str) -> Result<&str, ContentError> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(ContentError::Required);
    }
    if trimmed.chars().count() > MAX_CONTENT_CHARS {
        return Err(ContentError::TooLong);
    }
    Ok(trimmed)
}

/// Mirror of `apps/api/src/messagerie/mod.rs::can_modify`: the message's
/// author, or a group owner/admin, may edit or delete it. Used to decide
/// whether to render a row's edit/delete controls (the backend still 403s a
/// forged request). Reading the thread and posting are open to any member and
/// are *not* gated by this.
pub fn can_modify(role: &str, is_author: bool) -> bool {
    is_author || role == "owner" || role == "admin"
}

/// Formats a message timestamp in Europe/Paris (`24/07/2026 à 14:05`). The API
/// speaks UTC; Paris is the fixed v1 display timezone (F3's `DISPLAY_TZ`), so
/// the DST handling lives in one tested place.
pub fn format_message_time(dt: DateTime<Utc>) -> String {
    dt.with_timezone(&Paris)
        .format("%d/%m/%Y à %H:%M")
        .to_string()
}

/// Clamps a requested page size to the backend's own bounds (default 50, 1…100
/// — `list_messages` clamps identically, so an out-of-range `?limit=` in the URL
/// can never make the front's cursor link disagree with what it renders).
pub fn clamp_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT)
}

/// Query string for the "older messages" link, built from the **oldest message
/// of the current window**. Microsecond precision is deliberate: Postgres
/// `timestamptz` is microsecond-exact and the backend compares
/// `(created_at, id) < (before_created_at, before_id)`, so a truncated cursor
/// would skip or repeat a message at the page boundary. `limit` is carried
/// through (clamped) only when the caller set one, so the default page size
/// stays out of the URL.
pub fn older_page_query(created_at: DateTime<Utc>, id: Uuid, limit: Option<i64>) -> String {
    let mut q = format!(
        "before_created_at={}&before_id={}",
        created_at.to_rfc3339_opts(SecondsFormat::Micros, true),
        id,
    );
    if let Some(limit) = limit {
        q.push_str(&format!("&limit={}", clamp_limit(Some(limit))));
    }
    q
}

/// The browser-facing WebSocket URL for a family's thread, derived from
/// `API_PUBLIC_BASE_URL` (the same base the Google OAuth button links to):
/// `http` → `ws`, `https` → `wss`, and a relative base (`/api`, the production
/// default behind Caddy) stays relative for the inline script to prefix with
/// `location.host`.
pub fn message_ws_url(api_public_base_url: &str, group_id: Uuid) -> String {
    let base = api_public_base_url.trim_end_matches('/');
    let base = match base.strip_prefix("http://") {
        Some(rest) => format!("ws://{rest}"),
        None => match base.strip_prefix("https://") {
            Some(rest) => format!("wss://{rest}"),
            None => base.to_string(),
        },
    };
    format!("{base}/groups/{group_id}/messages/ws")
}

/// Display name for a message's `created_by`, resolved against the family's
/// members. Falls back to `"Membre"` when the author has left the family (their
/// messages stay in the thread) or when the member list couldn't be fetched —
/// names are decoration, the messages are the content.
pub fn author_name(members: &[GroupMember], user_id: Uuid) -> &str {
    members
        .iter()
        .find(|m| m.user_id == user_id)
        .map(|m| m.display_name.as_str())
        .unwrap_or("Membre")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn member(user_id: Uuid, display_name: &str, role: &str) -> GroupMember {
        GroupMember {
            user_id,
            role: role.to_string(),
            display_name: display_name.to_string(),
            email: format!("{display_name}@example.test"),
        }
    }

    // -- validate_content ----------------------------------------------------

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(validate_content("  bonjour  ").unwrap(), "bonjour");
    }

    #[test]
    fn empty_content_is_rejected() {
        assert_eq!(validate_content(""), Err(ContentError::Required));
    }

    #[test]
    fn whitespace_only_content_is_rejected() {
        assert_eq!(validate_content("   \n\t  "), Err(ContentError::Required));
    }

    #[test]
    fn content_at_the_max_length_is_accepted() {
        let content = "a".repeat(MAX_CONTENT_CHARS);
        assert_eq!(validate_content(&content).unwrap(), content);
    }

    #[test]
    fn content_over_the_max_length_is_rejected() {
        let content = "a".repeat(MAX_CONTENT_CHARS + 1);
        assert_eq!(validate_content(&content), Err(ContentError::TooLong));
    }

    #[test]
    fn the_max_length_counts_chars_not_bytes() {
        // Mirrors the backend's `trimmed.chars().count()`: 4000 accented
        // characters are 4000 chars but 8000 bytes, and must be accepted.
        let content = "é".repeat(MAX_CONTENT_CHARS);
        assert!(validate_content(&content).is_ok());
    }

    #[test]
    fn the_max_length_applies_after_trimming() {
        // Surrounding whitespace is stripped before the length check, same as
        // `apps/api/src/messagerie/messages.rs::validate_content`.
        let content = format!("  {}  ", "a".repeat(MAX_CONTENT_CHARS));
        assert!(validate_content(&content).is_ok());
    }

    #[test]
    fn error_codes_match_the_backend() {
        assert_eq!(ContentError::Required.code(), "content_required");
        assert_eq!(ContentError::TooLong.code(), "content_too_long");
    }

    #[test]
    fn max_content_chars_matches_the_backend() {
        assert_eq!(MAX_CONTENT_CHARS, 4000);
    }

    // -- can_modify ----------------------------------------------------------

    #[test]
    fn author_can_modify_own_message_regardless_of_role() {
        assert!(can_modify("standard", true));
    }

    #[test]
    fn owner_and_admin_can_modify_others_messages() {
        assert!(can_modify("owner", false));
        assert!(can_modify("admin", false));
    }

    #[test]
    fn standard_member_cannot_modify_another_members_message() {
        assert!(!can_modify("standard", false));
    }

    // -- format_message_time -------------------------------------------------

    #[test]
    fn formats_a_utc_instant_in_paris_summer_time() {
        // 2026-07-24 12:05 UTC is 14:05 in Paris (CEST, UTC+2).
        let dt = Utc.with_ymd_and_hms(2026, 7, 24, 12, 5, 0).unwrap();
        assert_eq!(format_message_time(dt), "24/07/2026 à 14:05");
    }

    #[test]
    fn formats_a_utc_instant_in_paris_winter_time() {
        // 2026-01-05 12:05 UTC is 13:05 in Paris (CET, UTC+1) — the display
        // timezone is fixed to Europe/Paris (F3's convention), DST included.
        let dt = Utc.with_ymd_and_hms(2026, 1, 5, 12, 5, 0).unwrap();
        assert_eq!(format_message_time(dt), "05/01/2026 à 13:05");
    }

    #[test]
    fn formatted_time_crosses_the_day_boundary_in_paris() {
        // 23:30 UTC on the 24th is 01:30 on the 25th in Paris.
        let dt = Utc.with_ymd_and_hms(2026, 7, 24, 23, 30, 0).unwrap();
        assert_eq!(format_message_time(dt), "25/07/2026 à 01:30");
    }

    // -- clamp_limit ---------------------------------------------------------

    #[test]
    fn limit_defaults_to_the_backend_default() {
        assert_eq!(clamp_limit(None), 50);
    }

    #[test]
    fn limit_is_clamped_to_the_backend_bounds() {
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(-10)), 1);
        assert_eq!(clamp_limit(Some(101)), 100);
        assert_eq!(clamp_limit(Some(100)), 100);
        assert_eq!(clamp_limit(Some(1)), 1);
        assert_eq!(clamp_limit(Some(25)), 25);
    }

    // -- older_page_query ----------------------------------------------------

    #[test]
    fn older_page_query_carries_both_cursor_halves() {
        let dt = Utc.with_ymd_and_hms(2026, 7, 24, 12, 5, 0).unwrap();
        assert_eq!(
            older_page_query(dt, uuid(1), None),
            "before_created_at=2026-07-24T12:05:00.000000Z\
             &before_id=00000000-0000-0000-0000-000000000001"
        );
    }

    #[test]
    fn older_page_query_keeps_microsecond_precision() {
        // Postgres `timestamptz` is microsecond-exact; a truncated cursor would
        // silently skip or repeat a message at the page boundary.
        let dt = Utc
            .with_ymd_and_hms(2026, 7, 24, 12, 5, 0)
            .unwrap()
            .with_timezone(&Utc)
            + chrono::Duration::microseconds(123_456);
        assert!(older_page_query(dt, uuid(1), None).contains("12:05:00.123456Z"));
    }

    #[test]
    fn older_page_query_preserves_an_explicit_limit() {
        let dt = Utc.with_ymd_and_hms(2026, 7, 24, 12, 5, 0).unwrap();
        let q = older_page_query(dt, uuid(2), Some(2));
        assert!(q.ends_with("&limit=2"), "got {q}");
    }

    #[test]
    fn older_page_query_clamps_the_limit_it_carries() {
        let dt = Utc.with_ymd_and_hms(2026, 7, 24, 12, 5, 0).unwrap();
        assert!(older_page_query(dt, uuid(2), Some(9999)).ends_with("&limit=100"));
    }

    // -- message_ws_url ------------------------------------------------------

    #[test]
    fn ws_url_rewrites_http_to_ws() {
        assert_eq!(
            message_ws_url("http://localhost:8080", uuid(3)),
            "ws://localhost:8080/groups/00000000-0000-0000-0000-000000000003/messages/ws"
        );
    }

    #[test]
    fn ws_url_rewrites_https_to_wss() {
        assert_eq!(
            message_ws_url("https://mondomaine.com/api", uuid(3)),
            "wss://mondomaine.com/api/groups/00000000-0000-0000-0000-000000000003/messages/ws"
        );
    }

    #[test]
    fn ws_url_stays_relative_for_a_relative_base() {
        // Production default (`/api`, behind Caddy): the scheme and host are
        // the browser's, so the inline script prefixes `location.host`.
        assert_eq!(
            message_ws_url("/api", uuid(3)),
            "/api/groups/00000000-0000-0000-0000-000000000003/messages/ws"
        );
    }

    #[test]
    fn ws_url_drops_a_trailing_slash_on_the_base() {
        assert_eq!(
            message_ws_url("http://localhost:8080/", uuid(3)),
            "ws://localhost:8080/groups/00000000-0000-0000-0000-000000000003/messages/ws"
        );
    }

    // -- author_name ---------------------------------------------------------

    #[test]
    fn author_name_resolves_a_member_display_name() {
        let members = vec![
            member(uuid(1), "Alice", "owner"),
            member(uuid(2), "Bob", "standard"),
        ];
        assert_eq!(author_name(&members, uuid(2)), "Bob");
    }

    #[test]
    fn author_name_falls_back_for_someone_who_left_the_family() {
        // Messages outlive membership: the row must still render.
        let members = vec![member(uuid(1), "Alice", "owner")];
        assert_eq!(author_name(&members, uuid(9)), "Membre");
    }

    #[test]
    fn author_name_falls_back_when_the_member_list_is_unavailable() {
        assert_eq!(author_name(&[], uuid(1)), "Membre");
    }
}
