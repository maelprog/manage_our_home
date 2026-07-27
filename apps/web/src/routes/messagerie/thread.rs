//! `/messagerie` — the family's single chat thread. Renders oldest→newest with
//! a composer underneath; `?before_created_at`+`?before_id` render an older
//! window (history page) instead of the live view. The live view carries the
//! inline WebSocket script (an enhancement — the page is complete without it).
//! Send/edit/delete are plain form posts; a rejected send/edit re-renders the
//! thread inline with the submitted text preserved, success PRGs. See
//! `docs/front-epic-8-messagerie.md`.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::groups::GroupDetailResponse;
use manage_our_home_shared::dto::messagerie::{
    CreateMessageRequest, MessageList, MessageResponse, UpdateMessageRequest,
};
use manage_our_home_shared::validation::messagerie::{
    author_name, clamp_limit, format_message_time, message_ws_url, older_page_query,
    validate_content,
};
use uuid::Uuid;

use crate::app::{html_escape, shell};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{
    can_modify, family_context, forbidden_page, message_not_found_page, messagerie_cookie,
    service_unavailable_page, FamilyContext,
};

/// Query params for `GET /messagerie`. All optional: with none it's the live
/// view; `before_*` make it a history window; `limit` sets the page size.
/// `notice` is the PRG success banner.
#[derive(serde::Deserialize)]
pub struct ThreadQuery {
    notice: Option<String>,
    before_created_at: Option<chrono::DateTime<chrono::Utc>>,
    before_id: Option<Uuid>,
    limit: Option<i64>,
}

fn notice_html(notice: Option<&str>) -> String {
    let text = match notice {
        Some("message_sent") => "Message envoyé.",
        Some("message_updated") => "Message modifié.",
        Some("message_deleted") => "Message supprimé.",
        _ => return String::new(),
    };
    format!(r#"<p class="notice success">{}</p>"#, html_escape(text))
}

/// French copy for a message form error code (send + edit share it).
fn error_message(code: &str) -> &'static str {
    match code {
        "content_required" => "Le message ne peut pas être vide.",
        "content_too_long" => "Le message ne peut pas dépasser 4000 caractères.",
        "unavailable" => "Service momentanément indisponible, merci de réessayer.",
        _ => "Une erreur est survenue, merci de réessayer.",
    }
}

/// Fetches the family's members (for author names). A failure degrades to an
/// empty list — `author_name` then falls back to "Membre" per row, rather than
/// taking the whole page down over decoration.
async fn fetch_members(
    state: &AppState,
    gid: Uuid,
    cookie: Option<&str>,
) -> Vec<manage_our_home_shared::dto::groups::GroupMember> {
    match api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}"),
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<GroupDetailResponse>(resp.body)
                .map(|g| g.members)
                .unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// Renders one message row. `content` is escaped and newlines are preserved
/// (`white-space:pre-wrap`); the edit/delete controls render only for
/// `can_edit`. Editing is inline in a `<details>` disclosure — the backend has
/// no single-message GET, and the row already carries the full content.
fn message_row(
    msg: &MessageResponse,
    members: &[manage_our_home_shared::dto::groups::GroupMember],
    can_edit: bool,
) -> String {
    let author = author_name(members, msg.created_by);
    let time = format_message_time(msg.created_at);
    let edited = if msg.edited_at.is_some() {
        r#" <span class="muted" style="font-size:0.75rem;">(modifié)</span>"#
    } else {
        ""
    };

    let controls = if can_edit {
        format!(
            r#"<div style="display:flex;gap:0.5rem;align-items:flex-start;margin-top:0.3rem;">
<details style="margin:0;">
<summary class="button secondary" style="display:inline-block;cursor:pointer;">Modifier</summary>
<form method="post" action="/messagerie/{id}/edit" style="margin-top:0.4rem;display:flex;flex-direction:column;gap:0.4rem;">
<textarea name="content" required rows="2" maxlength="4000" aria-label="Modifier le message" style="min-width:16rem;">{content}</textarea>
<button type="submit">Enregistrer</button>
</form>
</details>
<form method="post" action="/messagerie/{id}/delete" style="margin:0;">
<button type="submit" class="secondary" style="color:var(--error);">Supprimer</button>
</form>
</div>"#,
            id = msg.id,
            content = html_escape(&msg.content),
        )
    } else {
        String::new()
    };

    format!(
        r#"<li data-message-id="{id}" style="padding:0.6rem 0;border-bottom:1px solid var(--border);">
<div class="muted" style="font-size:0.85rem;"><strong>{author}</strong> — {time}{edited}</div>
<div style="white-space:pre-wrap;margin-top:0.2rem;">{content}</div>
{controls}
</li>"#,
        id = msg.id,
        author = html_escape(author),
        time = html_escape(&time),
        content = html_escape(&msg.content),
    )
}

