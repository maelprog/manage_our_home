use axum::extract::{Multipart, Path, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use serde::Serialize;
use uuid::Uuid;

use crate::agenda::thumbnail::{make_thumbnail, thumbnail_key, THUMBNAIL_CONTENT_TYPE};
use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::storage::{
    sniff_and_validate_mime, Storage, MAX_ATTACHMENT_SIZE_BYTES, MAX_UPLOAD_BODY_BYTES,
};
use crate::AppState;

#[derive(Serialize, utoipa::ToSchema)]
pub struct AttachmentResponse {
    pub id: Uuid,
    pub event_id: Uuid,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: i64,
    /// Whether a thumbnail was stored with the file (#774): a picture
    /// decoded within the bounds of `agenda::thumbnail`. Never for a
    /// video or a PDF; the `mime_type` tells those apart.
    pub has_thumbnail: bool,
}

/// How long an upload waits for the decode permit it shares with the
/// avatar (`avatar::DECODES`) before storing its picture without a
/// thumbnail. A photo upload holds the permit while its body arrives, at
/// the client's pace; an attachment's response does not wait on it longer.
const THUMBNAIL_PERMIT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Reads a single `file` field from the multipart body, sniffs its real
/// MIME type (never trusting the client-supplied content type or
/// filename extension), and rejects anything outside the allow-list or
/// over the size cap before it ever reaches MinIO.
///
/// Two transactions, with the body read between them and no connection
/// held while it arrives (#216). The client decides how long the body takes
/// to send; a transaction opened before reading it held a pool connection
/// for that long, and as many slow uploads as the pool has connections
/// failed every other request on `PoolTimedOut`.
///
/// Between the two, and still before the body, an upload permit (#219):
/// the body is held in memory whole, so how many uploads read at once is
/// bounded per account and per process, and one over either bound is
/// answered 503 without a byte read. How long the body may take is bounded
/// on every route by the middleware in `build_router`.
#[utoipa::path(
    post,
    path = "/groups/{id}/events/{event_id}/attachments",
    tag = "agenda",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("event_id" = uuid::Uuid, Path, description = "Event id"),
    ),
    request_body(content = inline(crate::api_docs::schemas::FileUpload), content_type = "multipart/form-data", description = "One `file` field, 20 MiB at most"),
    responses(
        (status = 201, description = "Attachment stored", body = AttachmentResponse),
        (status = 400, description = "`invalid_multipart`, `missing_file` or `multiple_files`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 404, description = "`not_found`: no such event", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 422, description = "`file_too_large` or `unsupported_file_type`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 503, description = "`uploads_busy`: too many uploads at once; `Retry-After` says when to retry", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn upload_attachment(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> AppResult<impl IntoResponse> {
    // First transaction, before the body: a non-member (403) or an unknown
    // event (404) is answered without reading a byte of the upload.
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    sqlx::query_scalar!(
        "SELECT id FROM events WHERE id = $1 AND group_id = $2",
        event_id,
        group_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    tx.commit().await?;

    // Held until this function returns or its future is dropped — the
    // client disconnecting mid-body drops it too.
    let _upload_permit = state
        .upload_gate
        .try_acquire(auth.user_id)
        .map_err(|busy| {
            tracing::info!(?busy, "upload turned away");
            AppError::UploadsBusy
        })?;

    // The file goes into one buffer sized before the first chunk (#249):
    // `Field::bytes` grows its buffer by doubling, which leaves a file at
    // the 20 MiB cap in a 32 MiB allocation.
    let mut filename = None;
    let mut bytes = None;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| AppError::BadRequest("invalid_multipart".into()))?
    {
        if field.name() == Some("file") {
            // One file per upload (#326): a second `file` field is refused
            // at its headers. Read, it took a second buffer of the declared
            // body while the first was still held, and the memory an upload
            // cost was no longer bounded by its size.
            if bytes.is_some() {
                return Err(AppError::BadRequest("multiple_files".into()));
            }
            filename = field.file_name().map(|s| s.to_string());
            let mut file = Vec::with_capacity(file_buffer_capacity(&headers));
            while let Some(chunk) = field
                .chunk()
                .await
                .map_err(|_| AppError::BadRequest("invalid_multipart".into()))?
            {
                file.extend_from_slice(&chunk);
            }
            // Takes the `Vec` over, no copy.
            bytes = Some(bytes::Bytes::from(file));
        }
    }
    // The multipart reader keeps a buffer of what it read, about 0.5 MiB
    // for a file at the cap: freed here rather than once the object is
    // written (#249).
    drop(multipart);
    let filename = filename.ok_or(AppError::BadRequest("missing_file".into()))?;
    let bytes = bytes.ok_or(AppError::BadRequest("missing_file".into()))?;

    if bytes.len() > MAX_ATTACHMENT_SIZE_BYTES {
        return Err(AppError::Unprocessable("file_too_large".into()));
    }
    let mime_type = sniff_and_validate_mime(&bytes)
        .ok_or(AppError::Unprocessable("unsupported_file_type".into()))?;

    let storage_key = format!("{group_id}/{event_id}/{}", Uuid::new_v4());

    // The thumbnail, before the second transaction: a decode holds no
    // connection. Best effort — none for a video, a PDF, an unreadable
    // picture or one past the bounds (`agenda::thumbnail`), and none when
    // the decode permit stays busy; the file is stored all the same.
    let (bytes, thumbnail) = generate_thumbnail(bytes, mime_type).await;
    let thumbnail_key = thumbnail.as_ref().map(|_| thumbnail_key(&storage_key));

    // Second transaction, once the body is in hand. The event may have been
    // deleted since the first one checked it; left to the INSERT, that
    // surfaces as a failed foreign key (or, under the runtime role, a row
    // refused by the `event_attachments` policy) and a 500. Checking again
    // answers 404 instead, and `FOR KEY SHARE` keeps the answer true until
    // commit: a concurrent event delete waits for this transaction rather
    // than removing the row the INSERT is about to reference.
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    sqlx::query_scalar!(
        "SELECT id FROM events WHERE id = $1 AND group_id = $2 FOR KEY SHARE",
        event_id,
        group_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    // Row first, object second, commit last — the mirror of the delete
    // ordering settled in #54/#56 and #57/#59 (objects first there, so a
    // storage failure drops the transaction and the rows survive for a
    // retry). Here the same principle points the other way: the row is
    // written inside the transaction *before* the object exists, so a
    // `put_object` failure below drops the transaction and neither the row
    // nor the object survives.
    //
    // The point is what this removes. The previous order wrote the object
    // first and so needed a compensating delete when the INSERT failed —
    // best-effort, and swallowing its own failure with `let _`, which made
    // a leaked object invisible even in the logs (#62). Rollback needs no
    // compensation and cannot half-fail, so that path is gone rather than
    // improved.
    //
    // Cost, accepted deliberately: `put_object` runs with the transaction
    // open, holding a connection for the duration of the call to MinIO —
    // server-side I/O only, the client's body having been read above.
    // Bounded by the storage client's operation timeout
    // (`storage::S3_OPERATION_TIMEOUT`), not by MAX_ATTACHMENT_SIZE_BYTES:
    // a size cap says nothing about how long a stalled storage takes to
    // answer. Same tradeoff as the batched delete in #59.
    let attachment = sqlx::query!(
        r#"
        INSERT INTO event_attachments (event_id, uploaded_by, storage_key, filename, mime_type, size_bytes, thumbnail_key)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id
        "#,
        event_id,
        auth.user_id,
        storage_key,
        filename,
        mime_type,
        bytes.len() as i64,
        thumbnail_key,
    )
    .fetch_one(&mut *tx)
    .await?;

    // The buffer itself goes to the storage, not a copy of it (#249).
    let size_bytes = bytes.len() as i64;
    state
        .storage
        .put_object(&storage_key, bytes, mime_type)
        .await
        .map_err(AppError::Internal)?;
    // Same transaction, same rule: a failure here drops the row, and the
    // object above is left to the reconciliation pass.
    if let (Some(key), Some(thumbnail)) = (&thumbnail_key, thumbnail) {
        state
            .storage
            .put_object(key, bytes::Bytes::from(thumbnail), THUMBNAIL_CONTENT_TYPE)
            .await
            .map_err(AppError::Internal)?;
    }

    // Still uncompensated, and not fixable at this layer: if this commit
    // fails, the object is written and the row never lands. Same for a
    // client disconnect or a process death anywhere above — the future is
    // dropped, the transaction rolls back, and the object stays. Closing
    // that window needs a transactional outbox (#62); until then the
    // reconciliation pass (#58) is what catches it.
    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(AttachmentResponse {
            id: attachment.id,
            event_id,
            filename,
            mime_type: mime_type.to_string(),
            size_bytes,
            has_thumbnail: thumbnail_key.is_some(),
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/groups/{id}/events/{event_id}/attachments",
    tag = "agenda",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("event_id" = uuid::Uuid, Path, description = "Event id"),
    ),
    responses(
        (status = 200, description = "The event's attachments", body = Vec<AttachmentResponse>),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_attachments(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let rows = sqlx::query_as!(
        AttachmentResponse,
        r#"SELECT id, event_id, filename, mime_type, size_bytes,
                  thumbnail_key IS NOT NULL AS "has_thumbnail!"
           FROM event_attachments WHERE event_id = $1"#,
        event_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(rows))
}

#[utoipa::path(
    get,
    path = "/groups/{id}/events/{event_id}/attachments/{attachment_id}/download",
    tag = "agenda",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("event_id" = uuid::Uuid, Path, description = "Event id"),
        ("attachment_id" = uuid::Uuid, Path, description = "Attachment id"),
    ),
    responses(
        (status = 200, description = "A short-lived presigned URL to the file", body = crate::api_docs::schemas::AttachmentDownload),
        (status = 404, description = "`not_found`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
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

#[utoipa::path(
    delete,
    path = "/groups/{id}/events/{event_id}/attachments/{attachment_id}",
    tag = "agenda",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("event_id" = uuid::Uuid, Path, description = "Event id"),
        ("attachment_id" = uuid::Uuid, Path, description = "Attachment id"),
    ),
    responses(
        (status = 204, description = "Attachment deleted"),
        (status = 404, description = "`not_found`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn delete_attachment(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id, attachment_id)): Path<(Uuid, Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let attachment = sqlx::query!(
        "SELECT storage_key, thumbnail_key FROM event_attachments WHERE id = $1 AND event_id = $2",
        attachment_id,
        event_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    // The thumbnail goes with its file (#774).
    let keys: Vec<String> = std::iter::once(attachment.storage_key)
        .chain(attachment.thumbnail_key)
        .collect();
    delete_objects(&state.storage, &keys).await?;

    sqlx::query!("DELETE FROM event_attachments WHERE id = $1", attachment_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Storage keys of every attachment hanging off the given events, and of
/// their thumbnails (#774).
///
/// `event_attachments` cascades from `events`, so any delete that reaches
/// an event takes its attachment rows with it — and with them the only
/// record of which objects the event owned. Callers collect the keys
/// through here *before* the rows go away.
pub(crate) async fn storage_keys_for_events(
    tx: &mut sqlx::PgConnection,
    event_ids: &[Uuid],
) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT k AS "key!" FROM event_attachments,
                  LATERAL (VALUES (storage_key), (thumbnail_key)) AS keys(k)
           WHERE event_id = ANY($1) AND k IS NOT NULL"#,
        event_ids,
    )
    .fetch_all(tx)
    .await?)
}

/// Storage keys of every attachment hanging off every event in a group,
/// thumbnails included (#774, migration 0054).
///
/// Group deletion never has the event ids in hand, and `events` cascades
/// from `groups` just as `event_attachments` cascades from `events` — so
/// this reads through both in one round trip rather than listing the
/// events first only to throw the ids away.
///
/// Read through `group_attachment_storage_keys()` (migration 0044), not
/// from the tables: under their policies, the owner deleting the group does
/// not see the private events they take no part in (#620), whose rows the
/// cascade deletes all the same. The function answers only for the group
/// `scoped_tx` was opened on, and only to its owner.
pub(crate) async fn storage_keys_for_group(
    tx: &mut sqlx::PgConnection,
    group_id: Uuid,
) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT storage_key AS "storage_key!" FROM group_attachment_storage_keys($1) AS storage_key"#,
        group_id,
    )
    .fetch_all(tx)
    .await?)
}

/// Removes objects from MinIO before the rows referencing them are
/// committed away. Ordering is deliberate: on failure the caller's
/// transaction is dropped (rolled back) rather than committed, so the
/// rows survive and a retry can still find the objects — the alternative
/// is a "deleted" event whose bytes stay in the bucket with nothing left
/// pointing at them. Failures surface as 500 for the same reason: swallowed
/// here, the leak would be silent and unfindable.
///
/// Batched (`Storage::delete_objects`) rather than one call per key: this
/// runs with the caller's transaction open, and a group delete has no
/// bound on how many attachments it carries.
pub(crate) async fn delete_objects(storage: &Storage, keys: &[String]) -> AppResult<()> {
    storage
        .delete_objects(keys)
        .await
        .map_err(AppError::Internal)
}

/// The thumbnail of an upload (`agenda::thumbnail`), off the async workers
/// and under the decode permit. Hands the buffer back: it is the one
/// stored, not a copy (#249).
async fn generate_thumbnail(
    bytes: bytes::Bytes,
    mime_type: &'static str,
) -> (bytes::Bytes, Option<Vec<u8>>) {
    if !mime_type.starts_with("image/") {
        return (bytes, None);
    }
    let Ok(Ok(permit)) =
        tokio::time::timeout(THUMBNAIL_PERMIT_WAIT, crate::avatar::DECODES.acquire()).await
    else {
        tracing::info!("attachment stored without a thumbnail: decode permit busy");
        return (bytes, None);
    };
    // A second handle on the same buffer, not a copy of it.
    let source = bytes.clone();
    let thumbnail = tokio::task::spawn_blocking(move || make_thumbnail(&source, mime_type))
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "thumbnail generation failed");
            None
        });
    drop(permit);
    (bytes, thumbnail)
}

/// How much room to make for the file before reading it (#249).
///
/// The declared body, which the file is part of, and never more than the
/// route's body limit: a client declaring a gigabyte and sending ten bytes
/// gets a buffer of the limit, not of its claim. Without a usable
/// `Content-Length`, the limit itself — one buffer of it is what the
/// upload gate budgets for each upload anyway.
fn file_buffer_capacity(headers: &HeaderMap) -> usize {
    headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(MAX_UPLOAD_BODY_BYTES, |declared| {
            usize::try_from(declared).map_or(MAX_UPLOAD_BODY_BYTES, |declared| {
                declared.min(MAX_UPLOAD_BODY_BYTES)
            })
        })
}

#[cfg(test)]
mod tests {
    use axum::http::{header, HeaderMap};

    use crate::storage::MAX_UPLOAD_BODY_BYTES;

    fn with_content_length(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_LENGTH, value.parse().unwrap());
        headers
    }

    #[test]
    fn the_file_buffer_is_sized_from_the_declared_body() {
        assert_eq!(
            super::file_buffer_capacity(&with_content_length("4096")),
            4096
        );
    }

    #[test]
    fn a_declared_body_past_the_limit_gets_no_more_than_the_limit() {
        for declared in [MAX_UPLOAD_BODY_BYTES + 1, 10 * MAX_UPLOAD_BODY_BYTES] {
            assert_eq!(
                super::file_buffer_capacity(&with_content_length(&declared.to_string())),
                MAX_UPLOAD_BODY_BYTES
            );
        }
        assert_eq!(
            super::file_buffer_capacity(&with_content_length("18446744073709551616")),
            MAX_UPLOAD_BODY_BYTES
        );
    }

    /// No length, or one that is not a number: the body limit is the only
    /// bound known, and one buffer of it is what the upload gate counts for
    /// each upload anyway.
    #[test]
    fn an_undeclared_or_unreadable_length_gets_the_limit() {
        assert_eq!(
            super::file_buffer_capacity(&HeaderMap::new()),
            MAX_UPLOAD_BODY_BYTES
        );
        assert_eq!(
            super::file_buffer_capacity(&with_content_length("beaucoup")),
            MAX_UPLOAD_BODY_BYTES
        );
    }
}
