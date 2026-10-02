use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::auth::session::{is_full_session, scoped_tx, AuthUser};
use crate::error::AppResult;
use crate::groups::require_role;
use crate::AppState;

// How often the connection re-validates the caller is still a group
// member, bounding how long a removed member (`DELETE /groups/:id/
// members/:user_id`) stays connected (AC #7). Per-message revalidation on
// every broadcast send was considered and dropped in review: it would
// mean a synchronous DB round-trip per event per connection on every
// fan-out, turning a low-latency push into a DB-bound RPC. Writes
// (POST/PATCH/DELETE) already re-run `require_role` on every request, so
// this tick only needs to cover the read-only, otherwise-silent WS leg.
// The interval lives on `AppState` (`message_ws_recheck_interval`, 30s in
// production) so the AC #7 flow test can shorten the bound instead of
// sleeping 30s for real.
//
// The same tick re-reads the session (#221): authenticated once at the
// upgrade, the socket would otherwise outlive a logout, a password change
// or reset (which revoke the other sessions precisely to cut off a thief),
// a deactivation, the expiry or the inactivity timeout. Same 30s bound as a
// lost membership.

/// Close code sent when the session behind the socket no longer opens the
/// messagerie (#221), in the application range (4000-4999). A lost
/// membership still ends the socket without a close frame; `apps/web`
/// tells the two apart by re-fetching the page, either way.
pub const CLOSE_SESSION_ENDED: u16 = 4401;

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

    Ok(ws.on_upgrade(move |socket| {
        handle_socket(socket, state, group_id, auth.user_id, auth.session_id)
    }))
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

async fn handle_socket(
    mut socket: WebSocket,
    state: AppState,
    group_id: Uuid,
    user_id: Uuid,
    session_id: Uuid,
) {
    let mut events = state.message_hubs.subscribe(group_id).await;
    let mut recheck = tokio::time::interval(state.message_ws_recheck_interval);
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
                if !is_full_session(&state.db, session_id).await {
                    let _ = socket
                        .send(Message::Close(Some(CloseFrame {
                            code: CLOSE_SESSION_ENDED,
                            reason: "session_ended".into(),
                        })))
                        .await;
                    break;
                }
                if !is_still_member(&state, group_id, user_id).await {
                    break;
                }
            }
        }
    }

    drop(events);
    state.message_hubs.cleanup_if_empty(group_id).await;
}
