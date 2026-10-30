//! Budget screens (front epic F7, issue #22): the family's grocery-derived
//! spend — one month against its caps + its entries (`/budget`, #533), an
//! entry form with a category and grocery items (`/budget/new`), a per-entry
//! edit/delete screen behind a permission bar (`/budget/:id`), and the caps,
//! categories and alert switches (`/budget/settings`, #533). Same SSR pattern as `routes/grocery_list/*` and
//! `routes/recipes/*` — plain `<form method=post>` submissions, per-page error
//! tables mapping `apps/api/src/budget/`'s exact status/error codes to French
//! copy, PRG (`?notice=`/`?error=` codes) after every mutation. Full spec
//! (route table, error tables, the strict-scope / euros-formatting /
//! price-on-checkout / idempotent-upsert decisions) in
//! `docs/front-epic-7-budget.md`.
//!
//! Budget is **tied to the grocery list**, not a standalone expense tracker:
//! the only two ways an entry is created are a manual price entry here and the
//! price-on-checkout action wired into `routes/grocery_list/list.rs` (the hook
//! F6 deferred to F7).
//!
//! Access (#531, `can_access_budget`): only an owner or an admin reaches these
//! pages; a standard member gets the 404 of an unknown path
//! (`family_context`), as the API answers them.
//!
//! Permission bar past it (mirrors `apps/api/src/budget/mod.rs::can_modify`):
//! create, read and set-a-price render for whoever reaches the budget; the
//! per-row edit link and the edit screen's edit/delete controls
//! render only for the entry's creator or a group admin/owner. The backend
//! stays the authority (a forged edit/delete is still 403'd, mapped
//! defensively here).

pub mod edit;
pub mod list;
pub mod new;
pub mod settings;

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::budget::{BudgetEntryResponse, LinkedGroceryItem};
use manage_our_home_shared::dto::grocery_list::GroceryItemResponse;
use manage_our_home_shared::dto::groups::GroupSummary;
use uuid::Uuid;

use crate::app::{shell, Width};
use crate::family::{active_group_id_from_headers, resolve_active_group};
use crate::routes::groups::{cookie_of, header_with_groups};
use crate::state::AppState;

// The permission mirror lives in `apps/shared` (pure, TDD'd) alongside the
// other budget logic; re-export it so the submodules import it from the local
// module like the Grocery-list/Recipes/Stocks pages do.
use manage_our_home_shared::validation::budget::can_access_budget;
pub(crate) use manage_our_home_shared::validation::budget::can_modify;

/// Resolved active-family context for a Budget page: the family id every
/// `/groups/:gid/…` API call is scoped to, the caller's role in it (for the
/// permission bar), and the shared authenticated header.
pub(crate) struct FamilyContext {
    pub gid: Uuid,
    pub role: String,
    pub header: String,
}

/// The active family of a Budget page, or the answer the page gives instead:
/// a redirect to `/groups/new` when the user has no group yet, and for a
/// standard member of the active family (#531) the very 404 an unknown path
/// gets — no route behind `/budget` tells a child that a budget exists.
pub(crate) async fn family_context(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    redirect_to: &str,
) -> Result<FamilyContext, Box<Response>> {
    let (groups, header) = header_with_groups(state, headers, me, redirect_to).await;
    let preferred = active_group_id_from_headers(headers);
    let active: Option<&GroupSummary> = resolve_active_group(&groups, preferred);
    match active {
        None => Err(Box::new(Redirect::to("/groups/new").into_response())),
        Some(g) if !can_access_budget(&g.role) => {
            Err(Box::new(StatusCode::NOT_FOUND.into_response()))
        }
        Some(g) => Ok(FamilyContext {
            gid: g.group_id,
            role: g.role.clone(),
            header,
        }),
    }
}

/// Fetches `cookie_of` for a Budget handler (re-exported so the submodules
/// don't each reach into `routes::groups`).
pub(crate) fn budget_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_of(headers)
}

// -- shared error/landing pages ---------------------------------------------

pub(crate) fn entry_not_found_page() -> axum::response::Html<String> {
    let body = view! {
        <h1>"Dépense introuvable"</h1>
        <p>"Cette dépense n'existe pas ou vous n'y avez pas accès."</p>
        <a class="btn secondary" href="/budget">"Retour au budget"</a>
    };
    axum::response::Html(shell(Width::Form, "Dépense introuvable", &body.to_html()))
}

pub(crate) fn service_unavailable_page() -> axum::response::Html<String> {
    let body = view! {
        <h1>"Service momentanément indisponible"</h1>
        <p>"Merci de réessayer dans quelques instants."</p>
        <a class="btn secondary" href="/budget">"Retour au budget"</a>
    };
    axum::response::Html(shell(Width::Form, "Service indisponible", &body.to_html()))
}

pub(crate) fn forbidden_page() -> axum::response::Html<String> {
    let body = view! {
        <h1>"Action non autorisée"</h1>
        <p>"Vous n'avez pas les droits nécessaires sur cette dépense."</p>
        <a class="btn secondary" href="/budget">"Retour au budget"</a>
    };
    axum::response::Html(shell(Width::Form, "Action non autorisée", &body.to_html()))
}

/// Every `<key>=<uuid>` pair of a raw `application/x-www-form-urlencoded`
/// body, in order. `axum::Form` (`serde_urlencoded`) cannot read one key
/// given several times, which is what a group of same-named checkboxes
/// sends — the agenda's assignees have the same reader
/// (`agenda::new::assignee_ids_from_raw_form`). A canonical UUID needs no
/// percent-decoding: hex digits and hyphens are unreserved.
pub(crate) fn uuids_from_raw_form(raw: &str, key: &str) -> Vec<Uuid> {
    raw.split('&')
        .filter_map(|pair| pair.split_once('='))
        .filter(|(k, _)| *k == key)
        .filter_map(|(_, v)| Uuid::parse_str(v).ok())
        .collect()
}

