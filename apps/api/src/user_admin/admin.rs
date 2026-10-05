use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::groups::succession::{self, check_designation, DesignationRefusal};
use crate::jobs::account_purge::{record_refusal, RefusalStamps};
use crate::user_admin::SuperAdminUser;
use crate::AppState;
use manage_our_home_shared::validation::groups::OwnershipReason;

#[derive(Serialize)]
pub struct AdminGroupResponse {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub member_count: i64,
    /// False for a group the purge left without an owner (#323).
    pub has_owner: bool,
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
    /// When a request was first refused since the deactivation (#289); a
    /// later refusal does not rewrite it (#313).
    pub reactivation_refused_at: Option<DateTime<Utc>>,
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
        SELECT g.id, g.name, g.created_at, count(gm.user_id) as "member_count!",
               COALESCE(bool_or(gm.role = 'owner'), false) AS "has_owner!"
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
            has_owner: r.has_owner,
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
/// is allowed to run on a connection carrying BYPASSRLS" rule (the pool's
/// other users are the four background jobs — attachment reconcile #215,
/// retention purge #138, account purge #139, reminder worker #293 — none
/// of which serves a request), kept uniform across all seven superadmin
/// endpoints rather than special-cased per table.
pub async fn list_users(
    State(state): State<AppState>,
    actor: SuperAdminUser,
) -> AppResult<impl IntoResponse> {
    let rows = sqlx::query_as!(
        AdminUserResponse,
        r#"
        SELECT u.id, u.email, u.email_verified, u.created_at, u.deleted_at, u.deactivated_at,
               u.deletion_requested_at, r.requested_at AS "reactivation_requested_at?",
               r.message AS reactivation_message, u.reactivation_refused_at
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
        r#"UPDATE users SET deactivated_at = now(), deactivation_notice_sent_at = NULL,
                            reactivation_refused_at = NULL
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
        r#"UPDATE users SET deactivated_at = NULL, deactivation_notice_sent_at = NULL,
                            reactivation_refused_at = NULL
           WHERE id = $1 AND deactivated_at IS NOT NULL AND deleted_at IS NULL"#,
        target_user_id
    )
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }

