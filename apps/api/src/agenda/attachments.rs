use axum::body::Body;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::{http::StatusCode, Json};
use serde::{Deserialize, Serialize};
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

/// `?download=1` on `attachment_content`: the file as a download rather
/// than shown in the page.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ContentQuery {
    /// `1` (or `true`): `Content-Disposition: attachment`.
    pub download: Option<String>,
}

/// The headers every answer of `attachment_content` and
/// `attachment_thumbnail` carries: the type is the stored one and the
/// browser must not guess another (an uploaded file read as HTML would run
/// in this origin); a shared cache must not keep a file only the family
/// may see.
fn private_file_headers(builder: axum::http::response::Builder) -> axum::http::response::Builder {
    builder
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CACHE_CONTROL, "private")
}

/// An attachment's bytes, served by the API itself (#775): the pages' CSP
/// (`default-src 'self'`, `infra/Caddyfile`) admits no `<img>`, `<video>`
/// or `<iframe>` from the storage's host, which the browser cannot reach
/// anyway. Same access as the listing: a member of the group, through its
/// policies (`scoped_tx`).
///
/// Streamed from the storage, never held: a `Range` (`parse_range`) asks
/// the storage for those bytes only, so a video player seeking in a
/// 20 MiB file costs what it reads. The type is the one sniffed at upload
/// (`mime_type`), never the client's; a PDF shown in the page is
/// sandboxed (`Content-Security-Policy: sandbox`): no script of it runs
/// with this origin's rights.
#[utoipa::path(
    get,
    path = "/groups/{id}/events/{event_id}/attachments/{attachment_id}/content",
    tag = "agenda",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("event_id" = uuid::Uuid, Path, description = "Event id"),
        ("attachment_id" = uuid::Uuid, Path, description = "Attachment id"),
        ("Range" = Option<String>, Header, description = "One byte range, `bytes=start-end`, `bytes=start-` or `bytes=-length`"),
        ContentQuery,
    ),
    responses(
        (status = 200, description = "The file, of its stored type", body = Vec<u8>, content_type = "application/octet-stream"),
        (status = 206, description = "The range asked, with `Content-Range`", body = Vec<u8>, content_type = "application/octet-stream"),
        (status = 404, description = "`not_found`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 416, description = "A range past the file, or several ranges; `Content-Range: bytes */<size>`"),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn attachment_content(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id, attachment_id)): Path<(Uuid, Uuid, Uuid)>,
    Query(query): Query<ContentQuery>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    // The group named, besides the policies: an id from another family
    // finds nothing whatever role the connection has.
    let file = sqlx::query!(
        r#"SELECT a.storage_key, a.filename, a.mime_type, a.size_bytes
           FROM event_attachments a JOIN events e ON e.id = a.event_id
           WHERE a.id = $1 AND a.event_id = $2 AND e.group_id = $3"#,
        attachment_id,
        event_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    tx.commit().await?;

    let size = u64::try_from(file.size_bytes).unwrap_or(0);
    let range = parse_range(
        headers.get(header::RANGE).and_then(|v| v.to_str().ok()),
        size,
    );
    let builder = private_file_headers(Response::builder()).header(header::ACCEPT_RANGES, "bytes");
    let span = match range {
        ByteRange::Unsatisfiable => {
            return builder
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                .body(Body::empty())
                .map_err(|e| AppError::Internal(e.into()));
        }
        ByteRange::Full => None,
        ByteRange::Partial { start, end } => Some((start, end)),
    };

    let stream = state
        .storage
        .get_object_stream(&file.storage_key, span)
        .await
        .map_err(AppError::Internal)?;

    let download = matches!(query.download.as_deref(), Some("1" | "true"));
    let mut builder = builder
        .header(header::CONTENT_TYPE, &file.mime_type)
        .header(
            header::CONTENT_DISPOSITION,
            content_disposition(&file.filename, download),
        );
    builder = match span {
        Some((start, end)) => builder
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"))
            .header(header::CONTENT_LENGTH, end - start + 1),
        None => builder
            .status(StatusCode::OK)
            .header(header::CONTENT_LENGTH, size),
    };
    if !download && file.mime_type == "application/pdf" {
        builder = builder.header(header::CONTENT_SECURITY_POLICY, "sandbox");
    }
    let body = Body::from_stream(futures::stream::unfold(stream, |mut stream| async move {
        stream.next().await.map(|chunk| (chunk, stream))
    }));
    builder.body(body).map_err(|e| AppError::Internal(e.into()))
}

/// An attachment's thumbnail (#774), a JPEG of a few tens of kilobytes:
/// read whole, like the profile photo. 404 when the attachment has none —
/// a video, a PDF, a picture stored before thumbnails or past their bounds.
#[utoipa::path(
    get,
    path = "/groups/{id}/events/{event_id}/attachments/{attachment_id}/thumbnail",
    tag = "agenda",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("event_id" = uuid::Uuid, Path, description = "Event id"),
        ("attachment_id" = uuid::Uuid, Path, description = "Attachment id"),
    ),
    responses(
        (status = 200, description = "The thumbnail", body = Vec<u8>, content_type = "image/jpeg"),
        (status = 404, description = "`not_found`: no such attachment, or none stored for it", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn attachment_thumbnail(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, event_id, attachment_id)): Path<(Uuid, Uuid, Uuid)>,
) -> AppResult<Response> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let key = sqlx::query_scalar!(
        r#"SELECT a.thumbnail_key
           FROM event_attachments a JOIN events e ON e.id = a.event_id
           WHERE a.id = $1 AND a.event_id = $2 AND e.group_id = $3"#,
        attachment_id,
        event_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .flatten()
    .ok_or(AppError::NotFound)?;
    tx.commit().await?;

    let bytes = state
        .storage
        .get_object(&key)
        .await
        .map_err(AppError::Internal)?;
    private_file_headers(Response::builder())
        .header(header::CONTENT_TYPE, THUMBNAIL_CONTENT_TYPE)
        .body(Body::from(bytes))
        .map_err(|e| AppError::Internal(e.into()))
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

/// What a `Range` request header asks of a file of `size` bytes
/// (`parse_range`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ByteRange {
    /// No range, or one this server ignores: the whole file, 200.
    Full,
    /// The bytes `start..=end`, both within the file: 206.
    Partial { start: u64, end: u64 },
    /// A range that cannot be served: 416.
    Unsatisfiable,
}

/// The one byte range a `Range` header asks of a file of `size` bytes, as
/// a video player sends it to seek (`bytes=N-`) or to read ahead.
///
/// RFC 9110 §14.2: a header that does not parse, or in a unit other than
/// `bytes` (compared case-insensitively), is ignored — the whole file is
/// served. Several ranges are
/// refused (416) rather than answered as `multipart/byteranges`, which no
/// player needs and which would serve the same bytes several times over.
pub(crate) fn parse_range(header: Option<&str>, size: u64) -> ByteRange {
    // The unit is case-insensitive (RFC 9110 §14.1).
    let Some(set) = header.map(str::trim).and_then(|h| {
        let (unit, set) = h.split_at_checked(6)?;
        unit.eq_ignore_ascii_case("bytes=").then_some(set)
    }) else {
        return ByteRange::Full;
    };
    // A list may carry empty elements, which do not count (RFC 9110 §5.6.1).
    let mut specs = set.split(',').map(str::trim).filter(|s| !s.is_empty());
    let spec = match (specs.next(), specs.next()) {
        (None, _) => return ByteRange::Full,
        (Some(_), Some(_)) => return ByteRange::Unsatisfiable,
        (Some(spec), None) => spec,
    };
    let Some((first, last)) = spec.split_once('-') else {
        return ByteRange::Full;
    };
    // Digits only: `u64::from_str` would take a leading `+`.
    let number = |s: &str| {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u64>().ok())
            .flatten()
    };
    // A last byte or a suffix length too large for a `u64` lies past the
    // end of any file: clipped to it like any other.
    let length = |s: &str| {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u64>().unwrap_or(u64::MAX))
    };
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() {
        // `bytes=-N`: the last N bytes.
        return match length(last) {
            None => ByteRange::Full,
            Some(0) => ByteRange::Unsatisfiable,
            Some(_) if size == 0 => ByteRange::Unsatisfiable,
            Some(n) => ByteRange::Partial {
                start: size - n.min(size),
                end: size - 1,
            },
        };
    }
    let Some(start) = number(first) else {
        return ByteRange::Full;
    };
    let end = if last.is_empty() {
        None
    } else {
        match length(last) {
            Some(end) if end >= start => Some(end),
            _ => return ByteRange::Full,
        }
    };
    if start >= size {
        return ByteRange::Unsatisfiable;
    }
    ByteRange::Partial {
        start,
        end: end.map_or(size - 1, |end| end.min(size - 1)),
    }
}

