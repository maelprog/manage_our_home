pub mod messages;
pub mod ws;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::postgres::PgListener;
use sqlx::{PgConnection, PgPool};
use tokio::sync::{broadcast, oneshot, Mutex, OnceCell};
use uuid::Uuid;

use crate::messagerie::messages::MessageResponse;

/// Same permission bar as Budget/Stocks/Recipes/Grocery list: any member may
/// create or read a message, but only the message's author or a group
/// admin/owner may edit or delete it.
pub(crate) fn can_modify(actor_role: &str, actor_is_creator: bool) -> bool {
    actor_is_creator || actor_role == "owner" || actor_role == "admin"
}

/// WS push events, tagged by `type`, mirroring the REST `MessageResponse`
/// shape for `created`/`updated` so clients can reuse one deserializer.
#[derive(Clone, Serialize)]
#[serde(tag = "type")]
pub enum MessageEvent {
    #[serde(rename = "message.created")]
    Created { message: MessageResponse },
    #[serde(rename = "message.updated")]
    Updated { message: MessageResponse },
    #[serde(rename = "message.deleted")]
    Deleted { id: Uuid },
}

/// The Postgres channel every API replica listens on (#429).
pub(crate) const NOTIFY_CHANNEL: &str = "messagerie_events";

/// What changed, as carried by a cross-replica notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Change {
    Created,
    Updated,
    Deleted,
}

/// The payload of a `NOTIFY` on [`NOTIFY_CHANNEL`]: identifiers only.
/// Postgres refuses a payload of 8000 bytes or more, and a message may hold
/// 4000 characters; the content also stays out of a channel any session on
/// the database can `LISTEN` to. Each replica reads the message back under
/// the group's RLS scope before pushing it (`messages::load_message`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Notification {
    pub group_id: Uuid,
    pub change: Change,
    pub message_id: Uuid,
}

pub(crate) fn encode_notification(n: &Notification) -> String {
    // Two UUIDs and a unit enum cannot fail to serialize.
    serde_json::to_string(n).expect("Notification serializes")
}

pub(crate) fn decode_notification(payload: &str) -> Option<Notification> {
    serde_json::from_str(payload).ok()
}

/// Announces a change to every replica, the one serving this request
/// included: none pushes it to its sockets before reading it back from the
/// channel, so each connected client gets it once (#429). Call it inside
/// the transaction that writes the change: Postgres delivers a `NOTIFY`
/// only once that transaction commits, and never if it rolls back.
pub(crate) async fn notify(
    conn: &mut PgConnection,
    notification: Notification,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(NOTIFY_CHANNEL)
        .bind(encode_notification(&notification))
        .execute(conn)
        .await?;
    Ok(())
}

const HUB_CHANNEL_CAPACITY: usize = 100;

/// Pause between two attempts to re-establish the listening connection.
const LISTENER_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Per-family broadcast registry of this replica's WebSocket connections,
/// fed by a Postgres `LISTEN` on [`NOTIFY_CHANNEL`] (#429), so that a write
/// on any replica reaches the sockets of all of them.
/// Entries are created lazily on first subscriber and removed by the WS
/// disconnect handler once a family's receiver count hits zero, so the map
/// doesn't grow unbounded across the process lifetime.
#[derive(Clone)]
pub struct MessageHub {
    channels: Arc<Mutex<HashMap<Uuid, broadcast::Sender<MessageEvent>>>>,
    db: PgPool,
    message_encryption_key: Arc<str>,
    listening: Arc<OnceCell<()>>,
}

impl MessageHub {
    /// `db` is the runtime pool: the listener holds one of its connections
    /// for as long as it runs, and reads messages back through it under
    /// RLS. Nothing listens until [`Self::ensure_listening`] is awaited.
    pub fn new(db: PgPool, message_encryption_key: &str) -> Self {
        Self {
            channels: Arc::default(),
            db,
            message_encryption_key: message_encryption_key.into(),
            listening: Arc::default(),
        }
    }

    /// Starts this replica's listener on first call and returns once it
    /// listens; later calls return at once. `main` awaits it before
    /// serving, and the WS upgrade awaits it too, so a socket never opens
    /// on a replica that does not listen yet.
    pub async fn ensure_listening(&self) {
        self.listening
            .get_or_init(|| async {
                let (ready, listening) = oneshot::channel();
                tokio::spawn(self.clone().run_listener(ready));
                let _ = listening.await;
            })
            .await;
    }

    pub async fn subscribe(&self, group_id: Uuid) -> broadcast::Receiver<MessageEvent> {
        let mut channels = self.channels.lock().await;
        channels
            .entry(group_id)
            .or_insert_with(|| broadcast::channel(HUB_CHANNEL_CAPACITY).0)
            .subscribe()
    }

    /// Removes a family's channel once its subscriber count drops to zero.
    /// Called from the WS disconnect handler — nothing else would trigger
    /// this cleanup, since subscribe/dispatch alone can't observe zero
    /// receivers after the fact.
    pub async fn cleanup_if_empty(&self, group_id: Uuid) {
        let mut channels = self.channels.lock().await;
        if let Some(sender) = channels.get(&group_id) {
            if sender.receiver_count() == 0 {
                channels.remove(&group_id);
            }
        }
    }

    /// Pushes `event` to this replica's sockets of `group_id`. No receivers
    /// means no one is connected here right now; that's not an error, the
    /// event is dropped (WS has no replay buffer by design, decision #10 —
    /// clients re-fetch via GET on reconnect).
    async fn dispatch(&self, group_id: Uuid, event: MessageEvent) {
        if let Some(sender) = self.channels.lock().await.get(&group_id) {
            let _ = sender.send(event);
        }
    }

