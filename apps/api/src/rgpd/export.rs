//! Data export (Art. 15 accès, Art. 20 portabilité) — pure logic.
//!
//! Split from the DB-fetching handler in `mod.rs` so the shape of the
//! export (which categories are included, under which key) and the choice
//! of the groups it walks are unit testable without a DB, per CLAUDE.md's
//! TDD process. All the values passed in are already scoped to the
//! requesting user (fetched per-group via `scoped_tx`, filtered at the SQL
//! layer on the column that names them) — `build_export` only assembles
//! them into one document.

use serde_json::{json, Value};
use uuid::Uuid;

/// One bag of already-fetched, already-user-scoped category rows (each a
/// `Vec<serde_json::Value>` produced by the handler's queries), passed to
/// `build_export` as a single struct rather than positional arguments
/// (clippy's `too_many_arguments`).
#[derive(Default)]
pub struct ExportCategories {
    pub group_memberships: Vec<Value>,
    pub former_groups: Vec<Value>,
    pub agenda_events: Vec<Value>,
    pub event_assignments: Vec<Value>,
    pub event_reminders: Vec<Value>,
    pub reminder_notifications: Vec<Value>,
    pub event_attachments: Vec<Value>,
    pub event_completions: Vec<Value>,
    pub stock_items: Vec<Value>,
    pub recipes: Vec<Value>,
    pub meal_history: Vec<Value>,
    pub grocery_items: Vec<Value>,
    pub budget_entries: Vec<Value>,
    pub messages: Vec<Value>,
    pub message_read_state: Vec<Value>,
    pub calendar_imports: Vec<Value>,
    pub invitations_sent: Vec<Value>,
    pub sessions: Vec<Value>,
    pub oauth_identities: Vec<Value>,
    pub email_verifications: Vec<Value>,
    pub password_resets: Vec<Value>,
    pub audit_log: Vec<Value>,
}

/// Assembles the final export document from already-fetched, already-user-
/// scoped rows. Every key is always present, empty or not, so the shape of
/// the file does not depend on what the person happens to have.
pub fn build_export(profile: Value, categories: ExportCategories) -> Value {
    json!({
        "profile": profile,
        "group_memberships": categories.group_memberships,
        "former_groups": categories.former_groups,
        "agenda_events": categories.agenda_events,
        "event_assignments": categories.event_assignments,
        "event_reminders": categories.event_reminders,
        "reminder_notifications": categories.reminder_notifications,
        "event_attachments": categories.event_attachments,
        "event_completions": categories.event_completions,
        "stock_items": categories.stock_items,
        "recipes": categories.recipes,
        "meal_history": categories.meal_history,
        "grocery_items": categories.grocery_items,
        "budget_entries": categories.budget_entries,
        "messages": categories.messages,
        "message_read_state": categories.message_read_state,
        "calendar_imports": categories.calendar_imports,
        "invitations_sent": categories.invitations_sent,
        "sessions": categories.sessions,
        "oauth_identities": categories.oauth_identities,
        "email_verifications": categories.email_verifications,
        "password_resets": categories.password_resets,
        "audit_log": categories.audit_log,
    })
}

/// One group the export walks, and whether the caller is still a member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupScope {
    pub group_id: Uuid,
    pub is_member: bool,
}

