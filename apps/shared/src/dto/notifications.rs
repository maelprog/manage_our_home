//! Wire shapes of the reminder-channel preferences and the push
//! subscriptions (#306): `GET`/`PUT /account/notifications`,
//! `POST`/`DELETE /account/push-subscriptions`
//! (`apps/api/src/notifications/preferences.rs`); and of the notification
//! centre (#534), `/notifications` (`apps/api/src/notifications/center.rs`).

use serde::{Deserialize, Serialize};

/// `GET /account/notifications`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationSettings {
    /// `push`, `email` or `both` (`users.reminder_channel`).
    pub reminder_channel: String,
    /// Devices currently subscribed for this account, whatever their
    /// browser. Expired ones are deleted as soon as a push service says so.
    pub push_subscriptions: i64,
    /// The server's VAPID public key (unpadded base64url), the
    /// `applicationServerKey` a browser subscribes with — `None` when this
    /// server has notifications turned off (no `VAPID_PRIVATE_KEY`).
    pub vapid_public_key: Option<String>,
}

/// `PUT /account/notifications`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateNotificationSettings {
    pub reminder_channel: String,
}

/// `POST /account/push-subscriptions`: the endpoint of the browser's
/// `PushSubscription`, and nothing else of it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushSubscriptionRequest {
    pub endpoint: String,
}

/// One entry of the notification centre (#534), `GET /notifications`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationResponse {
    pub id: uuid::Uuid,
    /// The family the event happened in — for an invitation, the family the
    /// recipient is invited into, of which they are not a member yet.
    pub group_id: uuid::Uuid,
    /// `agenda_reminder`, `stock_expiry`, `message`, `invitation` or
    /// `grocery_item`.
    pub kind: String,
    /// The event, stock item, message, invitation or grocery item it is
    /// about, per `kind`. It may have been deleted since.
    pub object_id: Option<uuid::Uuid>,
    /// The short text to show, in French, on one line.
    pub body: String,
    /// `None` while unread.
    pub read_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// `GET /notifications`: newest first. The next page is asked with the
/// last entry's `created_at` and `id` as `before_created_at`/`before_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPage {
    pub notifications: Vec<NotificationResponse>,
    pub has_more: bool,
}

/// `GET /notifications/unread_count`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnreadNotificationCount {
    pub unread_count: i64,
}
