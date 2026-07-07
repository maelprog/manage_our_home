use axum::extract::{Multipart, Path, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use serde::Serialize;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::storage::{sniff_and_validate_mime, MAX_ATTACHMENT_SIZE_BYTES};
use crate::AppState;

#[derive(Serialize)]
pub struct AttachmentResponse {
    pub id: Uuid,
    pub event_id: Uuid,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: i64,
}

/// Reads a single `file` field from the multipart body, sniffs its real
/// MIME type (never trusting the client-supplied content type or
/// filename extension), and rejects anything outside the allow-list or
/// over the size cap before it ever reaches MinIO.
pub async fn upload_attachment(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id)): Path<(Uuid, Uuid)>,
    mut multipart: Multipart,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    sqlx::query_scalar!("SELECT id FROM events WHERE id = $1 AND group_id = $2", event_id, group_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?;

    let mut filename = None;
    let mut bytes = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| AppError::BadRequest("invalid_multipart".into()))?
    {
        if field.name() == Some("file") {
            filename = field.file_name().map(|s| s.to_string());
            let data = field
                .bytes()
                .await
                .map_err(|_| AppError::BadRequest("invalid_multipart".into()))?;
            bytes = Some(data);
        }
    }
    let filename = filename.ok_or(AppError::BadRequest("missing_file".into()))?;
    let bytes = bytes.ok_or(AppError::BadRequest("missing_file".into()))?;

    if bytes.len() > MAX_ATTACHMENT_SIZE_BYTES {
        return Err(AppError::Unprocessable("file_too_large".into()));
    }
    let mime_type = sniff_and_validate_mime(&bytes).ok_or(AppError::Unprocessable("unsupported_file_type".into()))?;

    let storage_key = format!("{group_id}/{event_id}/{}", Uuid::new_v4());
    state
        .storage
        .put_object(&storage_key, bytes.to_vec(), mime_type)
        .await
        .map_err(|e| AppError::Internal(e))?;

    let attachment = sqlx::query!(
        r#"
        INSERT INTO event_attachments (event_id, uploaded_by, storage_key, filename, mime_type, size_bytes)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id
        "#,
        event_id,
        auth.user_id,
        storage_key,
        filename,
        mime_type,
        bytes.len() as i64,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(AttachmentResponse {
            id: attachment.id,
            event_id,
            filename,
            mime_type: mime_type.to_string(),
            size_bytes: bytes.len() as i64,
        }),
    ))
}

pub async fn list_attachments(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let rows = sqlx::query_as!(
        AttachmentResponse,
        r#"SELECT id, event_id, filename, mime_type, size_bytes FROM event_attachments WHERE event_id = $1"#,
        event_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(rows))
}

pub async fn download_attachment(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id, attachment_id)): Path<(Uuid, Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let storage_key = sqlx::query_scalar!(
        "SELECT storage_key FROM event_attachments WHERE id = $1 AND event_id = $2",
        attachment_id,
        event_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    tx.commit().await?;

    let url = state
        .storage
        .presigned_get_url(&storage_key)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(serde_json::json!({ "url": url })))
}

pub async fn delete_attachment(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id, attachment_id)): Path<(Uuid, Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let storage_key = sqlx::query_scalar!(
        "SELECT storage_key FROM event_attachments WHERE id = $1 AND event_id = $2",
        attachment_id,
        event_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    sqlx::query!("DELETE FROM event_attachments WHERE id = $1", attachment_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    state.storage.delete_object(&storage_key).await.map_err(AppError::Internal)?;

    Ok(StatusCode::NO_CONTENT)
}