/// The class that paints a category in its colour (#533): `currentColor`
/// of its bar and of its swatch. Empty for a key the API would not store.
pub(crate) fn category_class(color: &str) -> &'static str {
    match color {
        "blue" => "cat-blue",
        "violet" => "cat-violet",
        "green" => "cat-green",
        "orange" => "cat-orange",
        "ochre" => "cat-ochre",
        _ => "",
    }
}

/// The grocery items an expense may be linked to, in the list's order: the
/// checked ones — bought, so paid for — that no other expense links to,
/// and every item `entry` already links to, checked or not, so that saving
/// the form keeps them. An item is paid once (#633): one linked elsewhere
/// is left out rather than offered for a 409.
pub(crate) fn linkable_items(
    items: &[GroceryItemResponse],
    entries: &[BudgetEntryResponse],
    entry: Option<&BudgetEntryResponse>,
) -> Vec<LinkedGroceryItem> {
    let own: &[LinkedGroceryItem] = entry.map_or(&[], |e| &e.grocery_items);
    let is_own = |id: Uuid| own.iter().any(|i| i.id == id);
    let elsewhere = |id: Uuid| {
        entries
            .iter()
            .filter(|e| entry.is_none_or(|own| own.id != e.id))
            .any(|e| e.grocery_items.iter().any(|i| i.id == id))
    };
    let mut offered: Vec<LinkedGroceryItem> = items
        .iter()
        .filter(|i| is_own(i.id) || (i.checked && !elsewhere(i.id)))
        .map(|i| LinkedGroceryItem {
            id: i.id,
            name: i.name.clone(),
        })
        .collect();
    for linked in own {
        if !offered.iter().any(|i| i.id == linked.id) {
            offered.push(linked.clone());
        }
    }
    offered
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, Utc};

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn item(n: u128, name: &str, checked: bool) -> GroceryItemResponse {
        GroceryItemResponse {
            id: id(n),
            group_id: id(999),
            created_by: id(998),
            name: name.to_string(),
            quantity: None,
            unit: None,
            checked,
            source: "manual".to_string(),
            source_recipe_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn entry(n: u128, linked: &[(u128, &str)]) -> BudgetEntryResponse {
        BudgetEntryResponse {
            id: id(n),
            group_id: id(999),
            created_by: id(998),
            grocery_items: linked
                .iter()
                .map(|(i, name)| LinkedGroceryItem {
                    id: id(*i),
                    name: name.to_string(),
                })
                .collect(),
            category_id: None,
            paid_by: None,
            name: "Dépense".to_string(),
            amount: 1.0,
            spent_at: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn ids(items: &[LinkedGroceryItem]) -> Vec<Uuid> {
        items.iter().map(|i| i.id).collect()
    }

    #[test]
    fn the_raw_form_gives_every_uuid_of_its_key_in_order() {
        let raw = format!(
            "name=Courses&grocery_item_ids={}&amount=3&grocery_item_ids={}&category_id={}",
            id(2),
            id(1),
            id(7)
        );
        assert_eq!(
            uuids_from_raw_form(&raw, "grocery_item_ids"),
            [id(2), id(1)]
        );
        assert_eq!(uuids_from_raw_form(&raw, "category_id"), [id(7)]);
    }

    #[test]
    fn the_raw_form_skips_what_is_no_uuid_and_other_keys() {
        let raw = format!(
            "grocery_item_ids=&grocery_item_ids=abc&x_grocery_item_ids={}",
            id(1)
        );
        assert!(uuids_from_raw_form(&raw, "grocery_item_ids").is_empty());
    }

    #[test]
    fn every_category_colour_has_its_own_class() {
        let classes: Vec<&str> = manage_our_home_shared::validation::budget::CATEGORY_COLORS
            .iter()
            .map(|c| category_class(c))
            .collect();
        assert_eq!(
            classes,
            [
                "cat-blue",
                "cat-violet",
                "cat-green",
                "cat-orange",
                "cat-ochre"
            ]
        );
        assert_eq!(category_class("#ff0000"), "");
    }

    #[test]
    fn a_new_expense_is_offered_the_checked_items_no_expense_links() {
        let items = [
            item(1, "Pain", true),
            item(2, "Lait", false),
            item(3, "Beurre", true),
            item(4, "Œufs", true),
        ];
        let entries = [entry(10, &[(3, "Beurre")])];
        assert_eq!(ids(&linkable_items(&items, &entries, None)), [id(1), id(4)]);
    }

    #[test]
    fn an_expense_keeps_its_own_items_checked_or_not() {
        let items = [
            item(1, "Pain", true),
            item(2, "Lait", false),
            item(3, "Beurre", true),
        ];
        let own = entry(10, &[(2, "Lait"), (3, "Beurre")]);
        let other = entry(11, &[(1, "Pain")]);
        let entries = [own.clone(), other];
        assert_eq!(
            ids(&linkable_items(&items, &entries, Some(&own))),
            [id(2), id(3)]
        );
    }

    #[test]
    fn an_item_the_list_did_not_return_is_still_offered_to_its_expense() {
        let own = entry(10, &[(5, "Café")]);
        let offered = linkable_items(&[], std::slice::from_ref(&own), Some(&own));
        assert_eq!(ids(&offered), [id(5)]);
        assert_eq!(offered[0].name, "Café");
    }
}
