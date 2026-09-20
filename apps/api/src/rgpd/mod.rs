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
/// parameter exists on this route). Every content category is fetched
/// per-group through `scoped_tx` (RLS-enforced) and additionally filtered by
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

    // "My groups" are read from the caller's own `group_members` rows, with
    // role and joined_at in the same query (issue #209, as `list_groups` since
    // #205). The membership filter is written here and does not rest on RLS:
    // under a role that bypasses it (the superuser of the `e2e` job and of
    // infra/docker-compose.yml) an unfiltered `SELECT … FROM groups` returned
    // every group of the database, and the loop below then ran once per
    // group. Under the role apps/api/README.md prescribes, the policies of
    // 0014 (`group_members_self_read`, `groups_membership_read`) let these
    // rows through with only `app.user_id` set, and apply the same filter a
    // second time. The content tables still require `app.family_id`, hence
    // one `scoped_tx` per group below.
    let mut group_tx = user_scoped_tx(&state.db, auth.user_id).await?;
    let groups = sqlx::query!(
        r#"SELECT g.id, g.name, gm.role AS "role: String", gm.joined_at
           FROM group_members gm
           JOIN groups g ON g.id = gm.group_id
           WHERE gm.user_id = $1
           ORDER BY g.name"#,
        auth.user_id
    )
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
        group_memberships.push(json!({
            "group_id": group.id,
            "name": group.name,
            "role": group.role,
            "joined_at": group.joined_at,
        }));

        let mut tx = scoped_tx(&state.db, group.id, auth.user_id).await?;

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

    let mut audit_tx = crate::db::begin(&state.db).await?;
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
    markdown_document(PRIVACY_POLICY_MD)
}

/// GET /legal-notice — the LCEN art. 6-III notice (publisher, publication
/// director, host), and GET /terms-of-service — the CGU (#132). Same three
/// properties as the privacy policy above, for the same reasons: public, no
/// session, compiled in from `docs/` so the served text and the versioned one
/// cannot drift.
///
/// They live in this module rather than one of their own because what groups
/// the three is the way they are served, not the regulation behind them: one
/// `include_str!`, one public route, one markdown response, rendered by
/// `apps/web` with the same `validation::rgpd::render_markdown`.
const LEGAL_NOTICE_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/legal-notice.md"
));

const TERMS_OF_SERVICE_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/terms-of-service.md"
));

pub async fn legal_notice() -> Response {
    markdown_document(LEGAL_NOTICE_MD)
}

pub async fn terms_of_service() -> Response {
    markdown_document(TERMS_OF_SERVICE_MD)
}

/// The one response shape these three documents share. `text/markdown` and not
/// HTML: rendering is `apps/web`'s job, and the API stays the single source of
/// the text itself.
fn markdown_document(md: &'static str) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        md,
    )
        .into_response()
}
