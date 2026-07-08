use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::user_admin::SuperAdminUser;
use crate::AppState;

#[derive(Serialize)]
pub struct AdminGroupResponse {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub member_count: i64,
}

#[derive(Serialize)]
pub struct AdminUserResponse {
    pub id: Uuid,
    pub email: String,
    pub email_verified: bool,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub deletion_requested_at: Option<DateTime<Utc>>,
}

/// AC #2/#6: lists every group across every family, including ones the
/// superadmin isn't a member of — the one deliberate, gated exception to
/// the `groups`/`group_members` RLS boundary. Runs on `AppState.admin_db`
/// (the `BYPASSRLS` pool), only ever reachable through `SuperAdminUser`.
/// Needed to locate a family when a user reports an issue (spec §Scope).
pub async fn list_groups(
    State(state): State<AppState>,
    actor: SuperAdminUser,
) -> AppResult<impl IntoResponse> {
    let rows = sqlx::query!(
        r#"
        SELECT g.id, g.name, g.created_at, count(gm.user_id) as "member_count!"
        FROM groups g
        LEFT JOIN group_members gm ON gm.group_id = g.id
        GROUP BY g.id, g.name, g.created_at
        ORDER BY g.created_at
        "#
    )
    .fetch_all(&state.admin_db)
    .await?;

    let groups: Vec<AdminGroupResponse> = rows
        .into_iter()
        .map(|r| AdminGroupResponse {
            id: r.id,
            name: r.name,
            created_at: r.created_at,
            member_count: r.member_count,
        })
        .collect();

    let mut tx = state.admin_db.begin().await?;
    audit::record(
        &mut tx,
        Some(actor.user_id),
        "admin.groups.list",
        "groups",
        "*",
        json!({ "count": groups.len() }),
    )
    .await?;
    tx.commit().await?;

    Ok(Json(json!({ "groups": groups })))
}

/// AC #6: lists every user account, for support look-up by email. `users`
/// has no RLS of its own (it isn't family-scoped), but still runs on
/// `admin_db` per the spec's "only that gated path is allowed to run on a
/// connection carrying BYPASSRLS" rule, kept uniform across all three
/// superadmin endpoints rather than special-cased per table.
pub async fn list_users(
    State(state): State<AppState>,
    actor: SuperAdminUser,
) -> AppResult<impl IntoResponse> {
    let rows = sqlx::query_as!(
        AdminUserResponse,
        r#"
        SELECT id, email, email_verified, created_at, deleted_at, deletion_requested_at
        FROM users
        ORDER BY created_at
        "#
    )
    .fetch_all(&state.admin_db)
    .await?;

    let mut tx = state.admin_db.begin().await?;
    audit::record(
        &mut tx,
        Some(actor.user_id),
        "admin.users.list",
        "users",
        "*",
        json!({ "count": rows.len() }),
    )
    .await?;
    tx.commit().await?;

    Ok(Json(json!({ "users": rows })))
}

/// AC #3: an immediate support action, distinct from the self-service
/// `account/delete` flow (which sets `deletion_requested_at` with a grace
/// period, see the Auth epic) — sets `deleted_at` directly and revokes
/// every active session in the same transaction as the audit-log write,
/// so a compromised/abusive account is locked out atomically.
pub async fn deactivate_user(
    State(state): State<AppState>,
    actor: SuperAdminUser,
    Path(target_user_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = state.admin_db.begin().await?;

    let updated = sqlx::query!(
        "UPDATE users SET deleted_at = now() WHERE id = $1 AND deleted_at IS NULL",
        target_user_id
    )
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }

    sqlx::query!(
        "UPDATE sessions SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
        target_user_id
    )
    .execute(&mut *tx)
    .await?;

    audit::record(
        &mut tx,
        Some(actor.user_id),
        "admin.user.deactivate",
        "user",
        &target_user_id.to_string(),
        json!({}),
    )
    .await?;

    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
