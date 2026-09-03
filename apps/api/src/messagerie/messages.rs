use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::messagerie::{can_modify, MessageEvent};
use crate::AppState;

const MAX_CONTENT_CHARS: usize = 4000;
const DEFAULT_PAGE_LIMIT: i64 = 50;
const MAX_PAGE_LIMIT: i64 = 100;

#[derive(Deserialize)]
pub struct CreateMessageRequest {
    pub content: String,
}

#[derive(Deserialize)]
pub struct UpdateMessageRequest {
    pub content: String,
}

#[derive(Deserialize)]
pub struct ListMessagesQuery {
    pub before_created_at: Option<DateTime<Utc>>,
    pub before_id: Option<Uuid>,
    pub limit: Option<i64>,
    /// #73: when set, the response holds only messages the caller hasn't
    /// read yet (`created_at` after their `message_read_state.last_read_at`
    /// watermark, `unread_messages` below) instead of the newest page.
    #[serde(default)]
    pub unread: bool,
}

#[derive(Serialize, Clone)]
pub struct MessageResponse {
    pub id: Uuid,
    pub group_id: Uuid,
    pub created_by: Uuid,
    pub content: String,
    pub edited_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

struct MessageRow {
    id: Uuid,
    group_id: Uuid,
    created_by: Uuid,
    content: String,
    edited_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<MessageRow> for MessageResponse {
    fn from(r: MessageRow) -> Self {
        MessageResponse {
            id: r.id,
            group_id: r.group_id,
            created_by: r.created_by,
            content: r.content,
            edited_at: r.edited_at,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// Trims and validates message content against the 4000-char max (decision
/// #9), before any encryption/write happens (AC #8).
fn validate_content(content: &str) -> AppResult<&str> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest("content_required".into()));
    }
    if trimmed.chars().count() > MAX_CONTENT_CHARS {
        return Err(AppError::BadRequest("content_too_long".into()));
    }
    Ok(trimmed)
}

/// Which of `messages` are unread, given the caller's `last_read_at`
/// watermark (`None` — never read anything in this family — means every
/// message is unread). "Unread" is `created_at` strictly after the
/// watermark: a message created at exactly `last_read_at` is what set the
/// watermark there in the first place (an unlikely tie in practice, since
/// `last_read_at` is `now()` at write time, not copied from a message row),
/// so it counts as read, not unread.
///
/// Pulled out as pure logic (issue #73) so "what counts as unread" is
/// TDD'd without a database; `list_messages` below calls it on an
/// already-fetched, `created_at DESC`-ordered page rather than reimplementing
/// the rule in SQL — unread messages are exactly the prefix of that page
/// newer than the watermark, so no extra sort is needed.
fn unread_messages(
    messages: &[MessageResponse],
    last_read_at: Option<DateTime<Utc>>,
) -> Vec<MessageResponse> {
    match last_read_at {
        None => messages.to_vec(),
        Some(watermark) => messages
            .iter()
            .filter(|m| m.created_at > watermark)
            .cloned()
            .collect(),
    }
}

pub async fn create_message(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<CreateMessageRequest>,
) -> AppResult<impl IntoResponse> {
    let content = validate_content(&body.content)?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let row = sqlx::query_as!(
        MessageRow,
        r#"
        INSERT INTO messages (group_id, created_by, content)
        VALUES ($1, $2, pgp_sym_encrypt($3, $4))
        RETURNING id, group_id, created_by,
                  pgp_sym_decrypt(content, $4) as "content!",
                  edited_at, created_at, updated_at
        "#,
        group_id,
        auth.user_id,
        content,
        state.message_encryption_key,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    let message = MessageResponse::from(row);
    state
        .message_hubs
        .publish(
            group_id,
            MessageEvent::Created {
                message: message.clone(),
            },
        )
        .await;

    Ok((StatusCode::CREATED, Json(message)))
}

pub async fn list_messages(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Query(query): Query<ListMessagesQuery>,
) -> AppResult<impl IntoResponse> {
    let limit = query
        .limit
        .unwrap_or(DEFAULT_PAGE_LIMIT)
        .clamp(1, MAX_PAGE_LIMIT);

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    // Cursor pagination (decision #7): `(created_at, id) < (before_created_at,
    // before_id)` when a cursor is supplied, else the most recent page.
    // Fetches `limit + 1` rows to compute `has_more` without a second query.
    let rows = sqlx::query_as!(
        MessageRow,
        r#"
        SELECT id, group_id, created_by,
               pgp_sym_decrypt(content, $4) as "content!",
               edited_at, created_at, updated_at
        FROM messages
        WHERE group_id = $1
          AND ($2::timestamptz IS NULL OR (created_at, id) < ($2, $3))
        ORDER BY created_at DESC, id DESC
        LIMIT $5
        "#,
        group_id,
        query.before_created_at,
        query.before_id,
        state.message_encryption_key,
        limit + 1,
    )
    .fetch_all(&mut *tx)
    .await?;

    let has_more = rows.len() as i64 > limit;
    let mut messages: Vec<MessageResponse> = rows
        .into_iter()
        .take(limit as usize)
        .map(MessageResponse::from)
        .collect();

    // `has_more` above describes the raw (unfiltered) page; once the unread
    // filter drops some of it, "is there another page" would need its own
    // query (there's no unread `?unread=true` pagination UI to serve today
    // — issue #73 asked for a dashboard preview, not a full unread inbox),
    // so it's reported as `false` rather than a number that doesn't
    // actually describe what's left.
    let has_more = if query.unread {
        let last_read_at = sqlx::query_scalar!(
            "SELECT last_read_at FROM message_read_state WHERE group_id = $1 AND user_id = $2",
            group_id,
            auth.user_id,
        )
        .fetch_optional(&mut *tx)
        .await?;
        messages = unread_messages(&messages, last_read_at);
        false
    } else {
        has_more
    };
    tx.commit().await?;

    Ok(Json(json!({ "messages": messages, "has_more": has_more })))
}

/// Advances the caller's read watermark for this family to now — everything
/// created up to this instant is "read" from here on (issue #73). Called
/// when the member opens `/messagerie`
/// (`apps/web/src/routes/messagerie/thread.rs`). Idempotent by design
/// (`ON CONFLICT … DO UPDATE`): opening the page twice in a row just moves
/// the watermark forward twice, which is a no-op in effect.
pub async fn mark_messages_read(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    sqlx::query!(
        r#"
        INSERT INTO message_read_state (group_id, user_id, last_read_at)
        VALUES ($1, $2, now())
        ON CONFLICT (group_id, user_id) DO UPDATE SET last_read_at = now()
        "#,
        group_id,
        auth.user_id,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

pub async fn update_message(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, message_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateMessageRequest>,
) -> AppResult<impl IntoResponse> {
    let content = validate_content(&body.content)?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;

    // FOR UPDATE locks the row for the rest of this transaction, mirroring
    // budget/entries.rs's concurrent-write protection (also what guards the
    // edit/delete race: the losing transaction's UPDATE/DELETE below simply
    // matches zero rows once the other has committed).
    let existing = sqlx::query!(
        "SELECT created_by FROM messages WHERE id = $1 AND group_id = $2 FOR UPDATE",
        message_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    if !can_modify(&actor_role, existing.created_by == auth.user_id) {
        return Err(AppError::Forbidden);
    }

    let row = sqlx::query_as!(
        MessageRow,
        r#"
        UPDATE messages SET
            content = pgp_sym_encrypt($3, $4),
            edited_at = now(),
            updated_at = now()
        WHERE id = $1 AND group_id = $2
        RETURNING id, group_id, created_by,
                  pgp_sym_decrypt(content, $4) as "content!",
                  edited_at, created_at, updated_at
        "#,
        message_id,
        group_id,
        content,
        state.message_encryption_key,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    tx.commit().await?;

    let message = MessageResponse::from(row);
    state
        .message_hubs
        .publish(
            group_id,
            MessageEvent::Updated {
                message: message.clone(),
            },
        )
        .await;

    Ok(Json(message))
}

pub async fn delete_message(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, message_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;

    let existing = sqlx::query!(
        "SELECT created_by FROM messages WHERE id = $1 AND group_id = $2 FOR UPDATE",
        message_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    if !can_modify(&actor_role, existing.created_by == auth.user_id) {
        return Err(AppError::Forbidden);
    }

    let deleted = sqlx::query!(
        "DELETE FROM messages WHERE id = $1 AND group_id = $2",
        message_id,
        group_id
    )
    .execute(&mut *tx)
    .await?;
    if deleted.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    tx.commit().await?;

    state
        .message_hubs
        .publish(group_id, MessageEvent::Deleted { id: message_id })
        .await;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn err_msg(result: AppResult<&str>) -> String {
        match result {
            Err(AppError::BadRequest(m)) => m,
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(validate_content("  hello  ").unwrap(), "hello");
    }

    #[test]
    fn rejects_empty_content() {
        assert_eq!(err_msg(validate_content("")), "content_required");
    }

    #[test]
    fn rejects_whitespace_only_content() {
        assert_eq!(err_msg(validate_content("   \n\t  ")), "content_required");
    }

    #[test]
    fn accepts_content_at_max_length() {
        let content = "a".repeat(MAX_CONTENT_CHARS);
        assert_eq!(validate_content(&content).unwrap(), content);
    }

    #[test]
    fn rejects_content_over_max_length() {
        let content = "a".repeat(MAX_CONTENT_CHARS + 1);
        assert_eq!(err_msg(validate_content(&content)), "content_too_long");
    }

    #[test]
    fn counts_chars_not_bytes_for_max_length() {
        // multi-byte chars must be counted as one char each, not as bytes
        let content = "é".repeat(MAX_CONTENT_CHARS);
        assert!(validate_content(&content).is_ok());
    }

    // -- unread_messages ----------------------------------------------

    fn message_at(hour: u32) -> MessageResponse {
        let created_at = chrono::Utc
            .with_ymd_and_hms(2026, 9, 3, hour, 0, 0)
            .unwrap();
        MessageResponse {
            id: Uuid::new_v4(),
            group_id: Uuid::nil(),
            created_by: Uuid::nil(),
            content: "hello".to_string(),
            edited_at: None,
            created_at,
            updated_at: created_at,
        }
    }

    #[test]
    fn never_read_before_means_every_message_is_unread() {
        let messages = vec![message_at(8), message_at(9)];
        assert_eq!(unread_messages(&messages, None).len(), 2);
    }

    #[test]
    fn only_messages_after_the_watermark_are_unread() {
        let watermark = chrono::Utc.with_ymd_and_hms(2026, 9, 3, 10, 0, 0).unwrap();
        let messages = vec![message_at(9), message_at(11), message_at(12)];
        let unread = unread_messages(&messages, Some(watermark));
        assert_eq!(unread.len(), 2);
        assert!(unread.iter().all(|m| m.created_at > watermark));
    }

    #[test]
    fn a_message_created_exactly_at_the_watermark_is_read_not_unread() {
        let watermark = chrono::Utc.with_ymd_and_hms(2026, 9, 3, 10, 0, 0).unwrap();
        let mut m = message_at(9);
        m.created_at = watermark;
        assert!(unread_messages(&[m], Some(watermark)).is_empty());
    }

    #[test]
    fn everything_before_the_watermark_leaves_nothing_unread() {
        let watermark = chrono::Utc.with_ymd_and_hms(2026, 9, 3, 23, 0, 0).unwrap();
        let messages = vec![message_at(8), message_at(9)];
        assert!(unread_messages(&messages, Some(watermark)).is_empty());
    }

    #[test]
    fn an_empty_message_list_has_nothing_unread() {
        let watermark = chrono::Utc.with_ymd_and_hms(2026, 9, 3, 10, 0, 0).unwrap();
        assert!(unread_messages(&[], Some(watermark)).is_empty());
        assert!(unread_messages(&[], None).is_empty());
    }
}