/// The inline live-updates script. It is the *only* optional part of the page:
/// it opens the push socket, re-renders `#thread` on any frame (coalesced), and
/// turns a mid-session auth/membership loss into a visible banner rather than a
/// silent hang. Bails out immediately if `WebSocket` is unavailable. Rendered
/// only on the live view (`data-live="true"`); a history window renders none.
///
/// The re-render never destroys an inline edit in progress (issue #48): while a
/// row's `<details>` is open the fresh rows are reconciled into the live list by
/// `data-message-id` rather than swapped in wholesale, so another member's
/// message no longer collapses the disclosure and discards the typed text.
///
/// `ws_url` is either absolute (`ws://…`/`wss://…`) or a relative path
/// (`/api/…`, the production default) that the script prefixes with
/// `location.host` and the page's scheme.
fn live_script(ws_url: &str) -> String {
    // The URL is a server-produced string with no user input; embed as a JSON
    // string literal so any stray quote can't break out of the script.
    let ws_url_js = serde_json::to_string(ws_url).unwrap();
    format!(
        r#"<script>
(function() {{
  if (!("WebSocket" in window)) return;
  var thread = document.getElementById("thread");
  if (!thread || thread.getAttribute("data-live") !== "true") return;
  var status = document.getElementById("live-status");
  var raw = {ws_url_js};
  var url = raw;
  if (raw.charAt(0) === "/") {{
    url = (location.protocol === "https:" ? "wss://" : "ws://") + location.host + raw;
  }}

  var attempts = 0, socket = null, coalesce = null, stopped = false, pendingSync = false;

  function setStatus(text) {{ if (status) status.textContent = text; }}

  // The row whose inline edit form is open, if any: the form lives in a native
  // <details> inside the row (see message_row). Its textarea holds text the
  // user typed, which no server render can reproduce — so that row must
  // survive a refresh it didn't ask for (another member posting, a reconnect).
  function editingRow() {{
    var open = thread.querySelector("details[open]");
    return open ? open.closest("li[data-message-id]") : null;
  }}

  // "Has this row's render moved?", asked of the row being edited. Its live
  // <details> is open and its textarea holds unsaved text, so the raw
  // outerHTML never matches the fresh render's closed, server-valued row —
  // compare with the disclosure dropped instead. Everything the server can
  // change (author, time, the (modifié) marker, the content, the delete
  // control) sits outside it.
  function rowSignature(row) {{
    var copy = row.cloneNode(true);
    var forms = copy.querySelectorAll("details");
    for (var j = 0; j < forms.length; j++) forms[j].parentNode.removeChild(forms[j]);
    return copy.outerHTML;
  }}

  // Applies a freshly fetched #thread. With no edit open that is the plain
  // whole-subtree swap. While one is open the fresh rows are reconciled into
  // the live list by data-message-id instead — new, changed and deleted
  // messages still land, the row being edited is left untouched, and its own
  // update — if the fresh render even has one — is replayed once the
  // disclosure closes (pendingSync).
  function applyFresh(fresh) {{
    var editing = editingRow();
    if (!editing) {{ thread.innerHTML = fresh.innerHTML; pendingSync = false; return; }}

    var list = thread.querySelector("ul");
    var freshList = fresh.querySelector("ul");
    // No row list on either side (empty thread): nothing to reconcile against,
    // so hold the swap back entirely rather than dropping the edit.
    if (!list || !freshList) {{ pendingSync = true; return; }}

    var editId = editing.getAttribute("data-message-id");
    var freshRows = freshList.querySelectorAll("li[data-message-id]");
    var live = thread.querySelectorAll("li[data-message-id]");
    var byId = {{}}, i, id, node;
    for (i = 0; i < live.length; i++) byId[live[i].getAttribute("data-message-id")] = live[i];

    var seen = {{}}, prev = null;
    for (i = 0; i < freshRows.length; i++) {{
      id = freshRows[i].getAttribute("data-message-id");
      node = byId[id];
      if (id === editId) {{
        // This row's real render is owed once editing ends — but only if it
        // actually differs (issue #50). The frame that matters most, another
        // member posting, leaves the edited row untouched, and a replay there
        // is a whole fetch + parse that applies nothing.
        if (node && rowSignature(node) !== rowSignature(freshRows[i])) pendingSync = true;
      }} else if (!node) {{
        node = document.importNode(freshRows[i], true);
      }} else if (node.outerHTML !== freshRows[i].outerHTML) {{
        var replacement = document.importNode(freshRows[i], true);
        list.replaceChild(replacement, node);
        node = replacement;
      }}
      if (!node) continue;
      seen[id] = true;
      var ref = prev ? prev.nextSibling : list.firstChild;
      if (ref !== node) list.insertBefore(node, ref); // no-op when already in place
      prev = node;
    }}
    for (i = 0; i < live.length; i++) {{
      id = live[i].getAttribute("data-message-id");
      if (!seen[id] && live[i].parentNode) live[i].parentNode.removeChild(live[i]);
    }}
    // The edited message was deleted by someone else: nothing left to preserve,
    // so take the full render (older-link and empty state included) after all.
    if (!editingRow()) {{ thread.innerHTML = fresh.innerHTML; pendingSync = false; }}
  }}

  // <details>'s toggle event doesn't bubble, so catch it on the way down.
  // Closing the last open editor replays whatever refresh we held back.
  thread.addEventListener("toggle", function() {{
    if (pendingSync && !editingRow()) {{ pendingSync = false; scheduleRerender(); }}
  }}, true);

  // The authoritative probe on any close/error: re-fetch the current page.
  // If the session is gone the fetch lands on /login; if membership is gone
  // the re-rendered #thread carries a different (or no) data-group-id. Either
  // way we stop for good — this is what resolves the API's <=30s membership
  // revalidation window (see apps/api/src/messagerie/ws.rs) into UI state.
  function refresh(then) {{
    fetch(location.href, {{ headers: {{ "X-Requested-With": "fetch" }} }})
      .then(function(r) {{
        if (r.redirected && /\/login(\?|$)/.test(r.url)) {{ accessLost(); return; }}
        return r.text().then(function(html) {{
          var doc = new DOMParser().parseFromString(html, "text/html");
          var fresh = doc.getElementById("thread");
          if (!fresh || fresh.getAttribute("data-group-id") !== thread.getAttribute("data-group-id")) {{
            accessLost(); return;
          }}
          applyFresh(fresh);
          if (then) then();
        }});
      }})
      .catch(function() {{ if (then) then(); }}); // transient: let backoff retry
  }}

  function accessLost() {{
    stopped = true;
    if (socket) {{ try {{ socket.close(); }} catch (e) {{}} }}
    setStatus("Vous n'avez plus accès à cette conversation.");
    var a = document.createElement("a");
    a.href = location.href; a.textContent = " Recharger"; a.className = "button secondary";
    if (status) status.appendChild(a);
  }}

  function scheduleRerender() {{
    if (coalesce) return;
    coalesce = setTimeout(function() {{ coalesce = null; refresh(null); }}, 120);
  }}

  function connect() {{
    if (stopped) return;
    try {{ socket = new WebSocket(url); }} catch (e) {{ scheduleReconnect(); return; }}
    socket.onopen = function() {{ attempts = 0; setStatus(""); }};
    socket.onmessage = function() {{ scheduleRerender(); }}; // payload never parsed
    socket.onclose = function() {{
      if (stopped) return;
      // Probe access first; only reconnect if we still belong here.
      refresh(scheduleReconnect);
    }};
    socket.onerror = function() {{ try {{ socket.close(); }} catch (e) {{}} }};
  }}

  function scheduleReconnect() {{
    if (stopped) return;
    attempts++;
    if (attempts > 5) {{
      setStatus("Connexion temps réel interrompue. Rechargez la page pour réactiver les mises à jour en direct.");
      return;
    }}
    var delay = Math.min(1000 * Math.pow(2, attempts - 1), 30000);
    setTimeout(connect, delay);
  }}

  window.addEventListener("pagehide", function() {{ stopped = true; if (socket) {{ try {{ socket.close(); }} catch (e) {{}} }} }});
  connect();
}})();
</script>"#,
    )
}

