use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::audit;
use crate::auth::session::{scoped_tx, token_scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::AppState;

const MAX_GROUPS_PER_USER: i64 = 10;
const INVITATION_TTL_DAYS: i64 = 7;

#[derive(Deserialize)]
pub struct CreateGroupRequest {
    pub name: String,
}

#[derive(Serialize)]
pub struct GroupResponse {
    pub id: Uuid,
    pub name: String,
}

/// AC #9: the creator becomes owner+admin immediately; blocked at 10
/// active groups per user.
pub async fn create_group(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<CreateGroupRequest>,
) -> AppResult<impl IntoResponse> {
    let count = sqlx::query_scalar!(
        "SELECT count(*) FROM group_members WHERE user_id = $1",
        auth.user_id
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(0);

    if count >= MAX_GROUPS_PER_USER {
        return Err(AppError::Unprocessable("too_many_groups".into()));
    }

    let mut tx = state.db.begin().await?;
    let group = sqlx::query!(
        r#"
        INSERT INTO groups (name, created_by)
        VALUES ($1, $2)
        RETURNING id, name
        "#,
        body.name,
        auth.user_id,
    )
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query!(
        r#"
        INSERT INTO group_members (group_id, user_id, role)
        VALUES ($1, $2, 'owner')
        "#,
        group.id,
        auth.user_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(GroupResponse {
            id: group.id,
            name: group.name,
        }),
    ))
}

pub async fn get_group(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let group = sqlx::query!("SELECT id, name FROM groups WHERE id = $1", group_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?;

    let members = sqlx::query!(
        r#"SELECT user_id, role as "role: String" FROM group_members WHERE group_id = $1"#,
        group_id
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(json!({
        "id": group.id,
        "name": group.name,
        "members": members.iter().map(|m| json!({"user_id": m.user_id, "role": m.role})).collect::<Vec<_>>(),
    })))
}

/// AC #13: pure role-permission check, shared by `change_role` and
/// `remove_member`. The owner can act on anyone but itself; an admin can
/// only act on standard members; a standard member can act on no one.
pub fn actor_can_act_on(actor_role: &str, target_role: &str) -> bool {
    if target_role == "owner" {
        return false;
    }
    match actor_role {
        "owner" => true,
        "admin" => target_role != "admin",
        _ => false,
    }
}

pub(crate) async fn require_role(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    user_id: Uuid,
) -> AppResult<String> {
    let row = sqlx::query!(
        r#"SELECT role as "role: String" FROM group_members WHERE group_id = $1 AND user_id = $2"#,
        group_id,
        user_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(AppError::Forbidden)?;
    Ok(row.role)
}

/// Owner-only. Deleting the group also deletes members/invitations
/// (ON DELETE CASCADE) — this is the only sanctioned way to empty a group.
pub async fn delete_group(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let role = require_role(&mut tx, group_id, auth.user_id).await?;
    if role != "owner" {
        return Err(AppError::Forbidden);
    }

    sqlx::query!("DELETE FROM groups WHERE id = $1", group_id)
        .execute(&mut *tx)
        .await?;

    audit::record(
        &mut tx,
        Some(auth.user_id),
        "group_deleted",
        "group",
        &group_id.to_string(),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct CreateInvitationRequest {
    pub invited_email: Option<String>,
}

/// AC #14: admin/owner only; invitation expires after 7 days and is
/// single-use.
pub async fn create_invitation(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<CreateInvitationRequest>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let role = require_role(&mut tx, group_id, auth.user_id).await?;
    if role != "owner" && role != "admin" {
        return Err(AppError::Forbidden);
    }

    let expires_at = Utc::now() + Duration::days(INVITATION_TTL_DAYS);
    let invitation = sqlx::query!(
        r#"
        INSERT INTO invitations (group_id, invited_email, created_by, expires_at)
        VALUES ($1, $2, $3, $4)
        RETURNING id, token
        "#,
        group_id,
        body.invited_email,
        auth.user_id,
        expires_at,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    if let Some(email) = &body.invited_email {
        let group_name = sqlx::query_scalar!("SELECT name FROM groups WHERE id = $1", group_id)
            .fetch_one(&state.db)
            .await?;
        let link = format!("{}/groups/invitations/{}/accept", state.frontend_base_url, invitation.token);
        if let Err(e) = state
            .email
            .send(email, "Invitation à rejoindre un groupe", crate::email::EmailSender::invitation_body(&link, &group_name))
            .await
        {
            tracing::error!(error = ?e, "failed to send invitation email");
        }
    }

    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": invitation.id, "token": invitation.token })),
    ))
}

/// AC #14: single-use, 7-day expiry. Re-use after `consumed_at` is set
/// returns 410 Gone.
pub async fn accept_invitation(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(token): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut lookup_tx = token_scoped_tx(&state.db, token).await?;
    let group_id = sqlx::query_scalar!(
        "SELECT group_id FROM invitations WHERE token = $1",
        token
    )
    .fetch_optional(&mut *lookup_tx)
    .await?
    .ok_or(AppError::NotFound)?;
    lookup_tx.commit().await?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let invitation = sqlx::query!(
        r#"
        SELECT id, expires_at, consumed_at
        FROM invitations
        WHERE token = $1
        FOR UPDATE
        "#,
        token
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    if invitation.consumed_at.is_some() {
        return Err(AppError::Gone);
    }
    if invitation.expires_at < Utc::now() {
        return Err(AppError::Gone);
    }

    sqlx::query!(
        "UPDATE invitations SET consumed_at = now(), consumed_by = $1 WHERE token = $2",
        auth.user_id,
        token
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        r#"
        INSERT INTO group_members (group_id, user_id, role)
        VALUES ($1, $2, 'standard')
        ON CONFLICT (group_id, user_id) DO NOTHING
        "#,
        group_id,
        auth.user_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(Json(json!({ "group_id": group_id })))
}

#[derive(Deserialize)]
pub struct ChangeRoleRequest {
    pub role: String,
}

/// AC #13: only the owner may touch another admin or the owner role
/// itself; an admin may only act on standard members. Enforced here, not
/// just documented, and covered by a test per role pair.
pub async fn change_role(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, target_user_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ChangeRoleRequest>,
) -> AppResult<impl IntoResponse> {
    if body.role != "admin" && body.role != "standard" {
        return Err(AppError::BadRequest("invalid_role".into()));
    }

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;
    let target_role = require_role(&mut tx, group_id, target_user_id).await?;

    if !actor_can_act_on(&actor_role, &target_role) {
        return Err(AppError::Forbidden);
    }

    sqlx::query!(
        r#"
        UPDATE group_members SET role = ($1::text)::group_role
        WHERE group_id = $2 AND user_id = $3
        "#,
        body.role,
        group_id,
        target_user_id,
    )
    .execute(&mut *tx)
    .await?;

    audit::record(
        &mut tx,
        Some(auth.user_id),
        "role_change",
        "group_member",
        &target_user_id.to_string(),
        json!({ "group_id": group_id, "new_role": body.role }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::OK)
}

/// AC #13: admin/owner removes a standard; owner removes an admin. Nobody
/// removes the owner via this endpoint (must transfer ownership + leave,
/// or delete the group).
pub async fn remove_member(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, target_user_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;
    let target_role = require_role(&mut tx, group_id, target_user_id).await?;

    if !actor_can_act_on(&actor_role, &target_role) {
        return Err(AppError::Forbidden);
    }

    sqlx::query!(
        "DELETE FROM group_members WHERE group_id = $1 AND user_id = $2",
        group_id,
        target_user_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
pub struct LeaveGroupRequest {
    pub new_owner_id: Option<Uuid>,
}

/// AC #11, #12: an owner leaving must supply `new_owner_id` (422
/// otherwise). AC #12: the last remaining member must delete the group
/// instead (409).
pub async fn leave_group(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<LeaveGroupRequest>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let role = require_role(&mut tx, group_id, auth.user_id).await?;

    let member_count = sqlx::query_scalar!(
        "SELECT count(*) FROM group_members WHERE group_id = $1",
        group_id
    )
    .fetch_one(&mut *tx)
    .await?
    .unwrap_or(0);

    if member_count <= 1 {
        return Err(AppError::Conflict("last_member_must_delete_group".into()));
    }

    if role == "owner" {
        let new_owner_id = body.new_owner_id.ok_or_else(|| {
            AppError::Unprocessable("new_owner_id_required".into())
        })?;
        if new_owner_id == auth.user_id {
            return Err(AppError::Unprocessable("new_owner_id_required".into()));
        }
        require_role(&mut tx, group_id, new_owner_id).await?;

        sqlx::query!(
            r#"UPDATE group_members SET role = 'admin' WHERE group_id = $1 AND user_id = $2"#,
            group_id,
            auth.user_id
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            r#"UPDATE group_members SET role = 'owner' WHERE group_id = $1 AND user_id = $2"#,
            group_id,
            new_owner_id
        )
        .execute(&mut *tx)
        .await?;

        audit::record(
            &mut tx,
            Some(auth.user_id),
            "ownership_transferred",
            "group",
            &group_id.to_string(),
            json!({ "new_owner_id": new_owner_id }),
        )
        .await?;
    }

    sqlx::query!(
        "DELETE FROM group_members WHERE group_id = $1 AND user_id = $2",
        group_id,
        auth.user_id
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(StatusCode::OK)
}

#[cfg(test)]
mod tests {
    use super::actor_can_act_on;

    #[test]
    fn owner_can_act_on_admin_and_standard() {
        assert!(actor_can_act_on("owner", "admin"));
        assert!(actor_can_act_on("owner", "standard"));
    }

    #[test]
    fn owner_cannot_act_on_owner() {
        assert!(!actor_can_act_on("owner", "owner"));
    }

    #[test]
    fn admin_can_act_on_standard_only() {
        assert!(actor_can_act_on("admin", "standard"));
        assert!(!actor_can_act_on("admin", "admin"));
        assert!(!actor_can_act_on("admin", "owner"));
    }

    #[test]
    fn standard_cannot_act_on_anyone() {
        assert!(!actor_can_act_on("standard", "standard"));
        assert!(!actor_can_act_on("standard", "admin"));
        assert!(!actor_can_act_on("standard", "owner"));
    }
}
