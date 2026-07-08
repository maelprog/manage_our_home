pub mod export;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::{http::header, http::StatusCode, Json};
use serde_json::{json, Value};

use crate::audit;
use crate::auth::session::{scoped_tx, user_scoped_tx, AuthUser};
use crate::error::AppResult;
use crate::AppState;

/// GET /account/export — Art. 20 (portabilité). Self-service: a user can
/// only ever export their own data (scoped by `AuthUser`, no target-user
/// parameter exists on this route). Every category is fetched per-group
/// through `scoped_tx` (RLS-enforced) and additionally filtered by
/// `created_by = auth.user_id` / `user_id = auth.user_id` at the SQL layer,
/// so this is never a "dump my whole family's data" endpoint.
pub async fn export_account(
    State(state): State<AppState>,
    auth: AuthUser,
) -> AppResult<impl IntoResponse> {
    let profile = sqlx::query!(
        "SELECT id, email, email_verified, display_name, created_at FROM users WHERE id = $1",
        auth.user_id
    )
    .fetch_one(&state.db)
    .await?;
    let profile_json = json!({
        "id": profile.id,
        "email": profile.email,
        "email_verified": profile.email_verified,
        "display_name": profile.display_name,
        "created_at": profile.created_at,
    });

    // "My groups" is visible via the `groups` table's membership-based RLS
    // fallback with only `app.user_id` set (see `user_scoped_tx`'s doc
    // comment) — `group_members` itself requires `app.family_id`, so role/
    // joined_at per group is fetched in the per-group loop below instead.
    let mut group_tx = user_scoped_tx(&state.db, auth.user_id).await?;
    let groups = sqlx::query!("SELECT id, name FROM groups ORDER BY name")
        .fetch_all(&mut *group_tx)
        .await?;
    group_tx.commit().await?;

    let mut group_memberships: Vec<Value> = Vec::new();
    let mut agenda_events: Vec<Value> = Vec::new();
    let mut stock_items: Vec<Value> = Vec::new();
    let mut recipes: Vec<Value> = Vec::new();
    let mut meal_history: Vec<Value> = Vec::new();
    let mut grocery_items: Vec<Value> = Vec::new();
    let mut budget_entries: Vec<Value> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    let mut calendar_imports: Vec<Value> = Vec::new();

    for group in &groups {
        let mut tx = scoped_tx(&state.db, group.id, auth.user_id).await?;

        let membership = sqlx::query!(
            "SELECT role AS \"role: String\", joined_at FROM group_members WHERE group_id = $1 AND user_id = $2",
            group.id,
            auth.user_id
        )
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(m) = membership {
            group_memberships.push(json!({
                "group_id": group.id,
                "name": group.name,
                "role": m.role,
                "joined_at": m.joined_at,
            }));
        }

        let events = sqlx::query!(
            r#"SELECT id, group_id, title, description, location, starts_at, ends_at, all_day, is_task, completed_at, rrule, created_at
               FROM events WHERE group_id = $1 AND created_by = $2 ORDER BY created_at"#,
            group.id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        agenda_events.extend(events.into_iter().map(|e| {
            json!({
                "id": e.id, "group_id": e.group_id, "title": e.title, "description": e.description,
                "location": e.location, "starts_at": e.starts_at, "ends_at": e.ends_at,
                "all_day": e.all_day, "is_task": e.is_task, "completed_at": e.completed_at,
                "rrule": e.rrule, "created_at": e.created_at,
            })
        }));

        let stocks = sqlx::query!(
            r#"SELECT id, group_id, name, category, quantity, unit, reorder_threshold, created_at
               FROM stock_items WHERE group_id = $1 AND created_by = $2 ORDER BY created_at"#,
            group.id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        stock_items.extend(stocks.into_iter().map(|s| {
            json!({
                "id": s.id, "group_id": s.group_id, "name": s.name, "category": s.category,
                "quantity": s.quantity, "unit": s.unit, "reorder_threshold": s.reorder_threshold,
                "created_at": s.created_at,
            })
        }));

        let recipe_rows = sqlx::query!(
            r#"SELECT id, group_id, name, instructions, created_at
               FROM recipes WHERE group_id = $1 AND created_by = $2 ORDER BY created_at"#,
            group.id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        recipes.extend(recipe_rows.into_iter().map(|r| {
            json!({
                "id": r.id, "group_id": r.group_id, "name": r.name,
                "instructions": r.instructions, "created_at": r.created_at,
            })
        }));

        let meals = sqlx::query!(
            r#"SELECT id, group_id, recipe_id, eaten_on, created_at
               FROM meal_history WHERE group_id = $1 AND created_by = $2 ORDER BY created_at"#,
            group.id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        meal_history.extend(meals.into_iter().map(|m| {
            json!({
                "id": m.id, "group_id": m.group_id, "recipe_id": m.recipe_id,
                "eaten_on": m.eaten_on, "created_at": m.created_at,
            })
        }));

        let groceries = sqlx::query!(
            r#"SELECT id, group_id, name, quantity, unit, checked, source, created_at
               FROM grocery_items WHERE group_id = $1 AND created_by = $2 ORDER BY created_at"#,
            group.id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        grocery_items.extend(groceries.into_iter().map(|g| {
            json!({
                "id": g.id, "group_id": g.group_id, "name": g.name, "quantity": g.quantity,
                "unit": g.unit, "checked": g.checked, "source": g.source, "created_at": g.created_at,
            })
        }));

        let budget_rows = sqlx::query!(
            r#"SELECT id, group_id, name, amount_cents, spent_at, created_at
               FROM budget_entries WHERE group_id = $1 AND created_by = $2 ORDER BY created_at"#,
            group.id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        budget_entries.extend(budget_rows.into_iter().map(|b| {
            json!({
                "id": b.id, "group_id": b.group_id, "name": b.name,
                "amount": b.amount_cents as f64 / 100.0, "spent_at": b.spent_at,
                "created_at": b.created_at,
            })
        }));

        let message_rows = sqlx::query!(
            r#"SELECT id, group_id, pgp_sym_decrypt(content, $3) as "content!", edited_at, created_at
               FROM messages WHERE group_id = $1 AND created_by = $2 ORDER BY created_at"#,
            group.id,
            auth.user_id,
            state.message_encryption_key,
        )
        .fetch_all(&mut *tx)
        .await?;
        messages.extend(message_rows.into_iter().map(|m| {
            json!({
                "id": m.id, "group_id": m.group_id, "content": m.content,
                "edited_at": m.edited_at, "created_at": m.created_at,
            })
        }));

        // The ICS feed URL is a bearer credential (see 0010's migration
        // comment / `google_calendar::imports`'s doc comment on why the
        // `GET` endpoint never returns it decrypted either) — export
        // includes the import's metadata but not the secret URL itself.
        let calendar_rows = sqlx::query!(
            r#"SELECT id, group_id, label, last_imported_at, created_at
               FROM calendar_imports WHERE group_id = $1 AND created_by = $2 ORDER BY created_at"#,
            group.id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        calendar_imports.extend(calendar_rows.into_iter().map(|c| {
            json!({
                "id": c.id, "group_id": c.group_id, "label": c.label,
                "last_imported_at": c.last_imported_at, "created_at": c.created_at,
            })
        }));

        tx.commit().await?;
    }

    let doc = export::build_export(
        profile_json,
        export::ExportCategories {
            group_memberships,
            agenda_events,
            stock_items,
            recipes,
            meal_history,
            grocery_items,
            budget_entries,
            messages,
            calendar_imports,
        },
    );

    let mut audit_tx = state.db.begin().await?;
    audit::record(
        &mut audit_tx,
        Some(auth.user_id),
        "account_data_exported",
        "user",
        &auth.user_id.to_string(),
        json!({}),
    )
    .await?;
    audit_tx.commit().await?;

    Ok(Json(doc))
}

/// GET /privacy-policy — static document, no auth required (a prospective
/// user needs to be able to read it before registering). Content lives in
/// `docs/privacy-policy.md` (repo root) and is compiled into the binary via
/// `include_str!`, so there's no filesystem dependency at runtime and no
/// drift between what's deployed and what's in source control.
const PRIVACY_POLICY_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/privacy-policy.md"
));

pub async fn privacy_policy() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        PRIVACY_POLICY_MD,
    )
        .into_response()
}