    async fn has_subscribers(&self, group_id: Uuid) -> bool {
        self.channels.lock().await.contains_key(&group_id)
    }

    /// Ends every socket of this replica: dropping a family's sender closes
    /// its receivers, and `ws::handle_socket` closes the socket on that.
    /// `apps/web` re-fetches the thread on any close, which is how its
    /// clients catch up on what was notified while nothing listened.
    async fn close_all(&self) {
        self.channels.lock().await.clear();
    }

    async fn handle(&self, payload: &str) {
        let Some(notification) = decode_notification(payload) else {
            tracing::warn!("ignoring a malformed messagerie notification");
            return;
        };
        // Reading a message back costs a round-trip and a decryption: skip
        // it for the groups nobody follows on this replica.
        if !self.has_subscribers(notification.group_id).await {
            return;
        }
        let Notification {
            group_id,
            change,
            message_id,
        } = notification;
        let event = match change {
            Change::Deleted => Some(MessageEvent::Deleted { id: message_id }),
            Change::Created | Change::Updated => {
                let key = &self.message_encryption_key;
                match messages::load_message(&self.db, key, group_id, message_id).await {
                    Ok(message) => message.map(|message| match change {
                        Change::Created => MessageEvent::Created { message },
                        _ => MessageEvent::Updated { message },
                    }),
                    Err(e) => {
                        tracing::warn!("messagerie notification not pushed: {e}");
                        None
                    }
                }
            }
        };
        // `None`: the message was deleted before it could be read back, and
        // its own `deleted` notification follows.
        if let Some(event) = event {
            self.dispatch(group_id, event).await;
        }
    }

    /// Listens until the pool closes, re-establishing the connection
    /// whenever it drops. What was notified while nothing listened is lost,
    /// so every re-establishment closes this replica's sockets
    /// (`close_all`).
    async fn run_listener(self, ready: oneshot::Sender<()>) {
        let mut ready = Some(ready);
        loop {
            let mut listener = match self.connect_listener().await {
                Ok(listener) => listener,
                Err(_) if self.db.is_closed() => return,
                Err(e) => {
                    tracing::warn!("messagerie listener cannot connect: {e}");
                    tokio::time::sleep(LISTENER_RETRY_DELAY).await;
                    continue;
                }
            };
            match ready.take() {
                Some(ready) => {
                    let _ = ready.send(());
                }
                None => self.close_all().await,
            }
            loop {
                match listener.try_recv().await {
                    Ok(Some(notification)) => self.handle(notification.payload()).await,
                    // The connection dropped and sqlx re-established it:
                    // whatever was notified in between is lost.
                    Ok(None) => {
                        tracing::warn!("messagerie listener reconnected");
                        self.close_all().await;
                    }
                    Err(_) if self.db.is_closed() => return,
                    Err(e) => {
                        tracing::warn!("messagerie listener lost: {e}");
                        break;
                    }
                }
            }
            tokio::time::sleep(LISTENER_RETRY_DELAY).await;
        }
    }

    async fn connect_listener(&self) -> Result<PgListener, sqlx::Error> {
        let mut listener = PgListener::connect_with(&self.db).await?;
        listener.listen(NOTIFY_CHANNEL).await?;
        Ok(listener)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(change: Change) -> Notification {
        Notification {
            group_id: Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888),
            change,
            message_id: Uuid::from_u128(0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0000),
        }
    }

    #[test]
    fn every_change_round_trips() {
        for change in [Change::Created, Change::Updated, Change::Deleted] {
            let n = sample(change);
            assert_eq!(decode_notification(&encode_notification(&n)), Some(n));
        }
    }

    /// Postgres refuses a `NOTIFY` payload of 8000 bytes or more; a message
    /// alone may reach 4000 characters, so 16 000 bytes of UTF-8. The
    /// payload carries identifiers only and stays far below the limit.
    #[test]
    fn payload_carries_identifiers_only_and_fits_notify() {
        let encoded = encode_notification(&sample(Change::Updated));
        assert!(encoded.len() < 200, "{} bytes: {encoded}", encoded.len());
        let fields: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        let mut keys: Vec<&str> = fields
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["change", "group_id", "message_id"]);
    }

    #[test]
    fn wire_spelling_of_changes_is_stable() {
        let encoded = encode_notification(&sample(Change::Deleted));
        assert!(encoded.contains(r#""change":"deleted""#), "{encoded}");
    }

    #[test]
    fn malformed_payloads_are_rejected() {
        let group = "11112222-3333-4444-5555-666677778888";
        let message = "9999aaaa-bbbb-cccc-dddd-eeeeffff0000";
        for payload in [
            String::new(),
            "not json".to_string(),
            format!(r#"{{"group_id":"{group}","change":"created"}}"#),
            format!(r#"{{"group_id":"{group}","change":"read","message_id":"{message}"}}"#),
            format!(r#"{{"group_id":"nope","change":"created","message_id":"{message}"}}"#),
        ] {
            assert_eq!(decode_notification(&payload), None, "{payload}");
        }
    }

    #[test]
    fn author_can_modify_own_message() {
        assert!(can_modify("standard", true));
    }

    #[test]
    fn non_author_standard_cannot_modify() {
        assert!(!can_modify("standard", false));
    }

    #[test]
    fn admin_and_owner_can_modify_others_messages() {
        assert!(can_modify("admin", false));
        assert!(can_modify("owner", false));
    }
}
