pub mod messages;
pub mod ws;

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{broadcast, Mutex};
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

const HUB_CHANNEL_CAPACITY: usize = 100;

/// Per-family in-memory broadcast registry (single API instance in both v1
/// and current v2 planning — see TODOS.md for the multi-instance caveat).
/// Entries are created lazily on first subscriber/publish and removed by
/// the WS disconnect handler once a family's receiver count hits zero, so
/// the map doesn't grow unbounded across the process lifetime.
#[derive(Clone, Default)]
pub struct MessageHub {
    channels: Arc<Mutex<HashMap<Uuid, broadcast::Sender<MessageEvent>>>>,
}

impl MessageHub {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn subscribe(&self, group_id: Uuid) -> broadcast::Receiver<MessageEvent> {
        let mut channels = self.channels.lock().await;
        channels
            .entry(group_id)
            .or_insert_with(|| broadcast::channel(HUB_CHANNEL_CAPACITY).0)
            .subscribe()
    }

    pub async fn publish(&self, group_id: Uuid, event: MessageEvent) {
        let mut channels = self.channels.lock().await;
        if let Some(sender) = channels.get(&group_id) {
            // No receivers means no one is connected right now; that's not
            // an error, just drop the event (WS has no replay buffer by
            // design, decision #10 — clients re-fetch via GET on reconnect).
            let _ = sender.send(event);
        } else {
            let (sender, _) = broadcast::channel(HUB_CHANNEL_CAPACITY);
            let _ = sender.send(event);
            channels.insert(group_id, sender);
        }
    }

    /// Removes a family's channel once its subscriber count drops to zero.
    /// Called from the WS disconnect handler — nothing else would trigger
    /// this cleanup, since publish/subscribe alone can't observe zero
    /// receivers after the fact.
    pub async fn cleanup_if_empty(&self, group_id: Uuid) {
        let mut channels = self.channels.lock().await;
        if let Some(sender) = channels.get(&group_id) {
            if sender.receiver_count() == 0 {
                channels.remove(&group_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::can_modify;

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