/// Renders the full `/messagerie` page. `messages` arrive newest-first from the
/// API and are reversed to render oldest→newest.
fn page(
    fam: &FamilyContext,
    list: &MessageList,
    members: &[manage_our_home_shared::dto::groups::GroupMember],
    query: &ThreadQuery,
    live: bool,
    composer_content: &str,
    composer_error: Option<&str>,
) -> String {
    // Oldest at the top, newest at the bottom (chat convention).
    let ordered: Vec<&MessageResponse> = list.messages.iter().rev().collect();

    let rows = ordered
        .iter()
        .map(|m| {
            message_row(
                m,
                members,
                can_modify(&fam.role, m.created_by == fam.user_id),
            )
        })
        .collect::<String>();

    // "Load older" link, built from the oldest message of this window. Only
    // when the API says there are older messages.
    let older_link = if list.has_more {
        ordered
            .first()
            .map(|oldest| {
                format!(
                    r#"<div style="text-align:center;margin-bottom:0.5rem;"><a class="button secondary" href="/messagerie?{q}">Charger les messages plus anciens</a></div>"#,
                    q = older_page_query(oldest.created_at, oldest.id, query.limit),
                )
            })
            .unwrap_or_default()
    } else {
        String::new()
    };

    let list_html = if ordered.is_empty() {
        r#"<p class="muted">Aucun message pour le moment. Écrivez le premier&nbsp;!</p>"#
            .to_string()
    } else {
        format!(r#"{older_link}<ul style="list-style:none;padding:0;margin:0;">{rows}</ul>"#)
    };

    // A history window (not the live view) shows a way back to the recent view
    // and no composer/live-status; the composer belongs on the live thread.
    let back_link = if live {
        String::new()
    } else {
        r#"<div style="margin-bottom:0.75rem;"><a class="button secondary" href="/messagerie">Revenir aux messages récents</a></div>"#.to_string()
    };

    let composer_error_html = composer_error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();

    let composer = if live {
        format!(
            r#"<form method="post" action="/messagerie" style="margin-top:1rem;display:flex;flex-direction:column;gap:0.5rem;">
{composer_error_html}<textarea name="content" required rows="3" maxlength="4000" aria-label="Votre message" placeholder="Votre message…">{content}</textarea>
<button type="submit">Envoyer</button>
</form>"#,
            content = html_escape(composer_content),
        )
    } else {
        String::new()
    };

    let live_status = if live {
        r#"<p id="live-status" class="muted" style="font-size:0.85rem;min-height:1.2em;margin:0.25rem 0;" aria-live="polite"></p>"#
    } else {
        ""
    };

    let script = if live {
        live_script(&message_ws_url(&fam.api_public_base_url, fam.gid))
    } else {
        String::new()
    };

    let body = format!(
        r#"{header}
<h1 style="margin-bottom:0.25rem;">Messagerie</h1>
<p class="muted" style="margin-top:0;">La conversation de la famille. Un seul fil, partagé par tous les membres.</p>
{notice}
{live_status}
{back_link}
<div id="thread" data-group-id="{gid}" data-live="{live}">
{list_html}
</div>
{composer}
{script}"#,
        header = fam.header,
        notice = notice_html(query.notice.as_deref()),
        gid = fam.gid,
        live = live,
    );
    shell("Messagerie", &body)
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ThreadQuery>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/messagerie").await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = messagerie_cookie(&headers);

    // A window is "live" (opens the socket, shows the composer) unless a cursor
    // is present. A partial cursor (one half only) is treated as no cursor.
    let cursor = query.before_created_at.zip(query.before_id);
    let live = cursor.is_none();
    let limit = clamp_limit(query.limit);

    let mut path = format!("/groups/{}/messages?limit={}", fam.gid, limit);
    if let Some((created_at, id)) = cursor {
        path.push_str(&format!(
            "&before_created_at={}&before_id={}",
            created_at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            id,
        ));
    }

    let list: MessageList = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &path,
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<MessageList>(resp.body).unwrap_or(MessageList {
                messages: Vec::new(),
                has_more: false,
            })
        }
        // A non-member (403, unreachable once family resolved) or any other
        // status: render an empty thread rather than leaking JSON.
        Ok(_) => MessageList {
            messages: Vec::new(),
            has_more: false,
        },
        Err(_) => return service_unavailable_page().into_response(),
    };

    let members = fetch_members(&state, fam.gid, cookie.as_deref()).await;

    Html(page(&fam, &list, &members, &query, live, "", None)).into_response()
}

