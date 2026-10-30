//! Active-family selection, persisted client-side in a cookie — issue
//! #17's root-layout requirement. The cookie only stores the *preference*;
//! every page still resolves it against the freshly fetched `GET /groups`
//! list (`resolve_active_group`), so a stale cookie (group left/deleted)
//! silently falls back to the first group the user still belongs to.
//! Family-scoped epics (Agenda, Stocks, ...) read the resolved group id to
//! scope their `/groups/:id/...` API calls.
//!
//! The active family also gives the page its colour (#538): its accent is
//! recorded by the shared header and written on `<html data-accent>`.

use std::cell::Cell;

use axum::extract::Request;
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use manage_our_home_shared::dto::groups::GroupSummary;
use manage_our_home_shared::validation::groups::Accent;
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

/// The accent `group` wears (#538): the identifier its summary carries,
/// sarcelle — the theme's own — for one this build does not know, an
/// apps/api ahead of it. Never an error: the page renders either way.
pub fn accent_of(group: &GroupSummary) -> Accent {
    Accent::parse(&group.accent).unwrap_or_default()
}

/// The name the family settings give each accent.
pub fn accent_label(accent: Accent) -> &'static str {
    match accent {
        Accent::Sarcelle => "Sarcelle",
        Accent::Ocean => "Océan",
        Accent::Prune => "Prune",
        Accent::Foret => "Forêt",
        Accent::Terracotta => "Terracotta",
        Accent::Framboise => "Framboise",
        Accent::Ardoise => "Ardoise",
        Accent::Miel => "Miel",
    }
}

tokio::task_local! {
    /// The active family's accent on the page being served: empty until
    /// the header records it.
    static ACTIVE_ACCENT: Cell<Option<Accent>>;
}

/// Serves `future` with an empty accent slot that
/// [`record_active_accent`] fills and `app::document` reads.
pub async fn scope_accent<F: std::future::Future>(future: F) -> F::Output {
    ACTIVE_ACCENT.scope(Cell::new(None), future).await
}

/// Middleware: every page gets its slot, so that `<html>` can carry the
/// active family's accent (`data-accent`) without each route passing it
/// to the shell. The family is known only once the route has fetched
/// `GET /groups`, which is why it is a slot filled later and not a value
/// read here, as `theme::scope` reads its cookie.
pub async fn accent_scope(request: Request, next: Next) -> Response {
    scope_accent(next.run(request)).await
}

/// Records the active family's accent for the page being served — called
/// by the shared header (`routes::groups::header_parts`), the one place
/// every page with a family resolves it. Outside [`scope_accent`], a unit
/// test rendering a page directly, it records nothing.
pub fn record_active_accent(active: Option<&GroupSummary>) {
    let _ = ACTIVE_ACCENT.try_with(|slot| slot.set(active.map(accent_of)));
}

/// The accent recorded for the page being served, `None` before the
/// header has recorded one, without a family, or outside the scope.
pub fn active_accent() -> Option<Accent> {
    ACTIVE_ACCENT.try_with(Cell::get).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(accent: &str) -> GroupSummary {
        GroupSummary {
            group_id: Uuid::from_u128(1),
            name: "F".to_string(),
            role: "owner".to_string(),
            accent: accent.to_string(),
            ownership_notice: None,
        }
    }

    #[test]
    fn a_familys_accent_is_read_from_its_summary() {
        for accent in Accent::ALL {
            assert_eq!(accent_of(&group(accent.as_str())), accent);
        }
    }

    /// An identifier this build does not know — an API ahead of it — is
    /// the default, never an error.
    #[test]
    fn an_unknown_accent_is_sarcelle() {
        for value in ["", "Ocean", "bleu", "#1f5f99"] {
            assert_eq!(accent_of(&group(value)), Accent::Sarcelle, "{value:?}");
        }
    }

    #[test]
    fn every_accent_is_named_as_the_settings_name_it() {
        let names: Vec<&str> = Accent::ALL.into_iter().map(accent_label).collect();
        assert_eq!(
            names,
            [
                "Sarcelle",
                "Océan",
                "Prune",
                "Forêt",
                "Terracotta",
                "Framboise",
                "Ardoise",
                "Miel"
            ]
        );
    }

    #[tokio::test]
    async fn the_recorded_accent_is_the_active_familys_inside_the_scope_only() {
        // Outside the middleware: nothing recorded, nothing to record into.
        record_active_accent(Some(&group("prune")));
        assert_eq!(active_accent(), None);
        let inside = scope_accent(async {
            let before = active_accent();
            record_active_accent(Some(&group("prune")));
            let after = active_accent();
            record_active_accent(None);
            (before, after, active_accent())
        })
        .await;
        assert_eq!(inside, (None, Some(Accent::Prune), None));
    }
}
