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
    pub deactivated_at: Option<DateTime<Utc>>,
    pub deletion_requested_at: Option<DateTime<Utc>>,
    /// The holder's pending reactivation request, if any (#289).
    pub reactivation_requested_at: Option<DateTime<Utc>>,
    pub reactivation_message: Option<String>,
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

    let mut tx = crate::db::begin(&state.admin_db).await?;
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
/// `admin_db` per the spec's "among request handlers, only that gated path
/// is allowed to run on a connection carrying BYPASSRLS" rule (the other
/// user of the pool is the attachment reconcile job, #215, which serves no
/// request), kept uniform across all three superadmin endpoints rather
/// than special-cased per table.
pub async fn list_users(
    State(state): State<AppState>,
    actor: SuperAdminUser,
) -> AppResult<impl IntoResponse> {
    let rows = sqlx::query_as!(
        AdminUserResponse,
        r#"
        SELECT u.id, u.email, u.email_verified, u.created_at, u.deleted_at, u.deactivated_at,
               u.deletion_requested_at, r.requested_at AS "reactivation_requested_at?",
               r.message AS reactivation_message
        FROM users u
        LEFT JOIN account_reactivation_requests r ON r.user_id = u.id
        ORDER BY u.created_at
        "#
    )
    .fetch_all(&state.admin_db)
    .await?;

    let mut tx = crate::db::begin(&state.admin_db).await?;
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
/// period, see the Auth epic) — sets `deactivated_at` and revokes every
/// active session in the same transaction as the audit-log write, so a
/// compromised/abusive account is locked out atomically. Nothing is
/// anonymised: the account purge takes it 2 years later, unless
/// [`reactivate_user`] gives it back first (#256). An account already
/// deactivated, or purged, is a 404.
pub async fn deactivate_user(
    State(state): State<AppState>,
    actor: SuperAdminUser,
    Path(target_user_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = crate::db::begin(&state.admin_db).await?;

    let updated = sqlx::query!(
        r#"UPDATE users SET deactivated_at = now(), deactivation_notice_sent_at = NULL
           WHERE id = $1 AND deactivated_at IS NULL AND deleted_at IS NULL"#,
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

/// Gives back an account [`deactivate_user`] locked (#256): clears
/// `deactivated_at`, and with it the warning of the coming purge, so a
/// later deactivation starts a new 2 years. The sessions revoked at
/// deactivation stay revoked, and so are the restricted sessions opened
/// since (#289) — the holder logs in again. A pending reactivation request
/// is granted by it and deleted. An account that is not deactivated, or
/// already purged (nothing is left to give back), is a 404. Traced in
/// `audit_log` like the deactivation, saying whether it answered a request.
pub async fn reactivate_user(
    State(state): State<AppState>,
    actor: SuperAdminUser,
    Path(target_user_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = crate::db::begin(&state.admin_db).await?;

    let updated = sqlx::query!(
        r#"UPDATE users SET deactivated_at = NULL, deactivation_notice_sent_at = NULL
           WHERE id = $1 AND deactivated_at IS NOT NULL AND deleted_at IS NULL"#,
        target_user_id
    )
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }

    let answered = sqlx::query!(
        "DELETE FROM account_reactivation_requests WHERE user_id = $1",
        target_user_id
    )
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    sqlx::query!(
        "UPDATE sessions SET revoked_at = now()
         WHERE user_id = $1 AND restricted AND revoked_at IS NULL",
        target_user_id
    )
    .execute(&mut *tx)
    .await?;

    audit::record(
        &mut tx,
        Some(actor.user_id),
        "admin.user.reactivate",
        "user",
        &target_user_id.to_string(),
        json!({ "reactivation_request": answered }),
    )
    .await?;

    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Turns down the pending reactivation request of a deactivated account
/// (#289): the request is deleted and the account stays deactivated. The
/// purge 2 years after the deactivation, suspended while the request was
/// pending, applies again; the warning email is cleared so that, if it had
/// already gone out, the holder is warned afresh and the purge still comes
/// at least 30 days after that warning (`jobs::account_purge`). No pending
/// request, or an account reactivated or purged since, is a 404. Traced in
/// `audit_log`.
pub async fn refuse_reactivation(
    State(state): State<AppState>,
    actor: SuperAdminUser,
    Path(target_user_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = crate::db::begin(&state.admin_db).await?;

    let refused = sqlx::query!(
        r#"DELETE FROM account_reactivation_requests r
           USING users u
           WHERE r.user_id = $1 AND u.id = r.user_id
             AND u.deactivated_at IS NOT NULL AND u.deleted_at IS NULL"#,
        target_user_id
    )
    .execute(&mut *tx)
    .await?;
    if refused.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }

    sqlx::query!(
        "UPDATE users SET deactivation_notice_sent_at = NULL WHERE id = $1",
        target_user_id
    )
    .execute(&mut *tx)
    .await?;

    audit::record(
        &mut tx,
        Some(actor.user_id),
        "admin.user.reactivation_refuse",
        "user",
        &target_user_id.to_string(),
        json!({}),
    )
    .await?;

    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
