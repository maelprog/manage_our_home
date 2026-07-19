//! Active-family selection, persisted client-side in a cookie — issue
//! #17's root-layout requirement. The cookie only stores the *preference*;
//! every page still resolves it against the freshly fetched `GET /groups`
//! list (`resolve_active_group`), so a stale cookie (group left/deleted)
//! silently falls back to the first group the user still belongs to.
//! Family-scoped epics (Agenda, Stocks, ...) read the resolved group id to
//! scope their `/groups/:id/...` API calls.

use axum::http::HeaderMap;
use manage_our_home_shared::dto::groups::GroupSummary;
use uuid::Uuid;

pub const ACTIVE_GROUP_COOKIE: &str = "active_group_id";

/// Reads the active-family preference from the request's `Cookie` header.
pub fn active_group_id_from_headers(headers: &HeaderMap) -> Option<Uuid> {
    let cookies = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        if name == ACTIVE_GROUP_COOKIE {
            value.parse().ok()
        } else {
            None
        }
    })
}

/// `Set-Cookie` value persisting the active-family preference for a year.
/// Not `HttpOnly`-sensitive (it's a UI preference, not a credential) but
/// marked `HttpOnly` anyway since no client-side JS needs it (apps/web is
/// SSR-only), plus `SameSite=Lax` matching the session cookie's posture.
pub fn set_active_group_cookie(group_id: Uuid) -> String {
    format!("{ACTIVE_GROUP_COOKIE}={group_id}; Path=/; Max-Age=31536000; HttpOnly; SameSite=Lax")
}

/// Resolves the preference against the groups the user actually belongs
/// to: the cookie's group if still a member, else the first group, else
/// `None` (no groups at all).
pub fn resolve_active_group(
    groups: &[GroupSummary],
    preferred: Option<Uuid>,
) -> Option<&GroupSummary> {
    preferred
        .and_then(|id| groups.iter().find(|g| g.group_id == id))
        .or_else(|| groups.first())
}
