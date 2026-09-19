use axum::extract::{Multipart, Path, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use serde::Serialize;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::storage::{
    sniff_and_validate_mime, Storage, MAX_ATTACHMENT_SIZE_BYTES, MAX_UPLOAD_BODY_BYTES,
};
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

    // The buffer itself goes to the storage, not a copy of it (#249).
    let size_bytes = bytes.len() as i64;
    state
        .storage
        .put_object(&storage_key, bytes, mime_type)
        .await
        .map_err(AppError::Internal)?;

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

    delete_objects(&state.storage, std::slice::from_ref(&storage_key)).await?;

    sqlx::query!("DELETE FROM event_attachments WHERE id = $1", attachment_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Storage keys of every attachment hanging off the given events.
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
        "SELECT storage_key FROM event_attachments WHERE event_id = ANY($1)",
        event_ids,
    )
    .fetch_all(tx)
    .await?)
}

/// Storage keys of every attachment hanging off every event in a group.
///
/// Group deletion never has the event ids in hand, and `events` cascades
/// from `groups` just as `event_attachments` cascades from `events` — so
/// this reads through both in one round trip rather than listing the
/// events first only to throw the ids away.
pub(crate) async fn storage_keys_for_group(
    tx: &mut sqlx::PgConnection,
    group_id: Uuid,
) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar!(
        "SELECT storage_key FROM event_attachments
         WHERE event_id IN (SELECT id FROM events WHERE group_id = $1)",
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
