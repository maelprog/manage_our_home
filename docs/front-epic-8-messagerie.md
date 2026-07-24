# Front epic F8 — Messagerie (issue #23)

SSR spec at the depth F1 (#15) established: route table, error tables mapping
`apps/api/src/messagerie/`'s exact status/error codes to French copy, and
acceptance criteria. Backend is already shipped (`apps/api/src/messagerie/`,
migration `0008_messagerie.sql`, v1-scope row #7) — this epic is the `apps/web`
client only, following the `apps/web/src/routes/grocery_list/*` and
`apps/web/src/routes/budget/*` pattern (Leptos SSR, plain `<form method=post>`,
PRG with `?notice=`/`?error=` codes, `FamilyContext`/`family_context`, per-page
error tables, `can_modify` mirrored front-side for control visibility — the
backend stays the authority).

Independent of F3–F7; depends only on F2 (Groups) for the active-family
context. One thread per family, text only, no DMs, no attachments.

The one thing no previous front epic had: a **WebSocket**. `GET
/groups/:id/messages/ws` is push-only and fans out `message.created` /
`message.updated` / `message.deleted`; every write still goes through REST.
The decisions below are mostly about how a server-rendered, no-JS-first app
absorbs a live channel without growing a client-side rendering layer.

## Design decisions

### The WebSocket carries a *signal*, not a view — the server stays the renderer

The obvious way to use the push channel is to deserialize `message.created` in
JavaScript and build the row in the DOM. We deliberately don't: that would
mean a second renderer (escaping, the `Modifier`/`Supprimer` permission bar,
timestamp formatting, the "modifié" marker) written in JS and free to drift
from the Rust one — exactly the drift `apps/shared` exists to prevent.

Instead the inline script treats **every** WS frame as a bare "something
changed" signal (it never reads the payload) and re-fetches the *current URL*,
parses the response with `DOMParser`, and swaps `#thread`'s `innerHTML` with
the freshly server-rendered fragment. The permission bar, the escaping and the
copy therefore have exactly one implementation, in Rust, and the push channel
needs no client-side model of a message at all. Bursts are coalesced (120 ms)
so ten messages in a second cost one re-render. The cost — one extra HTTP round
trip per burst instead of a pure-push DOM insert — is irrelevant at family
scale (a handful of connected members, one thread).

`location.reload()` would have been even simpler but throws away whatever the
composer has typed and the scroll position; the fragment swap keeps both.

### No JS is the baseline, not the fallback

Everything on `/messagerie` is a plain `<form method=post>`: sending, editing
and deleting all work with JavaScript switched off, exactly like the rest of
`apps/web`. With no JS, the thread is simply what the last page load rendered —
a reload shows new messages. The script adds live updates on top and is the
only optional part of the page; it bails out immediately (`return`) when
`window.WebSocket` is missing, and nothing else on the page depends on it.

Consequences that shaped the markup:

- **Editing is inline, in a native `<details>` disclosure** on the message row,
  not a dedicated `/messagerie/:id` screen the way F6/F7 do it. Two reasons.
  First, the backend exposes **no `GET /groups/:id/messages/:message_id`** —
  only the paginated list — so an edit *screen* would have to re-derive one
  message from a cursor page just to pre-fill a textarea it already had. Second,
  a chat row already carries the full content, so the round trip buys nothing.
  `<details>`/`<summary>` gives the collapse behaviour with zero JS.
- **The composer keeps its text on a rejected send.** Every other epic PRGs on
  every outcome; here a rejected `POST /messagerie` re-renders the thread page
  inline (HTTP 200) with the error banner *and* the submitted content back in
  the textarea. Losing a long message to a round trip is a real cost, and there
  is nothing to guard against: the rejected write never happened, so there is no
  double-submit for PRG to prevent. Success still PRGs
  (`/messagerie?notice=message_sent`). Same rule for a rejected edit: the row's
  `<details>` comes back open, pre-filled with what was typed.

### The browser talks to `apps/api` directly for the socket

`apps/web`'s SSR layer reaches `apps/api` over the internal network
(`API_INTERNAL_BASE_URL`), which the browser cannot use. Rather than proxy the
socket through `apps/web` (a second Axum WS leg, its own backpressure and
lifecycle bugs, for no functional gain), the page hands the browser a URL built
from `API_PUBLIC_BASE_URL` — the same mechanism the Google OAuth button has used
since F1, and the same reason it works: `apps/api` is served under the same
registrable domain as `apps/web` in production, so the `SameSite=Lax` session
cookie rides along on the WS handshake with no CORS setup. `message_ws_url`
(pure, unit-tested) does the scheme rewrite (`http→ws`, `https→wss`) and leaves
a relative base (`/api`, the production default) relative, so the inline script
prefixes `location.host` and the page works identically behind Caddy and in the
CI stack where the API is on another port.

### Connection lifecycle: bound to the document, because there is no SPA

Open on load of the live view, close when the document goes away. Since
`apps/web` is server-rendered with full-page navigations, "navigate away" and
"switch family" *are* document teardowns — the socket dies with the page and the
next page opens a new one against the new `group_id`. There is no router to hook,
no stale-socket-after-family-switch class of bug to guard against, and a
`pagehide` listener closes the socket explicitly rather than leaving it to the
bfcache. This is a case where the SSR architecture makes the hard part
disappear, so no lifecycle state machine is warranted.

The socket is opened **only on the live view**. A history page (see below) is a
frozen window into the past; pushing live edits into it would be confusing, so
it renders `data-live="false"` and no script.

### Reconnect policy, and never a silent hang

`onclose` fires the same way for a network blip, an API restart, a lost session,
and a membership revoked mid-session — and the browser WebSocket API exposes
**no** handshake status code, so the client cannot tell them apart from the
socket alone. The policy therefore pairs every close with a server question:

1. On close, immediately run one **refresh fetch** of the current URL. That is
   the authoritative probe: if the session is gone the fetch lands on `/login`
   (final URL check), and if membership is gone the re-rendered page carries a
   different (or no) `#thread[data-group-id]`, since `family_context` resolves
   the active family from `GET /groups` on every request.
2. If either check fails → **stop for good** (no further reconnects) and replace
   the live-status line with `"Vous n'avez plus accès à cette conversation."`
   plus a reload link. Never a spinner that hangs, never a socket that retries
   into a 403 loop.
3. Otherwise reconnect with exponential backoff (1 s, 2 s, 4 s, 8 s, 16 s,
   capped 30 s). After 5 consecutive failures it gives up with a persistent
   `"Connexion temps réel interrompue…"` banner telling the user to reload — a
   degraded page that says so beats one that silently stops updating.

This is also the front-side answer to the **30 s revalidation window**
documented at the top of `apps/api/src/messagerie/ws.rs`: a member removed from
the family keeps receiving pushes for up to 30 s (a deliberate backend
perf/consistency trade-off, ratified in the backend epic), then the API drops
the socket on its recheck tick. Step 1 turns that drop into a correct, visible
UI state within one fetch, instead of a thread that quietly stops moving. Reads
and writes are unaffected by the window: every REST call re-runs `require_role`,
so a removed member's next send is `403`'d regardless of the socket.

### Pagination is a windowed history page, not infinite scroll

The API's cursor is `(before_created_at, before_id)` + `limit`, newest first,
`has_more` from `limit + 1`. An accumulating "load more" would need the page to
either re-request every page it has already shown or keep client-side state —
both wrong for an SSR page whose URL must be a complete description of what it
shows. So `Charger les messages plus anciens` is a plain link to
`/messagerie?before_created_at=…&before_id=…` that renders **that** window, with
a `Revenir aux messages récents` link back to the live view. One request, one
URL, back/forward and refresh all behave.

Page size is a URL knob: `?limit=` (clamped 1…100, default 50, mirroring
`DEFAULT_PAGE_LIMIT`/`MAX_PAGE_LIMIT` in `messages.rs`) preserved across the
older-page link. It keeps the front honest about the API contract and lets the
E2E suite exercise `has_more` with three messages instead of fifty-one.

The cursor is built by the pure `older_page_query` from the **oldest message of
the current window**, serialized with microsecond precision
(`to_rfc3339_opts(Micros, true)`) — Postgres `timestamptz` is microsecond-exact,
and a truncated cursor would silently skip or repeat a message.

### Display: chronological, Europe/Paris, author names from the group

The API returns newest-first; the thread renders **oldest at the top, newest at
the bottom**, composer underneath — the chat convention, and the reason
`has_more`'s link sits at the *top* of the list. Timestamps go through the pure
`format_message_time`, which converts the UTC instant to **Europe/Paris** (the
fixed v1 display timezone F3 established, DST included) and formats it
`24/07/2026 à 14:05`.

`MessageResponse` carries only `created_by`, so the page also fetches
`GET /groups/:id` (members with `display_name`, open to any member) and maps
ids to names through the pure `author_name`, which falls back to `"Membre"` for
an author who has since left the family — their messages stay in the thread, so
this is a real state, not a defensive branch. A failed member fetch degrades to
that same fallback rather than taking the page down: names are decoration, the
messages are the content.

Message content is encrypted at rest (`pgp_sym_encrypt`, dedicated
`MESSAGE_ENCRYPTION_KEY`) and decrypted by the API on read — entirely
transparent here. The front receives and posts plain text and never sees a key.

### Out of scope

DMs / multiple threads or channels, attachments and images (text only in v1),
reactions, read receipts and unread counts, typing indicators, message search,
`@`-mentions and push notifications, and any client-side message store. Message
*content* moderation/formatting (markdown, links) is not v1 either — content is
escaped and rendered as plain text with newlines preserved.

## Route table (`apps/web`)

| Method | Path | Handler | Purpose | API call(s) |
|---|---|---|---|---|
| GET | `/messagerie` | `messagerie::thread::get` | The family thread (live view), composer, per-row edit/delete for `can_modify`; `?before_created_at`+`?before_id` render an older window, `?limit` sets the page size | `GET /groups/:id/messages`, `GET /groups/:id` (author names) |
| POST | `/messagerie` | `messagerie::thread::post` | Send a message | `POST /groups/:id/messages` |
| POST | `/messagerie/:id/edit` | `messagerie::thread::edit` | Edit a message (author/admin/owner) | `PATCH /groups/:id/messages/:message_id` |
| POST | `/messagerie/:id/delete` | `messagerie::thread::delete` | Delete a message (author/admin/owner) | `DELETE /groups/:id/messages/:message_id` |
| (browser) | `{API_PUBLIC_BASE_URL}/groups/:id/messages/ws` | — | Push-only live channel, opened by the inline script on the live view | `GET /groups/:id/messages/ws` |

No active family → every route redirects to `/groups/new` (same as Budget /
Grocery list / Stocks / Recipes). Nav link sits between `/budget` and
`/groups` in `app.rs`.

## Error tables (per page)

Backend codes are `apps/api/src/error.rs` bodies (`{"error": "<code>"}`); each
row is the exact `(status, code)` → French UI state.

### `GET /messagerie` — `list_messages` (+ `get_group` for names)

| Status | Code | UI |
|---|---|---|
| 200 | — | thread (oldest→newest) + composer + `Charger les messages plus anciens` when `has_more` |
| 403 | `forbidden` | empty thread, no JSON leak (unreachable once the family is resolved; defensive) |
| — | other status | empty thread, no JSON leak |
| — | transport error | service-unavailable page |
| — | `GET /groups/:id` any failure | thread still renders; every author falls back to `Membre` |

### `POST /messagerie` (send) — `create_message`

| Status | Code | UI |
|---|---|---|
| 201 | — | PRG → `/messagerie?notice=message_sent` |
| 400 | `content_required` | thread re-rendered inline, banner "Le message ne peut pas être vide." (composer keeps its text) |
| 400 | `content_too_long` | thread re-rendered inline, banner "Le message ne peut pas dépasser 4000 caractères." |
| 403 | `forbidden` | forbidden page (unreachable — any member may post; defensive) |
| — | transport / other | thread re-rendered inline, banner "Service momentanément indisponible, merci de réessayer." |

Empty/whitespace-only and over-4000-char content are pre-validated by the shared
`validate_content` (no round trip); the backend 400s are the defensive fallback,
and both surface the same copy.

### `POST /messagerie/:id/edit` — `update_message`

| Status | Code | UI |
|---|---|---|
| 200 | — | PRG → `/messagerie?notice=message_updated` |
| 400 | `content_required` | thread re-rendered inline, the row's edit form re-opened with the submitted text + the banner |
| 400 | `content_too_long` | idem, "Le message ne peut pas dépasser 4000 caractères." |
| 403 | `forbidden` | forbidden page (the row's controls are hidden for non-permitted users; a forged POST lands here) |
| 404 | `not_found` | "Message introuvable" page (e.g. deleted from another session meanwhile) |
| — | transport / other | thread re-rendered inline, "Service momentanément indisponible…" |

### `POST /messagerie/:id/delete` — `delete_message`

| Status | Code | UI |
|---|---|---|
| 204 | — | PRG → `/messagerie?notice=message_deleted` |
| 403 | `forbidden` | forbidden page |
| 404 | `not_found` | "Message introuvable" page |
| — | transport / other | service-unavailable page |

### Live channel (`GET /groups/:id/messages/ws`, browser-side)

| Event | UI |
|---|---|
| `message.created` / `message.updated` / `message.deleted` | coalesced re-render of `#thread` from the server (payload never parsed client-side) |
| close, session still valid | reconnect with backoff (1→30 s), status line cleared on reopen |
| close, refresh lands on `/login` or a different/absent `#thread[data-group-id]` | stop reconnecting, banner "Vous n'avez plus accès à cette conversation." + reload link (covers the ≤30 s membership-revalidation window) |
| 5 consecutive reconnect failures | stop, banner "Connexion temps réel interrompue…" telling the user to reload |
| no JS / no `WebSocket` | nothing; the page is already complete and a reload shows new messages |

## Permission bar (front mirror of `messagerie::can_modify`)

Backend bar (`apps/api/src/messagerie/mod.rs::can_modify`): the message's
**author**, or a group **owner/admin**, may edit or delete it; **any** member may
read the thread, post a message, and open the WS. The front mirrors it
(`manage_our_home_shared::validation::messagerie::can_modify`, unit-tested,
re-exported by `routes/messagerie`):

- The thread, the composer, the `Charger les messages plus anciens` link and the
  live channel render for **every** member.
- A row's `Modifier` disclosure and `Supprimer` button render only when
  `can_modify(role, is_author)` — a standard member sees neither on another
  member's message, while an admin/owner sees both on every message.

The backend stays the authority: a forged edit/delete is still `403`'d and
mapped to the forbidden page per the tables above.

## Acceptance criteria

1. A member opens `/messagerie` and sees the family thread oldest→newest with
   each message's author name and Paris-local timestamp, plus a composer; a sent
   message lands in the thread with a success banner.
2. A message posted from one session appears in a **second session's** thread
   **live**, without that session reloading — the WS push triggers a
   server-rendered re-render of `#thread`. (This is the journey the backend epic
   #7 explicitly deferred to F8.)
3. The author (or an admin/owner) can edit a message inline — the row shows the
   updated text and a `modifié` marker — and delete it, each returning to
   `/messagerie` with a banner.
4. A standard member sees neither `Modifier` nor `Supprimer` on another member's
   message, but can still read the thread and post their own messages.
5. With more messages than the page size, `Charger les messages plus anciens`
   opens the older window (cursor URL), which links back to the live view; the
   live view itself never shows a message twice.
6. Documented error states render their exact French copy: server-side
   empty-content rejection (composer text preserved) and an unknown/deleted
   message id on edit or delete → "Message introuvable".
7. A mid-session auth/membership loss ends in a visible banner and a stopped
   reconnect loop, never a silent hang (the ≤30 s WS revalidation window of
   `apps/api/src/messagerie/ws.rs` resolves into UI state within one refresh
   fetch).
8. With JavaScript disabled, sending, editing and deleting still work
   (plain form posts + PRG); only live updating is lost.
9. The pure `validate_content` / `can_modify` / `format_message_time` /
   `clamp_limit` / `older_page_query` / `message_ws_url` / `author_name` logic is
   unit-tested in `apps/shared` (`validation::messagerie`), written test-first
   per `.claude/CLAUDE.md`; the E2E suite (`e2e/tests/messagerie.spec.ts`) drives
   every journey above end-to-end against the real stack as a CI merge gate.

## Cross-epic note — backend epic #7

`docs/v1-scope.md` row #7 parked one item for this epic: "the spec's E2E journey
(login → open thread → message appears live on a second WS session) is deferred
to front epic F8 (#23) with the rest of the UI-driven Playwright suites". AC #2
and `e2e/tests/messagerie.spec.ts`'s `deux sessions` test are that journey; the
backend is untouched by this epic.