    // A group the purge left without an owner, of which this account is a
    // member, gets one again: `successor` runs on its members as they are
    // now (#323) — this account, if it comes first.
    let ownerless = sqlx::query_scalar!(
        r#"SELECT gm.group_id FROM group_members gm
           WHERE gm.user_id = $1
             AND NOT EXISTS (SELECT 1 FROM group_members o
                             WHERE o.group_id = gm.group_id AND o.role = 'owner')"#,
        target_user_id
    )
    .fetch_all(&mut *tx)
    .await?;
    for group_id in ownerless {
        succession::assign_heir(&mut tx, group_id, OwnershipReason::MemberReactivated).await?;
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
/// purge 2 years after the deactivation, suspended while a first request
/// was pending, applies again. The first refusal since the deactivation
/// stamps `reactivation_refused_at`: the holder may ask again, but a later
/// request suspends nothing (arbitrage of 2026-09-29). It also clears the
/// warning email so that, if it had already gone out, the holder is warned
/// afresh once the purge is 30 days away or less, and the purge still comes
/// at least 30 days after that warning — and at least 30 days after this
/// refusal, whether the warning goes out or not (#296). That postponement
/// happens once: a later refusal deletes the request and rewrites nothing
/// else, so it moves the purge date no further (#313,
/// `jobs::account_purge::record_refusal`). No pending request, or an
/// account reactivated or purged since, is a 404. Traced in `audit_log`.
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

    // Locked so that the purge job's warning cannot land between the read
    // and the write.
    let row = sqlx::query!(
        r#"SELECT reactivation_refused_at, deactivation_notice_sent_at, now() AS "now!"
           FROM users WHERE id = $1 FOR UPDATE"#,
        target_user_id
    )
    .fetch_one(&mut *tx)
    .await?;
    let after = record_refusal(
        RefusalStamps {
            reactivation_refused_at: row.reactivation_refused_at,
            deactivation_notice_sent_at: row.deactivation_notice_sent_at,
        },
        row.now,
    );
    sqlx::query!(
        "UPDATE users SET deactivation_notice_sent_at = $2, reactivation_refused_at = $3
         WHERE id = $1",
        target_user_id,
        after.deactivation_notice_sent_at,
        after.reactivation_refused_at
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

/// One member of a group left without an owner, as
/// [`ownerless_group_members`] lists it for the superadmin to choose from.
#[derive(Serialize)]
pub struct AdminGroupMemberResponse {
    pub user_id: Uuid,
    pub display_name: String,
    pub email: String,
    pub role: String,
    pub joined_at: DateTime<Utc>,
    pub deactivated: bool,
    pub pending_deletion: bool,
}

/// The members of a group left without an owner (#323), for the superadmin
/// to designate one ([`designate_owner`]). Only such a group's: the members
/// of a group that has an owner are none of support's business (409
/// `group_has_owner`). An unknown group is a 404. Traced in `audit_log`.
pub async fn ownerless_group_members(
    State(state): State<AppState>,
    actor: SuperAdminUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = crate::db::begin(&state.admin_db).await?;
    let name = sqlx::query_scalar!("SELECT name FROM groups WHERE id = $1", group_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?;
    let members = sqlx::query!(
        r#"SELECT gm.user_id, u.display_name, u.email, gm.role::text AS "role!", gm.joined_at,
                  u.deactivated_at IS NOT NULL AS "deactivated!",
                  u.deletion_requested_at IS NOT NULL AS "pending_deletion!"
           FROM group_members gm JOIN users u ON u.id = gm.user_id
           WHERE gm.group_id = $1
           ORDER BY gm.joined_at, gm.user_id"#,
        group_id
    )
    .fetch_all(&mut *tx)
    .await?;
    if members.iter().any(|m| m.role == "owner") {
        return Err(AppError::Conflict("group_has_owner".into()));
    }
    let members: Vec<AdminGroupMemberResponse> = members
        .into_iter()
        .map(|m| AdminGroupMemberResponse {
            user_id: m.user_id,
            display_name: m.display_name,
            email: m.email,
            role: m.role,
            joined_at: m.joined_at,
            deactivated: m.deactivated,
            pending_deletion: m.pending_deletion,
        })
        .collect();
    audit::record(
        &mut tx,
        Some(actor.user_id),
        "admin.group.members.list",
        "group",
        &group_id.to_string(),
        json!({ "count": members.len() }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(
        json!({ "id": group_id, "name": name, "members": members }),
    ))
}

#[derive(Deserialize)]
pub struct DesignateOwnerRequest {
    pub user_id: Uuid,
}

/// Makes an active member the owner of a group left without one (#323,
/// controller's decision of 2026-10-04): 204. The member is told like an
/// heir of the purge (`groups::succession::stamp_inheritance`). 404 for an
/// unknown group; 409 `group_has_owner`; 422 `not_a_member`, or
/// `owner_must_be_active` for a deactivated account or one whose deletion
/// is pending ([`check_designation`]). Traced in `audit_log` as a transfer
/// of ownership, the superadmin its actor.
pub async fn designate_owner(
    State(state): State<AppState>,
    actor: SuperAdminUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<DesignateOwnerRequest>,
) -> AppResult<impl IntoResponse> {
    let mut tx = crate::db::begin(&state.admin_db).await?;
    sqlx::query_scalar!("SELECT id FROM groups WHERE id = $1", group_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?;
    let (has_owner, members) = succession::lock_members(&mut tx, group_id).await?;
    let target = members.iter().find(|m| m.user_id == body.user_id);
    check_designation(has_owner, target).map_err(|refusal| match refusal {
        DesignationRefusal::GroupHasOwner => AppError::Conflict("group_has_owner".into()),
        DesignationRefusal::NotAMember => AppError::Unprocessable("not_a_member".into()),
        DesignationRefusal::NotActive => AppError::Unprocessable("owner_must_be_active".into()),
    })?;
    succession::stamp_inheritance(
        &mut tx,
        group_id,
        body.user_id,
        OwnershipReason::DesignatedBySupport,
    )
    .await?;
    audit::record(
        &mut tx,
        Some(actor.user_id),
        "ownership_transferred",
        "group",
        &group_id.to_string(),
        json!({
            "new_owner_id": body.user_id,
            "reason": OwnershipReason::DesignatedBySupport.as_str(),
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
