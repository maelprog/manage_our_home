# TODOS

## Revisit broadcast-based WS fan-out if v2 ever moves the API to multiple instances

**What:** `tokio::sync::broadcast`-based per-family message fan-out (Epic #7 Messagerie, decision #3) only works within a single API process.

**Why:** If v2 deployment ever splits the API into multiple instances, cross-instance WebSocket delivery silently stops working — no error, no crash, just messages that never reach some connected clients.

**Context:** v1 and current v2 planning (`docs/v2-deployment.md`) both assume a single API instance. If that assumption breaks, fan-out needs to move to Postgres `LISTEN/NOTIFY` or an external pub/sub layer (Redis, NATS). No action needed until v2 deployment shape is decided.

**Depends on / blocked by:** v2 deployment scope decision (`docs/v2-deployment.md`).
