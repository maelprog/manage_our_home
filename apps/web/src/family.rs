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

// -- the families the agenda shows (#622) ------------------------------------

/// The agenda's selection of families (#622), kept apart from the active
/// family: choosing to see two families' events changes neither the active
/// family nor the page's colour.
pub const AGENDA_FAMILIES_COOKIE: &str = "agenda_familles";

/// The query key the selector's checkboxes submit, once per box ticked.
pub const FAMILIES_PARAM: &str = "familles";

/// The value of the selector's « Toutes » box.
pub const ALL_FAMILIES: &str = "toutes";

/// What a visitor asked the agenda to show: every family they belong to,
/// or these ones. Ids are what was asked for, not yet checked against
/// `GET /groups` — `resolve_selection` does that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FamilySelection {
    All,
    Only(Vec<Uuid>),
}

/// The selection a set of submitted values stands for. Each value may hold
/// several ids, separated by `,` (a hand-written link) or `.` (the
/// cookie): « Toutes » anywhere wins, an id is kept once, in the order
/// first given, and anything else — an empty value, a malformed id — is
/// ignored, never an error.
pub fn parse_family_selection<'a>(values: impl IntoIterator<Item = &'a str>) -> FamilySelection {
    let mut ids: Vec<Uuid> = Vec::new();
    for part in values.into_iter().flat_map(|v| v.split([',', '.'])) {
        let part = part.trim();
        if part == ALL_FAMILIES {
            return FamilySelection::All;
        }
        if let Ok(id) = part.parse::<Uuid>() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    FamilySelection::Only(ids)
}

/// The selection a query string asks for: `None` when it carries no
/// `familles` key at all — the page then falls back on the cookie — and a
/// selection otherwise, an empty one included (the selector submitted with
/// every box unticked).
pub fn selection_from_query(raw: Option<&str>) -> Option<FamilySelection> {
    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(raw?).ok()?;
    let values: Vec<&str> = pairs
        .iter()
        .filter(|(key, _)| key == FAMILIES_PARAM)
        .map(|(_, value)| value.as_str())
        .collect();
    (!values.is_empty()).then(|| parse_family_selection(values))
}

/// The selection the agenda's cookie remembers, `None` without one.
pub fn selection_from_headers(headers: &HeaderMap) -> Option<FamilySelection> {
    let cookies = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == AGENDA_FAMILIES_COOKIE).then(|| parse_family_selection([value]))
    })
}

/// The families the agenda shows, in the order `GET /groups` lists them:
/// every one for « Toutes », the requested ones the visitor still belongs
/// to otherwise — an id unknown or of a family left is dropped. An empty
/// result, or no selection at all, is the active family alone: the agenda
/// as it was before #622.
pub fn resolve_selection<'a>(
    groups: &'a [GroupSummary],
    active: Option<&'a GroupSummary>,
    requested: Option<&FamilySelection>,
) -> Vec<&'a GroupSummary> {
    let shown: Vec<&GroupSummary> = match requested {
        None => Vec::new(),
        Some(FamilySelection::All) => groups.iter().collect(),
        Some(FamilySelection::Only(ids)) => groups
            .iter()
            .filter(|g| ids.contains(&g.group_id))
            .collect(),
    };
    if shown.is_empty() {
        active.into_iter().collect()
    } else {
        shown
    }
}

/// What the agenda shows, as the links and the cookie carry it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShownFamilies {
    /// The active family alone: nothing to carry.
    Default,
    /// « Toutes »: kept as such, so a family joined later joins the view.
    All,
    /// These families, as resolved.
    Only(Vec<Uuid>),
}

