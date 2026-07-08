//! Data export (Art. 20, portabilité) — pure JSON-shaping logic.
//!
//! Split from the DB-fetching handler in `mod.rs` so the shape of the
//! export (which categories are included, under which key) is unit
//! testable without a DB, per CLAUDE.md's TDD process. All the values
//! passed in are already scoped to the requesting user (fetched per-group
//! via `scoped_tx`, filtered by `created_by = user_id` / `user_id = user_id`
//! at the SQL layer) — this function only assembles them into one document.

use serde_json::{json, Value};

/// One bag of already-fetched, already-user-scoped category rows (each a
/// `Vec<serde_json::Value>` produced by the handler's per-group queries),
/// passed to `build_export` as a single struct rather than ~10 positional
/// arguments (clippy's `too_many_arguments`).
#[derive(Default)]
pub struct ExportCategories {
    pub group_memberships: Vec<Value>,
    pub agenda_events: Vec<Value>,
    pub stock_items: Vec<Value>,
    pub recipes: Vec<Value>,
    pub meal_history: Vec<Value>,
    pub grocery_items: Vec<Value>,
    pub budget_entries: Vec<Value>,
    pub messages: Vec<Value>,
    pub calendar_imports: Vec<Value>,
}

/// Assembles the final export document from already-fetched, already-user-
/// scoped rows. Deliberately excludes anything not owned/authored by the
/// user even if technically reachable (e.g. other members' messages, other
/// members' stock edits) — export is strictly "my data", not "my family's
/// data" (architecture.md Art. 20 scope).
pub fn build_export(profile: Value, categories: ExportCategories) -> Value {
    json!({
        "profile": profile,
        "group_memberships": categories.group_memberships,
        "agenda_events": categories.agenda_events,
        "stock_items": categories.stock_items,
        "recipes": categories.recipes,
        "meal_history": categories.meal_history,
        "grocery_items": categories.grocery_items,
        "budget_entries": categories.budget_entries,
        "messages": categories.messages,
        "calendar_imports": categories.calendar_imports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_export_nests_every_category_under_its_own_key() {
        let profile = json!({"id": "u1", "email": "a@example.test"});
        let doc = build_export(
            profile.clone(),
            ExportCategories {
                group_memberships: vec![json!({"group_id": "g1"})],
                agenda_events: vec![json!({"id": "e1"})],
                stock_items: vec![json!({"id": "s1"})],
                recipes: vec![json!({"id": "r1"})],
                meal_history: vec![json!({"id": "m1"})],
                grocery_items: vec![json!({"id": "gi1"})],
                budget_entries: vec![json!({"id": "b1"})],
                messages: vec![json!({"id": "msg1"})],
                calendar_imports: vec![json!({"id": "ci1"})],
            },
        );

        assert_eq!(doc["profile"], profile);
        assert_eq!(doc["group_memberships"][0]["group_id"], "g1");
        assert_eq!(doc["agenda_events"][0]["id"], "e1");
        assert_eq!(doc["stock_items"][0]["id"], "s1");
        assert_eq!(doc["recipes"][0]["id"], "r1");
        assert_eq!(doc["meal_history"][0]["id"], "m1");
        assert_eq!(doc["grocery_items"][0]["id"], "gi1");
        assert_eq!(doc["budget_entries"][0]["id"], "b1");
        assert_eq!(doc["messages"][0]["id"], "msg1");
        assert_eq!(doc["calendar_imports"][0]["id"], "ci1");
    }

    #[test]
    fn build_export_handles_empty_categories() {
        let doc = build_export(json!({"id": "u1"}), ExportCategories::default());
        assert!(doc["agenda_events"].as_array().unwrap().is_empty());
        assert!(doc["messages"].as_array().unwrap().is_empty());
    }
}