/// The composer / edit textarea form field.
#[derive(serde::Deserialize)]
pub struct MessageForm {
    #[serde(default)]
    content: String,
}

/// Re-renders the live thread inline (HTTP 200, no PRG) with the composer
/// pre-filled and an error banner — used when a send is rejected, so a long
/// message isn't lost to a round trip. The rejected write never happened, so
/// there is no double-submit for PRG to prevent.
async fn rerender_with_composer_error(
    state: &AppState,
    fam: &FamilyContext,
    cookie: Option<&str>,
    content: &str,
    code: &str,
) -> Response {
    let list: MessageList = match api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{}/messages?limit={}", fam.gid, clamp_limit(None)),
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<MessageList>(resp.body).unwrap_or(MessageList {
                messages: Vec::new(),
                has_more: false,
            })
        }
        _ => MessageList {
            messages: Vec::new(),
            has_more: false,
        },
    };
    let members = fetch_members(state, fam.gid, cookie).await;
    let query = ThreadQuery {
        notice: None,
        before_created_at: None,
        before_id: None,
        limit: None,
    };
    Html(page(
        fam,
        &list,
        &members,
        &query,
        true,
        content,
        Some(error_message(code)),
    ))
    .into_response()
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<MessageForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/messagerie").await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = messagerie_cookie(&headers);

    // Pre-validate (non-empty after trim, <= 4000 chars); the backend 400 is
    // the defensive fallback below.
    let content = match validate_content(&form.content) {
        Ok(c) => c.to_string(),
        Err(e) => {
            return rerender_with_composer_error(
                &state,
                &fam,
                cookie.as_deref(),
                &form.content,
                e.code(),
            )
            .await
        }
    };

    let req = CreateMessageRequest { content };
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/messages", fam.gid),
        cookie.as_deref(),
        Some(serde_json::to_value(&req).unwrap()),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            Redirect::to("/messagerie?notice=message_sent").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            let code = resp
                .body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unavailable")
                .to_string();
            rerender_with_composer_error(&state, &fam, cookie.as_deref(), &form.content, &code)
                .await
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            forbidden_page().into_response()
        }
        Ok(_) | Err(_) => {
            rerender_with_composer_error(
                &state,
                &fam,
                cookie.as_deref(),
                &form.content,
                "unavailable",
            )
            .await
        }
    }
}