/// How the resolved selection is carried on.
pub fn shown_families(
    requested: Option<&FamilySelection>,
    shown: &[&GroupSummary],
    active: Option<&GroupSummary>,
) -> ShownFamilies {
    if requested == Some(&FamilySelection::All) {
        return ShownFamilies::All;
    }
    match (shown, active) {
        ([], _) => ShownFamilies::Default,
        ([only], Some(active)) if only.group_id == active.group_id => ShownFamilies::Default,
        _ => ShownFamilies::Only(shown.iter().map(|g| g.group_id).collect()),
    }
}

/// The query pairs a link of the agenda adds to keep the selection, without
/// a leading `&` or `?`: empty for the default view.
pub fn families_query(shown: &ShownFamilies) -> String {
    match shown {
        ShownFamilies::Default => String::new(),
        ShownFamilies::All => format!("{FAMILIES_PARAM}={ALL_FAMILIES}"),
        ShownFamilies::Only(ids) => format!("{FAMILIES_PARAM}={}", joined(ids, ",")),
    }
}

fn joined(ids: &[Uuid], separator: &str) -> String {
    ids.iter()
        .map(Uuid::to_string)
        .collect::<Vec<_>>()
        .join(separator)
}

/// The `Set-Cookie` value remembering the selection for a year, or removing
/// the cookie when the selection is back to the default. Same posture as
/// the theme's (`theme.rs`): a UI preference, `Secure`, `HttpOnly` since
/// no script reads it, `SameSite=Lax`.
pub fn families_cookie(shown: &ShownFamilies) -> String {
    // `.` and not `,` between ids: a comma is no cookie-octet (RFC 6265).
    let (value, max_age) = match shown {
        ShownFamilies::Default => (String::new(), 0),
        ShownFamilies::All => (ALL_FAMILIES.to_string(), 31_536_000),
        ShownFamilies::Only(ids) => (joined(ids, "."), 31_536_000),
    };
    format!("{AGENDA_FAMILIES_COOKIE}={value}; Path=/; Max-Age={max_age}; Secure; HttpOnly; SameSite=Lax")
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

    // -- the agenda's selection of families (#622) -----------------------

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn family(n: u128) -> GroupSummary {
        GroupSummary {
            group_id: id(n),
            name: format!("F{n}"),
            role: "owner".to_string(),
            accent: "sarcelle".to_string(),
            ownership_notice: None,
        }
    }

    #[test]
    fn a_selection_is_the_ids_it_names_in_order() {
        let (a, b) = (id(1).to_string(), id(2).to_string());
        assert_eq!(
            parse_family_selection([b.as_str(), a.as_str()]),
            FamilySelection::Only(vec![id(2), id(1)])
        );
    }

    #[test]
    fn several_ids_may_share_a_value() {
        let joined = format!("{},{}", id(1), id(2));
        let dotted = format!("{}.{}", id(3), id(1));
        assert_eq!(
            parse_family_selection([joined.as_str(), dotted.as_str()]),
            FamilySelection::Only(vec![id(1), id(2), id(3)])
        );
    }

    #[test]
    fn an_id_given_twice_is_kept_once() {
        let a = id(1).to_string();
        let both = format!("{},{}", id(1), id(1));
        assert_eq!(
            parse_family_selection([a.as_str(), both.as_str()]),
            FamilySelection::Only(vec![id(1)])
        );
    }

    #[test]
    fn an_empty_or_malformed_value_is_ignored() {
        let a = id(1).to_string();
        assert_eq!(
            parse_family_selection(["", "nope", "12", a.as_str(), " "]),
            FamilySelection::Only(vec![id(1)])
        );
        assert_eq!(
            parse_family_selection(["", "x"]),
            FamilySelection::Only(vec![])
        );
        assert_eq!(parse_family_selection([]), FamilySelection::Only(vec![]));
    }

    #[test]
    fn toutes_anywhere_is_every_family() {
        let a = id(1).to_string();
        assert_eq!(parse_family_selection(["toutes"]), FamilySelection::All);
        assert_eq!(
            parse_family_selection([a.as_str(), "toutes"]),
            FamilySelection::All
        );
        let mixed = format!("{a},toutes");
        assert_eq!(
            parse_family_selection([mixed.as_str()]),
            FamilySelection::All
        );
    }

    #[test]
    fn a_query_without_the_key_asks_for_nothing() {
        assert_eq!(selection_from_query(None), None);
        assert_eq!(selection_from_query(Some("")), None);
        assert_eq!(
            selection_from_query(Some("view=week&date=2026-07-15")),
            None
        );
    }

    #[test]
    fn a_query_reads_every_familles_pair() {
        let raw = format!(
            "view=month&familles=&familles={}&date=2026-07-15&familles={}",
            id(1),
            id(2)
        );
        assert_eq!(
            selection_from_query(Some(&raw)),
            Some(FamilySelection::Only(vec![id(1), id(2)]))
        );
        // Every box unticked: the key alone, an empty selection.
        assert_eq!(
            selection_from_query(Some("familles=")),
            Some(FamilySelection::Only(vec![]))
        );
        // A comma a browser percent-encoded is still a comma.
        let encoded = format!("familles={}%2C{}", id(1), id(2));
        assert_eq!(
            selection_from_query(Some(&encoded)),
            Some(FamilySelection::Only(vec![id(1), id(2)]))
        );
        assert_eq!(
            selection_from_query(Some("familles=toutes")),
            Some(FamilySelection::All)
        );
    }

    fn cookie_headers(cookie: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(axum::http::header::COOKIE, cookie.parse().unwrap());
        headers
    }

    #[test]
    fn the_cookie_is_read_apart_from_the_active_family() {
        let raw = format!(
            "session=s; active_group_id={}; agenda_familles={}.{}",
            id(9),
            id(1),
            id(2)
        );
        assert_eq!(
            selection_from_headers(&cookie_headers(&raw)),
            Some(FamilySelection::Only(vec![id(1), id(2)]))
        );
        assert_eq!(
            selection_from_headers(&cookie_headers("agenda_familles=toutes")),
            Some(FamilySelection::All)
        );
        assert_eq!(selection_from_headers(&cookie_headers("session=s")), None);
        assert_eq!(selection_from_headers(&HeaderMap::new()), None);
    }

    fn ids(shown: &[&GroupSummary]) -> Vec<Uuid> {
        shown.iter().map(|g| g.group_id).collect()
    }

    #[test]
    fn no_selection_is_the_active_family_alone() {
        let groups = [family(1), family(2)];
        let active = Some(&groups[1]);
        assert_eq!(ids(&resolve_selection(&groups, active, None)), [id(2)]);
        assert!(resolve_selection(&[], None, None).is_empty());
    }

    #[test]
    fn toutes_is_every_family_in_the_order_of_the_list() {
        let groups = [family(1), family(2), family(3)];
        assert_eq!(
            ids(&resolve_selection(
                &groups,
                Some(&groups[1]),
                Some(&FamilySelection::All)
            )),
            [id(1), id(2), id(3)]
        );
    }

    #[test]
    fn a_selection_keeps_the_families_of_the_member_in_list_order() {
        let groups = [family(1), family(2), family(3)];
        let asked = FamilySelection::Only(vec![id(3), id(1)]);
        assert_eq!(
            ids(&resolve_selection(&groups, Some(&groups[1]), Some(&asked))),
            [id(1), id(3)]
        );
    }

    /// An id from the URL or the cookie that is not one of the visitor's
    /// families — unknown, foreign, or one they left — is dropped silently.
    #[test]
    fn an_id_outside_the_membership_is_dropped() {
        let groups = [family(1), family(2)];
        let asked = FamilySelection::Only(vec![id(7), id(2)]);
        assert_eq!(
            ids(&resolve_selection(&groups, Some(&groups[0]), Some(&asked))),
            [id(2)]
        );
    }

    #[test]
    fn a_selection_left_empty_falls_back_on_the_active_family() {
        let groups = [family(1), family(2)];
        for asked in [
            FamilySelection::Only(vec![]),
            FamilySelection::Only(vec![id(7), id(8)]),
        ] {
            assert_eq!(
                ids(&resolve_selection(&groups, Some(&groups[1]), Some(&asked))),
                [id(2)],
                "{asked:?}"
            );
        }
    }

    #[test]
    fn the_active_family_alone_carries_nothing() {
        let groups = [family(1), family(2)];
        let active = Some(&groups[0]);
        for asked in [
            None,
            Some(FamilySelection::Only(vec![id(1)])),
            Some(FamilySelection::Only(vec![])),
        ] {
            let shown = resolve_selection(&groups, active, asked.as_ref());
            assert_eq!(
                shown_families(asked.as_ref(), &shown, active),
                ShownFamilies::Default
            );
        }
    }

    #[test]
    fn toutes_is_carried_as_toutes() {
        let groups = [family(1), family(2)];
        let active = Some(&groups[0]);
        let all = FamilySelection::All;
        let shown = resolve_selection(&groups, active, Some(&all));
        assert_eq!(
            shown_families(Some(&all), &shown, active),
            ShownFamilies::All
        );
        // Even for a visitor of one family: a second one joined later is
        // then shown with it.
        let one = [family(1)];
        let shown = resolve_selection(&one, Some(&one[0]), Some(&all));
        assert_eq!(
            shown_families(Some(&all), &shown, Some(&one[0])),
            ShownFamilies::All
        );
    }

    #[test]
    fn other_families_are_carried_as_resolved() {
        let groups = [family(1), family(2), family(3)];
        let active = Some(&groups[0]);
        let asked = FamilySelection::Only(vec![id(3), id(9), id(2)]);
        let shown = resolve_selection(&groups, active, Some(&asked));
        assert_eq!(
            shown_families(Some(&asked), &shown, active),
            ShownFamilies::Only(vec![id(2), id(3)])
        );
        // Another family alone is not the default either.
        let asked = FamilySelection::Only(vec![id(2)]);
        let shown = resolve_selection(&groups, active, Some(&asked));
        assert_eq!(
            shown_families(Some(&asked), &shown, active),
            ShownFamilies::Only(vec![id(2)])
        );
    }

    #[test]
    fn the_query_of_a_selection_is_what_the_selector_submits() {
        assert_eq!(families_query(&ShownFamilies::Default), "");
        assert_eq!(families_query(&ShownFamilies::All), "familles=toutes");
        assert_eq!(
            families_query(&ShownFamilies::Only(vec![id(1), id(2)])),
            format!("familles={},{}", id(1), id(2))
        );
        // Read back, it is the same selection.
        let q = families_query(&ShownFamilies::Only(vec![id(1), id(2)]));
        assert_eq!(
            selection_from_query(Some(&q)),
            Some(FamilySelection::Only(vec![id(1), id(2)]))
        );
    }

    #[test]
    fn the_cookie_remembers_a_selection_and_forgets_the_default() {
        let only = families_cookie(&ShownFamilies::Only(vec![id(1), id(2)]));
        assert_eq!(
            only,
            format!(
                "agenda_familles={}.{}; Path=/; Max-Age=31536000; Secure; HttpOnly; SameSite=Lax",
                id(1),
                id(2)
            )
        );
        assert_eq!(
            families_cookie(&ShownFamilies::All),
            "agenda_familles=toutes; Path=/; Max-Age=31536000; Secure; HttpOnly; SameSite=Lax"
        );
        assert_eq!(
            families_cookie(&ShownFamilies::Default),
            "agenda_familles=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Lax"
        );
        // What it sets is what it reads back.
        let value = only.split(';').next().unwrap();
        assert_eq!(
            selection_from_headers(&cookie_headers(value)),
            Some(FamilySelection::Only(vec![id(1), id(2)]))
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
