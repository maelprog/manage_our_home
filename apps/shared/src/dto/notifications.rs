//! Wire shapes of the reminder-channel preferences and the push
//! subscriptions (#306): `GET`/`PUT /account/notifications`,
//! `POST`/`DELETE /account/push-subscriptions`
//! (`apps/api/src/notifications/preferences.rs`); and of the notification
//! centre (#534), `/notifications` (`apps/api/src/notifications/center.rs`);
//! and of a member's upcoming reminders (#667),
//! `GET /account/reminders/upcoming` (`apps/api/src/notifications/upcoming.rs`).

use serde::{Deserialize, Serialize};

/// `GET /account/notifications`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
    /// The Paris hour, 0 to 23, of the daily recap of the stock items
    /// nearing their expiry date (`users.expiry_digest_hour`, #700).
    pub expiry_digest_hour: i16,
}

/// `PUT /account/notifications`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateNotificationSettings {
    pub reminder_channel: String,
    /// The recap's hour, 0 to 23 (#700); left as it is when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry_digest_hour: Option<i64>,
}

/// `POST /account/push-subscriptions`: the endpoint of the browser's
/// `PushSubscription`, and nothing else of it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PushSubscriptionRequest {
    pub endpoint: String,
}

/// One entry of the notification centre (#534), `GET /notifications`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NotificationResponse {
    pub id: uuid::Uuid,
    /// The family the event happened in — for an invitation, the family the
    /// recipient is invited into, of which they are not a member yet.
    pub group_id: uuid::Uuid,
    /// `agenda_reminder`, `stock_expiry`, `message`, `invitation`,
    /// `grocery_item` or `budget_alert` (#536).
    pub kind: String,
    /// The event, stock item, message, invitation or grocery item it is
    /// about, per `kind`; for a budget alert, the category whose cap was
    /// exceeded, none for the family's cap. It may have been deleted since.
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NotificationPage {
    pub notifications: Vec<NotificationResponse>,
    pub has_more: bool,
}

/// `GET /notifications/unread_count`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UnreadNotificationCount {
    pub unread_count: i64,
}

/// One entry of `GET /account/reminders/upcoming` (#667): the next time a
/// reminder of the member's falls due, which the mobile application
/// schedules on the device so that it fires without network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpcomingReminder {
    /// The `scheduled_notifications` row, one per reminder and occurrence:
    /// the same from one call to the next, so the key a device schedules
    /// it under, and cancels it by once it is no longer listed.
    pub id: uuid::Uuid,
    pub event_id: uuid::Uuid,
    /// The family the event belongs to.
    pub group_id: uuid::Uuid,
    /// The event's title.
    pub title: String,
    /// The start of the occurrence the reminder is for.
    pub occurrence_at: chrono::DateTime<chrono::Utc>,
    /// When the reminder falls due.
    pub fire_at: chrono::DateTime<chrono::Utc>,
    pub all_day: bool,
}