pub async fn edit(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(message_id): Path<Uuid>,
    Form(form): Form<MessageForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/messagerie").await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = messagerie_cookie(&headers);

    // A rejected edit re-renders the thread inline (not PRG) so the submitted
    // text survives — same rationale as the composer — with the edited row's
    // disclosure reopened and pre-filled.
    let content = match validate_content(&form.content) {
        Ok(c) => c.to_string(),
        Err(e) => {
            return rerender_with_edit_error(
                &state,
                &fam,
                cookie.as_deref(),
                message_id,
                &form.content,
                e.code(),
            )
            .await
        }
    };

    let req = UpdateMessageRequest { content };
    let result = api_request_auth(
        &state,
        reqwest::Method::PATCH,
        &format!("/groups/{}/messages/{}", fam.gid, message_id),
        cookie.as_deref(),
        Some(serde_json::to_value(&req).unwrap()),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            Redirect::to("/messagerie?notice=message_updated").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            let code = resp
                .body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unavailable")
                .to_string();
            rerender_with_edit_error(
                &state,
                &fam,
                cookie.as_deref(),
                message_id,
                &form.content,
                &code,
            )
            .await
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            forbidden_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            message_not_found_page().into_response()
        }
        Ok(_) | Err(_) => {
            rerender_with_edit_error(
                &state,
                &fam,
                cookie.as_deref(),
                message_id,
                &form.content,
                "unavailable",
            )
            .await
        }
    }
}