/// The `Content-Disposition` of a stored file: `attachment` to download
/// it, `inline` to show it, under the name it was uploaded with.
///
/// The name is the uploader's, so it is never written into the header as
/// is: control characters (a line break would start another header) are
/// dropped; `filename` gets an ASCII stand-in in which a quote, a
/// backslash or anything outside printable ASCII is `_`; `filename*`
/// (RFC 6266, RFC 8187) carries the real name, percent-encoded in UTF-8.
/// The result is printable ASCII throughout: always a valid header value.
pub(crate) fn content_disposition(filename: &str, download: bool) -> String {
    let name: String = filename.chars().filter(|c| !c.is_control()).collect();
    let fallback: String = name
        .chars()
        .map(|c| match c {
            '"' | '\\' => '_',
            ' '..='~' => c,
            _ => '_',
        })
        .collect();
    let mut encoded = String::with_capacity(name.len());
    for byte in name.bytes() {
        // RFC 8187 `attr-char`.
        if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    let kind = if download { "attachment" } else { "inline" };
    format!(r#"{kind}; filename="{fallback}"; filename*=UTF-8''{encoded}"#)
}

#[cfg(test)]
mod tests {
    use axum::http::{header, HeaderMap};

    use super::{content_disposition, parse_range, ByteRange};
    use crate::storage::MAX_UPLOAD_BODY_BYTES;

    // -- Range (#775) -------------------------------------------------------

    #[test]
    fn no_range_is_the_whole_file() {
        assert_eq!(parse_range(None, 100), ByteRange::Full);
    }

    #[test]
    fn a_closed_range_is_served_as_asked() {
        assert_eq!(
            parse_range(Some("bytes=0-9"), 100),
            ByteRange::Partial { start: 0, end: 9 }
        );
        assert_eq!(
            parse_range(Some("bytes=10-10"), 100),
            ByteRange::Partial { start: 10, end: 10 }
        );
    }

    /// The end is clipped to the file: a player asking past it gets what
    /// there is.
    #[test]
    fn a_range_ending_past_the_file_stops_at_its_last_byte() {
        assert_eq!(
            parse_range(Some("bytes=90-500"), 100),
            ByteRange::Partial { start: 90, end: 99 }
        );
    }

    /// `bytes=N-`: what a video player sends to seek.
    #[test]
    fn an_open_range_runs_to_the_end() {
        assert_eq!(
            parse_range(Some("bytes=0-"), 100),
            ByteRange::Partial { start: 0, end: 99 }
        );
        assert_eq!(
            parse_range(Some("bytes=42-"), 100),
            ByteRange::Partial { start: 42, end: 99 }
        );
    }

    #[test]
    fn a_suffix_range_is_the_last_bytes() {
        assert_eq!(
            parse_range(Some("bytes=-10"), 100),
            ByteRange::Partial { start: 90, end: 99 }
        );
        assert_eq!(
            parse_range(Some("bytes=-500"), 100),
            ByteRange::Partial { start: 0, end: 99 }
        );
    }

    #[test]
    fn a_range_starting_past_the_file_is_unsatisfiable() {
        assert_eq!(
            parse_range(Some("bytes=100-"), 100),
            ByteRange::Unsatisfiable
        );
        assert_eq!(
            parse_range(Some("bytes=100-200"), 100),
            ByteRange::Unsatisfiable
        );
        assert_eq!(parse_range(Some("bytes=-0"), 100), ByteRange::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=0-"), 0), ByteRange::Unsatisfiable);
    }

    /// Several ranges would be a `multipart/byteranges` body: refused
    /// rather than served whole or in part.
    #[test]
    fn several_ranges_are_refused() {
        assert_eq!(
            parse_range(Some("bytes=0-9,20-29"), 100),
            ByteRange::Unsatisfiable
        );
        assert_eq!(
            parse_range(Some("bytes=0-9, 20-"), 100),
            ByteRange::Unsatisfiable
        );
    }

    /// RFC 9110 §14.2: a range that does not parse, or in another unit, is
    /// ignored and the whole file served.
    #[test]
    fn a_malformed_range_or_another_unit_is_ignored() {
        for header in [
            "bytes=9-0",
            "bytes=abc",
            "bytes=-",
            "bytes=",
            "bytes=1-2-3",
            "bytes=+1-2",
            "bytes=18446744073709551616-",
            "items=0-9",
            "0-9",
        ] {
            assert_eq!(parse_range(Some(header), 100), ByteRange::Full, "{header}");
        }
    }

    /// RFC 9110 §14.1: range units are compared case-insensitively.
    #[test]
    fn the_unit_is_case_insensitive() {
        for header in ["Bytes=0-9", "BYTES=0-9", "bYtEs=0-9"] {
            assert_eq!(
                parse_range(Some(header), 100),
                ByteRange::Partial { start: 0, end: 9 },
                "{header}"
            );
        }
    }

    /// RFC 9110 §5.6.1: a list may carry empty elements, which do not
    /// count — `bytes=0-9,` is one range, not two.
    #[test]
    fn an_empty_list_element_is_not_another_range() {
        for header in ["bytes=0-9,", "bytes=,0-9", "bytes= , 0-9 , "] {
            assert_eq!(
                parse_range(Some(header), 100),
                ByteRange::Partial { start: 0, end: 9 },
                "{header}"
            );
        }
        assert_eq!(parse_range(Some("bytes=,"), 100), ByteRange::Full);
        assert_eq!(
            parse_range(Some("bytes=0-9,,20-29"), 100),
            ByteRange::Unsatisfiable
        );
    }

    /// A last byte, or a suffix length, too large for a `u64` still lies
    /// past the end of the file: clipped to it like any other.
    #[test]
    fn a_number_past_u64_is_clipped_to_the_file() {
        assert_eq!(
            parse_range(Some("bytes=0-18446744073709551616"), 100),
            ByteRange::Partial { start: 0, end: 99 }
        );
        assert_eq!(
            parse_range(Some("bytes=-18446744073709551616"), 100),
            ByteRange::Partial { start: 0, end: 99 }
        );
    }

    /// An empty file has no last bytes to give: without its own case the
    /// suffix arithmetic would ask for byte `0 - 1`.
    #[test]
    fn a_suffix_range_of_an_empty_file_is_unsatisfiable() {
        assert_eq!(parse_range(Some("bytes=-5"), 0), ByteRange::Unsatisfiable);
    }

    // -- Content-Disposition (#775) -----------------------------------------

    #[test]
    fn a_plain_name_is_given_as_is() {
        assert_eq!(
            content_disposition("scan.pdf", true),
            r#"attachment; filename="scan.pdf"; filename*=UTF-8''scan.pdf"#
        );
        assert_eq!(
            content_disposition("scan.pdf", false),
            r#"inline; filename="scan.pdf"; filename*=UTF-8''scan.pdf"#
        );
    }

    /// A quote or a backslash would end or escape the quoted string.
    #[test]
    fn quotes_and_backslashes_cannot_break_out_of_the_name() {
        assert_eq!(
            content_disposition(r#"a"b\c.pdf"#, true),
            r#"attachment; filename="a_b_c.pdf"; filename*=UTF-8''a%22b%5Cc.pdf"#
        );
    }

    /// A line break would start another header: none survives, in either
    /// form of the name.
    #[test]
    fn line_breaks_and_control_characters_are_dropped() {
        let value = content_disposition("a\r\nSet-Cookie: x=1\t.pdf", true);
        assert_eq!(
            value,
            r#"attachment; filename="aSet-Cookie: x=1.pdf"; filename*=UTF-8''aSet-Cookie%3A%20x%3D1.pdf"#
        );
        assert!(axum::http::HeaderValue::from_str(&value).is_ok());
    }

    /// The accented name travels percent-encoded in `filename*`, which
    /// every current browser reads; `filename` keeps an ASCII stand-in.
    #[test]
    fn accents_travel_in_the_extended_form() {
        assert_eq!(
            content_disposition("été.jpg", true),
            r#"attachment; filename="_t_.jpg"; filename*=UTF-8''%C3%A9t%C3%A9.jpg"#
        );
    }

    #[test]
    fn every_name_makes_a_valid_header() {
        for name in ["", "\u{7f}", "日本.png", "a;b=c.pdf", "%41.pdf", "\0"] {
            let value = content_disposition(name, false);
            assert!(
                axum::http::HeaderValue::from_str(&value).is_ok(),
                "{name:?}: {value}"
            );
        }
        assert_eq!(
            content_disposition("%41.pdf", false),
            r#"inline; filename="%41.pdf"; filename*=UTF-8''%2541.pdf"#
        );
    }

    // -- the upload buffer --------------------------------------------------

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
