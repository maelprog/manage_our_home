use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::AppResult;
use crate::groups::require_role;
use crate::AppState;

/// How often the connection re-validates the caller is still a group
/// member, bounding how long a removed member (`DELETE /groups/:id/
/// members/:user_id`) stays connected (AC #7). Per-message revalidation on
/// every broadcast send was considered and dropped in review: it would
/// mean a synchronous DB round-trip per event per connection on every
/// fan-out, turning a low-latency push into a DB-bound RPC. Writes
/// (POST/PATCH/DELETE) already re-run `require_role` on every request, so
/// this tick only needs to cover the read-only, otherwise-silent WS leg.
const MEMBERSHIP_RECHECK_INTERVAL: Duration = Duration::from_secs(30);

pub async fn message_ws(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    ws: WebSocketUpgrade,
) -> AppResult<impl IntoResponse> {
    // Same 403-if-not-a-member bar as any REST handler, checked before the
    // upgrade so a non-member gets a normal HTTP 403 rather than a socket
    // that's opened and then dropped.
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    tx.commit().await?;

    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state, group_id, auth.user_id)))
}

async fn is_still_member(state: &AppState, group_id: Uuid, user_id: Uuid) -> bool {
    match scoped_tx(&state.db, group_id, user_id).await {
        Ok(mut tx) => {
            let member = require_role(&mut tx, group_id, user_id).await.is_ok();
            let _ = tx.commit().await;
            member
        }
        Err(_) => false,
    }
}

async fn handle_socket(mut socket: WebSocket, state: AppState, group_id: Uuid, user_id: Uuid) {
    let mut events = state.message_hubs.subscribe(group_id).await;
    let mut recheck = tokio::time::interval(MEMBERSHIP_RECHECK_INTERVAL);
    recheck.tick().await; // first tick fires immediately; skip it

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {
                        // Receive-only push channel (decision #11) — any
                        // client frame other than ping/pong/close is
                        // ignored rather than rejected.
                    }
                    Some(Err(_)) => break,
                }
            }
            event = events.recv() => {
                let event = match event {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                let Ok(payload) = serde_json::to_string(&event) else { continue };
                if socket.send(Message::Text(payload)).await.is_err() {
                    break;
                }
            }
            _ = recheck.tick() => {
                if !is_still_member(&state, group_id, user_id).await {
                    break;
                }
            }
        }
    }

    drop(events);
    state.message_hubs.cleanup_if_empty(group_id).await;
}