/// Re-renders the thread inline with the given message's edit disclosure
/// reopened and pre-filled with the submitted text plus an error banner.
async fn rerender_with_edit_error(
    state: &AppState,
    fam: &FamilyContext,
    cookie: Option<&str>,
    message_id: Uuid,
    submitted: &str,
    code: &str,
) -> Response {
    let list: MessageList = match api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{}/messages?limit={}", fam.gid, clamp_limit(None)),
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<MessageList>(resp.body).unwrap_or(MessageList {
                messages: Vec::new(),
                has_more: false,
            })
        }
        _ => MessageList {
            messages: Vec::new(),
            has_more: false,
        },
    };
    let members = fetch_members(state, fam.gid, cookie).await;
    let ordered: Vec<&MessageResponse> = list.messages.iter().rev().collect();
    let rows = ordered
        .iter()
        .map(|m| {
            if m.id == message_id {
                edit_error_row(m, &members, submitted, error_message(code))
            } else {
                message_row(
                    m,
                    &members,
                    can_modify(&fam.role, m.created_by == fam.user_id),
                )
            }
        })
        .collect::<String>();

    let body = format!(
        r#"{header}
<h1 style="margin-bottom:0.25rem;">Messagerie</h1>
<p class="muted" style="margin-top:0;">La conversation de la famille. Un seul fil, partagé par tous les membres.</p>
<p id="live-status" class="muted" style="font-size:0.85rem;min-height:1.2em;margin:0.25rem 0;" aria-live="polite"></p>
<div id="thread" data-group-id="{gid}" data-live="true">
<ul style="list-style:none;padding:0;margin:0;">{rows}</ul>
</div>
<form method="post" action="/messagerie" style="margin-top:1rem;display:flex;flex-direction:column;gap:0.5rem;">
<textarea name="content" required rows="3" maxlength="4000" aria-label="Votre message" placeholder="Votre message…"></textarea>
<button type="submit">Envoyer</button>
</form>
{script}"#,
        header = fam.header,
        gid = fam.gid,
        script = live_script(&message_ws_url(&fam.api_public_base_url, fam.gid)),
    );
    Html(shell("Messagerie", &body)).into_response()
}

/// A message row whose edit disclosure is forced open, pre-filled with the
/// submitted (rejected) text and carrying the error banner.
fn edit_error_row(
    msg: &MessageResponse,
    members: &[manage_our_home_shared::dto::groups::GroupMember],
    submitted: &str,
    error: &str,
) -> String {
    let author = author_name(members, msg.created_by);
    let time = format_message_time(msg.created_at);
    format!(
        r#"<li data-message-id="{id}" style="padding:0.6rem 0;border-bottom:1px solid var(--border);">
<div class="muted" style="font-size:0.85rem;"><strong>{author}</strong> — {time}</div>
<div style="white-space:pre-wrap;margin-top:0.2rem;">{content}</div>
<div style="margin-top:0.3rem;">
<details open style="margin:0;">
<summary class="button secondary" style="display:inline-block;cursor:pointer;">Modifier</summary>
<form method="post" action="/messagerie/{id}/edit" style="margin-top:0.4rem;display:flex;flex-direction:column;gap:0.4rem;">
<p class="notice error">{error}</p>
<textarea name="content" required rows="2" maxlength="4000" aria-label="Modifier le message" style="min-width:16rem;">{submitted}</textarea>
<button type="submit">Enregistrer</button>
</form>
</details>
</div>
</li>"#,
        id = msg.id,
        author = html_escape(author),
        time = html_escape(&time),
        content = html_escape(&msg.content),
        error = html_escape(error),
        submitted = html_escape(submitted),
    )
}

