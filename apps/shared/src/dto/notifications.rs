//! Wire shapes of the reminder-channel preferences and the push
//! subscriptions (#306): `GET`/`PUT /account/notifications`,
//! `POST`/`DELETE /account/push-subscriptions`
//! (`apps/api/src/notifications/preferences.rs`).

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
