//! Request/response shapes for the Messagerie endpoints
//! (`apps/api/src/messagerie/`), consumed by `apps/web`'s SSR client. Kept
//! field-for-field identical to `apps/api`'s wire structs so there is one
//! documented shape; only the fields `apps/web` needs are declared (serde
//! ignores extras on deserialize). The backend is *not* modified by this epic
//! — these mirror it, they don't replace it.
//!
//! `content` is plain text on the wire in both directions: the backend stores
//! it encrypted (`pgp_sym_encrypt` with a dedicated `MESSAGE_ENCRYPTION_KEY`)
//! and decrypts on read, which is entirely transparent to the front.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `POST /groups/:id/messages` request body. Mirrors `CreateMessageRequest`.
/// The backend trims and validates (`content_required` / `content_too_long`,
/// 4000 chars max); `validation::messagerie::validate_content` mirrors both
/// rules so the form can reject without a round trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateMessageRequest {
    pub content: String,
}

/// `PATCH /groups/:id/messages/:message_id` request body. Mirrors
/// `UpdateMessageRequest`. **Not** a partial update: `content` is the whole
/// message and is required (there is no other editable field, so there is no
/// `Option`/"leave as-is" state to model).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateMessageRequest {
    pub content: String,
}

/// One message. Mirrors `MessageResponse`. `created_by` is a user id only —
/// the thread resolves display names separately via `GET /groups/:id`
/// (`validation::messagerie::author_name`). `edited_at` is `Some` once the
/// message has been edited, which is what the `modifié` marker renders from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageResponse {
    pub id: Uuid,
    pub group_id: Uuid,
    pub created_by: Uuid,
    pub content: String,
    pub edited_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `GET /groups/:id/messages` response envelope
/// (`{ "messages": [...], "has_more": bool }`).
///
/// Cursor pagination, **newest first**: pass the oldest rendered message's
/// `created_at`/`id` back as `before_created_at`/`before_id` (plus an optional
/// `limit`, default 50, max 100) to get the window before it. `has_more` is
/// computed backend-side by fetching `limit + 1` rows, so it means "there are
/// older messages", never "there are newer ones".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageList {
    pub messages: Vec<MessageResponse>,
    pub has_more: bool,
}

/// The push-only WebSocket contract of `GET /groups/:id/messages/ws`, mirroring
/// `apps/api/src/messagerie/mod.rs::MessageEvent` (tagged by `type`).
///
/// Declared here so the wire shape has one documented home, but note that
/// `apps/web` never deserializes it: per `docs/front-epic-8-messagerie.md`, the
/// browser treats every frame as a bare "something changed" signal and asks the
/// server to re-render the thread, so no client-side renderer can drift from
/// the Rust one. Writes never go over this socket — they are REST only.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MessageEvent {
    #[serde(rename = "message.created")]
    Created { message: MessageResponse },
    #[serde(rename = "message.updated")]
    Updated { message: MessageResponse },
    #[serde(rename = "message.deleted")]
    Deleted { id: Uuid },
}