pub async fn delete(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(message_id): Path<Uuid>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/messagerie").await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = messagerie_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &format!("/groups/{}/messages/{}", fam.gid, message_id),
        cookie.as_deref(),
        None,
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            Redirect::to("/messagerie?notice=message_deleted").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            forbidden_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            message_not_found_page().into_response()
        }
        Ok(_) | Err(_) => service_unavailable_page().into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(content: &str) -> MessageResponse {
        let now = chrono::Utc::now();
        MessageResponse {
            id: Uuid::nil(),
            group_id: Uuid::nil(),
            created_by: Uuid::nil(),
            content: content.to_string(),
            edited_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// The live reconcile matches rows across renders by `data-message-id`;
    /// every row shape has to carry it, or the row being edited can't be found.
    #[test]
    fn every_row_shape_carries_the_reconcile_key() {
        let msg = sample("Bonjour");
        assert!(message_row(&msg, &[], true).contains("data-message-id="));
        assert!(message_row(&msg, &[], false).contains("data-message-id="));
        assert!(edit_error_row(&msg, &[], "Bonjour", "Erreur").contains("data-message-id="));
    }

    /// Issue #48: `refresh()` assigned `thread.innerHTML` directly, so a refresh
    /// triggered by *another* member's message collapsed the open `<details>`
    /// and threw away the text typed into its textarea. Every whole-subtree swap
    /// must now sit inside `applyFresh`, behind its "is someone editing?" guard.
    #[test]
    fn live_script_routes_every_swap_through_the_edit_aware_apply() {
        let js = live_script("/api/groups/x/messages/ws");
        let apply_at = js
            .find("function applyFresh")
            .expect("refresh() must delegate the swap to applyFresh");
        assert!(js.contains("applyFresh(fresh)"));
        assert!(js.contains("thread.innerHTML = fresh.innerHTML"));
        for (at, _) in js.match_indices("thread.innerHTML = fresh.innerHTML") {
            assert!(
                at > apply_at,
                "a bare thread.innerHTML swap escaped applyFresh"
            );
        }
    }

    /// The guard keys off the native disclosure the inline edit form lives in,
    /// and a refresh held back during an edit is replayed when it closes.
    #[test]
    fn live_script_detects_the_open_inline_editor_and_replays_after_it_closes() {
        let js = live_script("/api/groups/x/messages/ws");
        assert!(js.contains("details[open]"));
        assert!(message_row(&sample("Bonjour"), &[], true).contains("<details"));
        // <details>'s toggle event doesn't bubble — the listener has to capture.
        assert!(js.contains(r#"addEventListener("toggle""#));
        assert!(js.contains("li[data-message-id]"));
    }

    /// Issue #50: the held-back replay is only owed when the edited row's own
    /// render actually moved. The flag used to be set on *every* refresh that
    /// landed during an edit — including the common one, another member posting
    /// — so closing the disclosure always cost one more fetch + full-page parse
    /// with nothing to apply.
    #[test]
    fn live_script_owes_the_replay_only_when_the_edited_row_changed() {
        let js = live_script("/api/groups/x/messages/ws");
        let at = js
            .find("if (id === editId) {")
            .expect("the reconcile still has to special-case the row being edited");
        let len = js[at..]
            .find("} else if")
            .expect("the edited-row branch still heads the reconcile's else-if chain");
        let branch = &js[at..at + len];
        assert!(
            branch.contains("pendingSync = true"),
            "the edited row's own update is still owed once editing ends"
        );
        assert!(
            branch.contains("rowSignature(node) !== rowSignature(freshRows[i])"),
            "but only when the fresh render differs: {branch}"
        );
        // The hold-back with no row list on either side is unconditional still:
        // nothing was reconciled at all, so a replay is definitely owed.
        assert!(js.contains("if (!list || !freshList) { pendingSync = true; return; }"));
    }

    /// The edited row's live `<details>` is open and holds text no server render
    /// can reproduce, so a raw `outerHTML` compare differs on every frame and
    /// would make the guard above a no-op. Everything the server can change
    /// (author, time, `(modifié)`, content) sits outside the disclosure.
    #[test]
    fn the_edited_row_comparison_ignores_the_disclosure_subtree() {
        let js = live_script("/api/groups/x/messages/ws");
        let at = js
            .find("function rowSignature(row) {")
            .expect("the edited-row comparison needs a details-free signature");
        let len = js[at..]
            .find("\n  }")
            .expect("rowSignature is a plain function");
        let body = &js[at..at + len];
        assert!(
            body.contains(r#"querySelectorAll("details")"#) && body.contains("removeChild"),
            "the signature has to drop the <details> subtree: {body}"
        );
        // Only the disclosure is dropped: the row's rendered blocks — and the
        // delete control, which vanishes when the row stops being yours — are
        // exactly what the comparison is for.
        let row = message_row(&sample("Bonjour"), &[], true);
        let closed = row
            .find("</details>")
            .expect("the edit form is a disclosure");
        assert!(
            row.find("/delete")
                .expect("rows you may edit carry a delete form")
                > closed,
            "the delete control has to sit outside the <details> the signature drops"
        );
    }
}