/// The groups the export reads, in order: the caller's current groups as
/// given (already sorted by name), then every other group still holding a
/// row that names them — a group they left or were removed from (#140) —
/// in the order given. Each group once.
pub fn export_scope(member_of: &[Uuid], holding_my_rows: &[Uuid]) -> Vec<GroupScope> {
    let mut scope: Vec<GroupScope> = Vec::new();
    let members = member_of.iter().map(|id| (id, true));
    let former = holding_my_rows.iter().map(|id| (id, false));
    for (&group_id, is_member) in members.chain(former) {
        if !scope.iter().any(|g| g.group_id == group_id) {
            scope.push(GroupScope {
                group_id,
                is_member,
            });
        }
    }
    scope
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
                group_memberships: vec![json!({"id": "gm"})],
                former_groups: vec![json!({"id": "fg"})],
                agenda_events: vec![json!({"id": "ev"})],
                event_assignments: vec![json!({"id": "ea"})],
                event_reminders: vec![json!({"id": "er"})],
                reminder_notifications: vec![json!({"id": "rn"})],
                event_attachments: vec![json!({"id": "at"})],
                event_completions: vec![json!({"id": "ec"})],
                stock_items: vec![json!({"id": "si"})],
                recipes: vec![json!({"id": "re"})],
                meal_history: vec![json!({"id": "mh"})],
                grocery_items: vec![json!({"id": "gi"})],
                budget_entries: vec![json!({"id": "be"})],
                messages: vec![json!({"id": "ms"})],
                message_read_state: vec![json!({"id": "mr"})],
                calendar_imports: vec![json!({"id": "ci"})],
                invitations_sent: vec![json!({"id": "is"})],
                sessions: vec![json!({"id": "se"})],
                oauth_identities: vec![json!({"id": "oi"})],
                email_verifications: vec![json!({"id": "ev2"})],
                password_resets: vec![json!({"id": "pr"})],
                audit_log: vec![json!({"id": "al"})],
            },
        );

        assert_eq!(doc["profile"], profile);
        for (key, id) in [
            ("group_memberships", "gm"),
            ("former_groups", "fg"),
            ("agenda_events", "ev"),
            ("event_assignments", "ea"),
            ("event_reminders", "er"),
            ("reminder_notifications", "rn"),
            ("event_attachments", "at"),
            ("event_completions", "ec"),
            ("stock_items", "si"),
            ("recipes", "re"),
            ("meal_history", "mh"),
            ("grocery_items", "gi"),
            ("budget_entries", "be"),
            ("messages", "ms"),
            ("message_read_state", "mr"),
            ("calendar_imports", "ci"),
            ("invitations_sent", "is"),
            ("sessions", "se"),
            ("oauth_identities", "oi"),
            ("email_verifications", "ev2"),
            ("password_resets", "pr"),
            ("audit_log", "al"),
        ] {
            assert_eq!(doc[key][0]["id"], id, "{key}");
        }
        // The profile and the 22 categories, nothing else.
        assert_eq!(doc.as_object().unwrap().len(), 23);
    }

    #[test]
    fn build_export_keeps_every_key_when_empty() {
        let doc = build_export(json!({"id": "u1"}), ExportCategories::default());
        let object = doc.as_object().unwrap();
        assert_eq!(object.len(), 23);
        for (key, value) in object {
            if key != "profile" {
                assert_eq!(value.as_array().map(Vec::len), Some(0), "{key}");
            }
        }
    }

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn member(n: u128) -> GroupScope {
        GroupScope {
            group_id: id(n),
            is_member: true,
        }
    }

    fn former(n: u128) -> GroupScope {
        GroupScope {
            group_id: id(n),
            is_member: false,
        }
    }

    #[test]
    fn export_scope_lists_current_groups_first_in_the_given_order() {
        let scope = export_scope(&[id(2), id(1)], &[id(1), id(2)]);
        assert_eq!(scope, [member(2), member(1),]);
    }

    #[test]
    fn export_scope_adds_the_groups_left_after_the_current_ones() {
        let scope = export_scope(&[id(1)], &[id(3), id(1), id(2)]);
        assert_eq!(scope, [member(1), former(3), former(2),]);
    }

    #[test]
    fn export_scope_walks_a_current_group_holding_nothing_of_mine() {
        // A member who wrote nothing yet still gets their membership.
        let scope = export_scope(&[id(1)], &[]);
        assert_eq!(scope, [member(1)]);
    }

    #[test]
    fn export_scope_lists_each_group_once() {
        let scope = export_scope(&[id(1), id(1)], &[id(2), id(2), id(1)]);
        assert_eq!(scope, [member(1), former(2),]);
    }

    #[test]
    fn export_scope_is_empty_without_any_group() {
        assert!(export_scope(&[], &[]).is_empty());
    }
}
