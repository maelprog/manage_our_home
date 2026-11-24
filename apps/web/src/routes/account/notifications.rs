//! `/account/notifications` — how reminders reach the member (#306): by
//! notification on their devices, by email, both or neither; their daily
//! recap of the products nearing their expiry date, on or off, and its
//! hour (#700); whether they are told of the additions, changes and
//! deletions other members make to the agenda (#853); and this device's
//! subscription.
//!
//! Every setting is a switch (`.switch`, #853) or the recap's hour, in one
//! form sent whole as soon as one changes (`data-submit-on-change`, the
//! mechanism of #844): the page comes back with « Préférence
//! enregistrée. » and the focus on that switch (`data-keep-scroll`), the
//! form's fields locked meanwhile (`data-lock-on-change`). « Enregistrer »
//! is the fallback without the script (`app::submit_fallback`).
//!
//! **No fallback** (controller's decision of 2026-10-01): a member on
//! notifications with no device subscribed receives no reminder at all,
//! and no email in its place. They are told so instead, by [`push_block`],
//! wherever a reminder is set — the new-event form, the event's reminder
//! form — and here, with a direct link to the email choice.
//!
//! What the server knows decides the first warning (no device subscribed,
//! or none left once the push services reported them expired). What only
//! the browser knows — the permission refused on *this* device, or no
//! Push API at all — is read by [`PUSH_SCRIPT`], which reveals the
//! matching warning; refused is final on the browser's side (no prompt
//! comes again), so that warning says where its settings are. A permission
//! never asked (`default`) is not a refusal: the script offers a button,
//! and the system prompt opens on that click only.
//!
//! The service worker is [`SERVICE_WORKER`], served at `/sw.js`. A push
//! carries no text (#862: only the ids of its entries, for the click to mark them read): the worker asks [`last_push`] which kind of push its
//! device got and shows that kind's fixed text — « Rappel d'un événement à
//! venir » for a reminder, never the event's title, the same choice as the
//! reminder emails' subject (#146); « Alerte de budget » for a budget alert
//! (#536); « Produits bientôt périmés » for the daily expiry recap (#566),
//! never an item's name; « Agenda de la famille modifié » for a change
//! another member made to the agenda (#851), never the event's title nor
//! its author; a neutral one when it cannot tell.
//!
//! The Android app's WebView has no Push API. There (#668), the reminders
//! are scheduled on the phone by [`LOCAL_REMINDERS_SCRIPT`], from the list
//! [`upcoming`] relays, and show the event's title and time; this page
//! shows their state ([`local_block`]) in place of [`push_block`].

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::{Form, Json};
use manage_our_home_shared::dto::notifications::{
    FcmTokenRequest, NotificationSettings, PushSubscriptionRequest, UpcomingReminder,
    UpdateNotificationSettings,
};

use crate::app::{html_escape, shell_with_header, submit_fallback, Width};
use crate::assets::Script;
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{account_cookie, account_header, service_unavailable_page};

/// Where the warnings send a member who would rather have emails: the
/// email choice of the form below, by its `id`.
pub(crate) const EMAIL_CHOICE_HREF: &str = "/account/notifications#rappels-email";

/// Whether `channel` sends notifications (`push` or `both`).
pub(crate) fn includes_push(channel: &str) -> bool {
    matches!(channel, "push" | "both")
}

/// Whether `channel` sends emails (`email` or `both`).
fn includes_email(channel: &str) -> bool {
    matches!(channel, "email" | "both")
}

/// The channel of the two reminder switches (#853), as the API spells it:
/// both off is `none`, the reminders in the notification centre alone.
fn channel_of(push: bool, email: bool) -> &'static str {
    match (push, email) {
        (true, true) => "both",
        (true, false) => "push",
        (false, true) => "email",
        (false, false) => "none",
    }
}

/// The recap's hour as the form sends it, if it is one of the day's, 0 to
/// 23 — the API checks it too (`invalid_expiry_digest_hour`, #700).
pub(crate) fn parse_digest_hour(text: &str) -> Option<i64> {
    text.parse().ok().filter(|hour| (0..=23).contains(hour))
}

/// The link every warning ends on.
fn email_choice_link() -> String {
    format!(r#"<a href="{EMAIL_CHOICE_HREF}">passer aux rappels par email</a>"#)
}

/// The notification state and controls of this device, for a page where a
/// reminder is set (`always: false`: nothing for a member on email) or for
/// this page (`always: true`: a member on email may still subscribe a
/// device before switching).
///
/// Its warnings are only for a member whose reminders go by notification:
/// `#push-warning`, rendered here when no device is subscribed; and, left
/// `hidden` for [`PUSH_SCRIPT`] to reveal, `#push-blocked` (refused on this
/// device) and `#push-unsupported` (no Push API). Each ends on the email
/// choice. A server without a VAPID key renders its own warning and no
/// control: nothing could subscribe.
pub(crate) fn push_block(settings: &NotificationSettings, always: bool) -> String {
    let warn = includes_push(&settings.reminder_channel);
    if !warn && !always {
        return String::new();
    }
    let email = email_choice_link();
    let Some(key) = settings.vapid_public_key.as_deref() else {
        return if warn {
            format!(
                r#"<div class="notice warning" id="push-warning"><p><strong>Vous ne recevrez pas vos rappels par notification.</strong> Les notifications ne sont pas disponibles sur ce serveur. Vous pouvez {email}.</p></div>"#
            )
        } else {
            r#"<p class="muted">Les notifications ne sont pas disponibles sur ce serveur.</p>"#
                .to_string()
        };
    };

    let mut html = format!(
        r#"<div id="push-device" data-vapid-key="{key}" data-warn="{warn}">"#,
        key = html_escape(key),
    );
    if warn && settings.push_subscriptions == 0 {
        html.push_str(&format!(
            r#"<div class="notice warning" id="push-warning"><p><strong>Vous ne recevrez pas vos rappels par notification.</strong> Les notifications sont désactivées : aucun de vos appareils n'y est abonné. Activez-les sur cet appareil, ou {email}.</p></div>"#
        ));
    }
    if warn {
        html.push_str(&format!(
            r#"<div class="notice warning" id="push-blocked" hidden><p><strong>Les notifications sont bloquées sur cet appareil</strong> : votre navigateur les refuse pour ce site, et ne vous le redemandera pas. Pour les réactiver, ouvrez les réglages du site dans votre navigateur (l'icône à gauche de l'adresse, ou Réglages › Notifications sur téléphone), autorisez les notifications, puis rechargez la page. Ou {email}.</p></div>
<div class="notice warning" id="push-unsupported" hidden><p><strong>Ce navigateur ne peut pas recevoir de notifications.</strong> Sur iPhone et iPad, ajoutez d'abord le site à l'écran d'accueil et ouvrez-le depuis là. Ou {email}.</p></div>"#
        ));
    }
    html.push_str(
        r#"<p id="push-enable" hidden><button type="button" class="secondary" id="push-enable-button">Activer les notifications sur cet appareil</button></p>
<p class="notice success" id="push-enabled" hidden>Notifications activées sur cet appareil.</p>
<p class="notice error" id="push-failed" hidden>L'activation a échoué, merci de réessayer.</p>
</div>"#,
    );
    html.push_str(&Script::Push.tag());
    html
}

/// The script behind [`push_block`], served under `/assets`
/// (`assets::Script::Push`, #325). Progressive enhancement — without it
/// the server-side warning still says what matters.
///
/// - No Push API: reveals `#push-unsupported` (when warnings apply).
/// - Permission refused: reveals `#push-blocked` (when warnings apply).
/// - Permission granted: makes sure this device is subscribed and its
///   endpoint and keys stored (`POST /account/notifications/subscription`), with no
///   prompt, and hides `#push-warning` once it is.
/// - Never asked: reveals the button; the browser's prompt opens on its
///   click only. A prompt dismissed without an answer leaves the
///   permission `default`, and the button stays; only a refusal hides it.
pub(crate) const PUSH_SCRIPT: &str = r#"
(function () {
  var box = document.getElementById("push-device");
  if (!box) return;
  var key = box.getAttribute("data-vapid-key");
  var warn = box.getAttribute("data-warn") === "true";
  function el(id) { return document.getElementById(id); }
  function show(id) { var e = el(id); if (e) e.hidden = false; }
  function hide(id) { var e = el(id); if (e) e.hidden = true; }
  if (!key) return;
  if (!("serviceWorker" in navigator) || !("PushManager" in window) || !("Notification" in window)) {
    if (warn) show("push-unsupported");
    return;
  }
  if (Notification.permission === "denied") {
    if (warn) show("push-blocked");
    return;
  }
  function bytes(b64) {
    var s = b64.replace(/-/g, "+").replace(/_/g, "/");
    while (s.length % 4) s += "=";
    var raw = atob(s), out = new Uint8Array(raw.length);
    for (var i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
    return out;
  }
  // Whether `sub` was made with this server's current key. After a key
  // rotation the browser still holds the old subscription, which every
  // push service now refuses. A browser that does not expose the key
  // (`options` missing) is trusted, so as not to resubscribe on each visit.
  function sameKey(sub) {
    if (!sub.options || !("applicationServerKey" in sub.options)) return true;
    var held = sub.options.applicationServerKey;
    if (!held) return false;
    var a = new Uint8Array(held), b = bytes(key);
    if (a.length !== b.length) return false;
    for (var i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
    return true;
  }
  function subscribe() {
    return navigator.serviceWorker.register("/sw.js")
      .then(function () { return navigator.serviceWorker.ready; })
      .then(function (reg) {
        return reg.pushManager.getSubscription().then(function (sub) {
          if (sub && sameKey(sub)) return sub;
          // Made with another key: drop it, then subscribe again.
          var dropped = sub ? sub.unsubscribe() : Promise.resolve();
          return dropped.then(function () {
            return reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: bytes(key) });
          });
        });
      })
      .then(function (sub) {
        return fetch("/account/notifications/subscription", {
          method: "POST",
          credentials: "same-origin",
          headers: { "Content-Type": "application/json" },
          // The keys (#862): a push's payload is encrypted with them.
          body: JSON.stringify({ endpoint: sub.endpoint, keys: sub.toJSON().keys })
        });
      })
      .then(function (res) {
        if (!res.ok) throw new Error("HTTP " + res.status);
        hide("push-warning");
        hide("push-enable");
      });
  }
  if (Notification.permission === "granted") {
    subscribe().catch(function () { show("push-enable"); });
  } else {
    show("push-enable");
  }
  var button = el("push-enable-button");
  if (!button) return;
  button.addEventListener("click", function () {
    button.disabled = true;
    hide("push-failed");
    Notification.requestPermission().then(function (permission) {
      if (permission === "granted") {
        return subscribe().then(function () { show("push-enabled"); });
      }
      // Refused: final, the button could only fail. Prompt dismissed
      // ("default"): nothing was decided, the button stays.
      if (permission === "denied") {
        hide("push-enable");
        if (warn) show("push-blocked");
      }
    }).catch(function () {
      show("push-failed");
    }).then(function () { button.disabled = false; });
  });
})();
"#;

/// The script of the Android app's local reminders (#668), served under
/// `/assets` (`assets::Script::LocalReminders`) and loaded by
/// `app::document` in the app's pages only. It schedules the member's
/// next reminders on the phone ([`upcoming`]) and drives [`local_block`].
pub(crate) const LOCAL_REMINDERS_SCRIPT: &str = include_str!("../../local-reminders.js");

/// In the app, what stands in place of [`push_block`] (#668): the state of
/// the reminders scheduled on this phone. Every state waits `hidden` for
/// [`LOCAL_REMINDERS_SCRIPT`], which alone can read the plugin:
/// `#local-unsupported` (an APK without it), `#local-granted` and
/// `#local-count` (the number scheduled), `#local-ask` (permission never
/// given: a button, the system prompt opens on its click only),
/// `#local-denied` (refused: Android's settings), `#local-exact` (exact
/// alarms refused: why they help, and a button to Android's « Alarmes et
/// rappels »; reminders go on inexact meanwhile), `#local-failed`.
pub(crate) fn local_block(settings: &NotificationSettings) -> String {
    let email = email_choice_link();
    let on_email = if includes_push(&settings.reminder_channel) {
        ""
    } else if includes_email(&settings.reminder_channel) {
        r#"<p class="muted">Vos rappels partent par email : aucun n'est programmé sur ce téléphone.</p>
"#
    } else {
        // Both switches off (#853).
        r#"<p class="muted">Vos rappels ne restent que dans vos notifications : aucun n'est programmé sur ce téléphone.</p>
"#
    };
    format!(
        r#"<div id="local-reminders">
{on_email}<p class="notice warning" id="local-unsupported" hidden>Cette version de l'application ne programme pas les rappels : installez la dernière version.</p>
<p id="local-granted" hidden>Notifications autorisées sur ce téléphone.</p>
<p id="local-count" hidden></p>
<p id="local-ask" hidden><button type="button" class="secondary" id="local-ask-button">Autoriser les notifications sur ce téléphone</button></p>
<div class="notice warning" id="local-denied" hidden><p><strong>Les notifications sont bloquées sur ce téléphone</strong> : aucun rappel n'y est programmé. Pour les réactiver, ouvrez Réglages › Applications, choisissez l'application, puis Notifications. Ou {email}.</p></div>
<div id="local-exact" hidden><p>Android peut retarder un rappel de quelques minutes pour économiser la batterie. Pour qu'il arrive à l'heure exacte, autorisez l'application dans « Alarmes et rappels ». Sans cela, les rappels arrivent quand même, avec ce léger retard possible.</p>
<p><button type="button" class="secondary" id="local-exact-button">Autoriser les rappels à l'heure exacte</button></p></div>
<p class="notice error" id="local-failed" hidden>Les rappels de ce téléphone n'ont pas pu être mis à jour : ils le seront à la prochaine ouverture de l'application.</p>
</div>"#
    )
}

/// What [`upcoming`] answers for the API's: the list as it came, `200`,
/// when it is one; `401` when the API says the caller has no session
/// (logged out, account deactivated or deleted), on which the script
/// cancels every reminder; `502` for anything else, on which it keeps
/// them. Never cached.
pub(crate) fn upcoming_answer(result: Result<crate::state::ApiResponse, String>) -> Response {
    let no_store = [(header::CACHE_CONTROL, "no-store")];
    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match serde_json::from_value::<Vec<UpcomingReminder>>(resp.body) {
                Ok(list) => (no_store, Json(list)).into_response(),
                Err(_) => (no_store, StatusCode::BAD_GATEWAY).into_response(),
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNAUTHORIZED => {
            (no_store, StatusCode::UNAUTHORIZED).into_response()
        }
        _ => (no_store, StatusCode::BAD_GATEWAY).into_response(),
    }
}

/// `GET /account/notifications/upcoming` — [`LOCAL_REMINDERS_SCRIPT`]'s
/// call, relayed to apps/api's `GET /account/reminders/upcoming` (#667).
/// No `CurrentUser`: its redirect to `/login` would read as a page, not
/// as the 401 the script cancels the reminders on; the API's session
/// check decides ([`upcoming_answer`]).
pub async fn upcoming(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let cookie = account_cookie(&headers);
    upcoming_answer(
        api_request_auth(
            &state,
            reqwest::Method::GET,
            "/account/reminders/upcoming",
            cookie.as_deref(),
            None,
        )
        .await,
    )
}

/// The kinds of notification the API pushes to the devices: an agenda
/// reminder come due (#306), a budget alert (#536), the daily recap of
/// the stock items nearing their expiry date (#566) and a change another
/// member made to the agenda (#851). The service worker shows a text of
/// its own for each (`sw.js`).
pub(crate) const PUSHED_KINDS: [&str; 4] = [
    "agenda_reminder",
    "budget_alert",
    "stock_expiry",
    "agenda_change",
];

/// Of the API's answer on a push received, the kind to show if it is one
/// of [`PUSHED_KINDS`]; `None`, the neutral text, for anything else.
pub(crate) fn pushed_kind(answer: &serde_json::Value) -> Option<&'static str> {
    let kind = answer.get("kind")?.as_str()?;
    PUSHED_KINDS.into_iter().find(|pushed| *pushed == kind)
}

/// The header the service worker names its device in: the endpoint of its
/// push subscription, which the API knows it by.
const PUSH_ENDPOINT_HEADER: &str = "x-push-endpoint";

/// `POST /account/notifications/last-push`, `{"kind": …}`: what the service
/// worker asks on each push, which carries no text, to choose its text.
/// It names its device ([`PUSH_ENDPOINT_HEADER`]); the API answers the
/// kind of the push this device got (`POST
/// /account/push-subscriptions/received`): `null` when pushes of two kinds
/// are on their way to it, when none is, without a device named, or on any
/// failure. The worker shows a neutral text then. A `POST`, never a `GET`
/// (#783): the answer takes the push off the device's count, and a `GET`
/// of apps/web writes nothing (#414). The rule, not a hole: the write
/// needed [`PUSH_ENDPOINT_HEADER`], which a cross-site navigation does not
/// send.
pub async fn last_push(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let cookie = account_cookie(&headers);
    let endpoint = headers
        .get(PUSH_ENDPOINT_HEADER)
        .and_then(|value| value.to_str().ok());
    let mut kind = None;
    if let Some(endpoint) = endpoint {
        let body = serde_json::to_value(PushSubscriptionRequest {
            endpoint: endpoint.to_string(),
            keys: None,
        })
        .ok();
        kind = api_request_auth(
            &state,
            reqwest::Method::POST,
            "/account/push-subscriptions/received",
            cookie.as_deref(),
            body,
        )
        .await
        .ok()
        .filter(|resp| resp.status == reqwest::StatusCode::OK)
        .and_then(|resp| pushed_kind(&resp.body));
    }
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({ "kind": kind })),
    )
        .into_response()
}

/// The service worker, served at `/sw.js` out of the binary.
pub(crate) const SERVICE_WORKER: &str = include_str!("../../sw.js");

/// `GET /sw.js`. At the root, so that its scope is the whole site; never
/// cached by HTTP, so that a deploy's worker is the one the browser checks
/// for updates.
pub async fn service_worker() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        SERVICE_WORKER,
    )
        .into_response()
}

/// `GET /account/notifications` from apps/api, `None` on any failure: the
/// reminder forms then render without the block rather than fail.
pub(crate) async fn fetch_settings(
    state: &AppState,
    cookie: Option<&str>,
) -> Option<NotificationSettings> {
    let resp = api_request_auth(
        state,
        reqwest::Method::GET,
        "/account/notifications",
        cookie,
        None,
    )
    .await
    .ok()?;
    if resp.status != reqwest::StatusCode::OK {
        return None;
    }
    serde_json::from_value(resp.body).ok()
}

#[derive(serde::Deserialize)]
pub struct PageQuery {
    notice: Option<String>,
    error: Option<String>,
}

fn notice_html(notice: Option<&str>) -> String {
    let text = match notice {
        Some("channel_saved") => "Préférence enregistrée.",
        Some("devices_removed") => {
            "Vos appareils sont désabonnés : plus aucune notification ne leur sera envoyée."
        }
        _ => return String::new(),
    };
    format!(
        r#"<p class="notice success" role="status">{}</p>"#,
        html_escape(text)
    )
}

fn error_html(error: Option<&str>) -> String {
    let text = match error {
        Some("invalid_hour") => "Heure invalide : choisissez une heure entre 0 h et 23 h.",
        Some("unavailable") => "Service momentanément indisponible, merci de réessayer.",
        // A switch sent on change (#853): the page comes back from the
        // server, the switch as it was saved.
        Some("not_saved") => {
            "Le réglage n’a pas pu être enregistré : il est revenu à son état précédent. Réessayez."
        }
        _ => return String::new(),
    };
    format!(
        r#"<p class="notice error" role="alert">{}</p>"#,
        html_escape(text)
    )
}

/// One setting (#853): a native box with `role="switch"`, drawn as a
/// switch by the sheet (`.switch`), at the left of its words on one row —
/// the row is its label. Sent as soon as it changes.
fn switch(name: &str, words: &str, on: bool, id: &str) -> String {
    let checked = if on { " checked" } else { "" };
    format!(
        r#"<label class="switch"><input type="checkbox" role="switch" name="{name}"{id}{checked} data-submit-on-change><span>{words}</span></label>
"#
    )
}

/// The body of the page.
fn page_body(settings: &NotificationSettings, notice: &str, error: &str) -> String {
    let channel = settings.reminder_channel.as_str();
    let reminders = format!(
        "{}{}",
        switch(
            "reminder_push",
            "Par notification sur mes appareils",
            includes_push(channel),
            ""
        ),
        switch(
            "reminder_email",
            "Par email",
            includes_email(channel),
            r#" id="rappels-email""#
        ),
    );
    let agenda = format!(
        "{}{}{}",
        switch(
            "notify_agenda_added",
            "Ajouts",
            settings.notify_agenda_added,
            ""
        ),
        switch(
            "notify_agenda_changed",
            "Modifications",
            settings.notify_agenda_changed,
            ""
        ),
        switch(
            "notify_agenda_deleted",
            "Suppressions",
            settings.notify_agenda_deleted,
            ""
        ),
    );
    let recap = switch(
        "expiry_digest",
        "Récapitulatif quotidien",
        settings.expiry_digest_enabled,
        "",
    );
    // The recap off, its hour is kept, and may be changed: the hour it
    // comes back at when switched on again.
    let hours: String = (0..24)
        .map(|hour| {
            let selected = if hour == settings.expiry_digest_hour {
                " selected"
            } else {
                ""
            };
            format!(r#"<option value="{hour}"{selected}>{hour} h</option>"#)
        })
        .collect();
    let n = settings.push_subscriptions;
    let devices = match n {
        0 => "Aucun appareil n'est abonné aux notifications.".to_string(),
        1 => "1 appareil est abonné aux notifications.".to_string(),
        n => format!("{n} appareils sont abonnés aux notifications."),
    };
    let remove = if n > 0 {
        r#"<form method="post" action="/account/notifications/devices/remove">
<button type="submit" class="secondary">Désabonner tous mes appareils</button>
</form>"#
    } else {
        ""
    };
    // In the app (#668), the reminders are scheduled on the phone, with
    // the event's title: what the browser's text says of the notifications
    // pushed to a device is not true of them.
    let (about, block) = if crate::mobile_app::current() {
        (
            r#"<p>Les rappels de vos événements vous parviennent par notification, par email, ou les deux. Dans l'application, les rappels par notification sont programmés sur ce téléphone et arrivent même sans réseau : la notification affiche le titre de l'événement et son heure, qu'Android garde sur le téléphone jusqu'au rappel.</p>
<p class="muted">Les rappels sont mis à jour à chaque ouverture de l'application. Un événement modifié pendant que le téléphone est hors ligne garde son rappel à l'ancienne heure jusque-là.</p>"#,
            local_block(settings),
        )
    } else {
        (
            r#"<p>Les rappels de vos événements vous parviennent par notification sur les appareils où vous les avez activées, par email, ou les deux. Une notification n'affiche jamais le titre de l'événement, seulement « Rappel d'un événement à venir », ou « Nouvelle notification » quand l'appareil ne peut pas savoir laquelle il a reçue : le titre se lit dans l'agenda.</p>
<p class="muted">Sans appareil abonné, un rappel par notification n'est pas envoyé du tout, et aucun email ne le remplace.</p>"#,
            push_block(settings, true),
        )
    };
    format!(
        r#"<p><a href="/account">← Retour à mon compte</a></p>
<h1>Notifications de rappel</h1>
{notice}{error}
{about}
<p class="muted" data-shown-with-js hidden>Chaque réglage s’applique dès que vous le changez.</p>
<form method="post" action="/account/notifications" data-keep-scroll data-lock-on-change>
<fieldset class="card">
<legend>Recevoir mes rappels</legend>
{reminders}</fieldset>
<fieldset class="card">
<legend>Produits bientôt périmés</legend>
{recap}<p>Chaque jour, à l'heure choisie (heure de Paris), un récapitulatif des produits de vos familles qui périment bientôt arrive dans vos notifications.</p>
<label class="field">Heure du récapitulatif <select name="expiry_digest_hour" data-submit-on-change>{hours}</select></label>
</fieldset>
<fieldset class="card">
<legend>Changements de l’agenda</legend>
{agenda}</fieldset>
{fallback}
</form>
<section class="card">
<h2>Mes appareils</h2>
{block}
<p>{devices}</p>
{remove}
</section>"#,
        fallback = submit_fallback("Enregistrer"),
    )
}

/// `GET /account/notifications`.
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Response {
    let cookie = account_cookie(&headers);
    let Some(settings) = fetch_settings(&state, cookie.as_deref()).await else {
        return service_unavailable_page().into_response();
    };
    let header = account_header(&state, &headers, &me, "/account/notifications").await;
    let body = page_body(
        &settings,
        &notice_html(query.notice.as_deref()),
        &error_html(query.error.as_deref()),
    );
    Html(shell_with_header(
        Width::Read,
        "Notifications de rappel",
        &header,
        &body,
    ))
    .into_response()
}

/// The settings' form (#853): a switch ticked is sent as `name=on`, a
/// switch off is not sent at all — every switch is then known, on or off.
#[derive(Debug, Default, serde::Deserialize)]
pub struct SettingsForm {
    reminder_push: Option<String>,
    reminder_email: Option<String>,
    expiry_digest: Option<String>,
    expiry_digest_hour: Option<String>,
    notify_agenda_added: Option<String>,
    notify_agenda_changed: Option<String>,
    notify_agenda_deleted: Option<String>,
}

/// What the form asks of the API: the whole of it, each switch on or off,
/// and the recap's hour, kept and saved while the recap is off. `Err`
/// holds the error of the page: an hour sent that is not one of the day's.
fn settings_update(form: &SettingsForm) -> Result<UpdateNotificationSettings, &'static str> {
    let expiry_digest_hour = form
        .expiry_digest_hour
        .as_deref()
        .map(|text| parse_digest_hour(text).ok_or("invalid_hour"))
        .transpose()?;
    Ok(UpdateNotificationSettings {
        reminder_channel: channel_of(form.reminder_push.is_some(), form.reminder_email.is_some())
            .to_string(),
        expiry_digest_hour,
        expiry_digest_enabled: Some(form.expiry_digest.is_some()),
        notify_agenda_added: Some(form.notify_agenda_added.is_some()),
        notify_agenda_changed: Some(form.notify_agenda_changed.is_some()),
        notify_agenda_deleted: Some(form.notify_agenda_deleted.is_some()),
    })
}

/// `POST /account/notifications` — the whole form, sent on each change of
/// a switch or of the hour (or by « Enregistrer » without JavaScript).
pub async fn post(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<SettingsForm>,
) -> Response {
    let update = match settings_update(&form) {
        Ok(update) => update,
        Err(code) => {
            return Redirect::to(&format!("/account/notifications?error={code}")).into_response()
        }
    };
    let cookie = account_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::PUT,
        "/account/notifications",
        cookie.as_deref(),
        Some(serde_json::to_value(update).unwrap()),
    )
    .await;
    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            "/account/notifications?notice=channel_saved"
        }
        _ => "/account/notifications?error=not_saved",
    };
    Redirect::to(target).into_response()
}

/// `POST /account/notifications/devices/remove` — unsubscribes every
/// device of the member.
pub async fn remove_devices(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let cookie = account_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        "/account/push-subscriptions",
        cookie.as_deref(),
        None,
    )
    .await;
    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            "/account/notifications?notice=devices_removed"
        }
        _ => "/account/notifications?error=unavailable",
    };
    Redirect::to(target).into_response()
}

/// `POST /account/notifications/subscription` — [`PUSH_SCRIPT`]'s call,
/// relayed to `POST /account/push-subscriptions`: 204 once stored, the
/// API's status otherwise (400 for an endpoint that is no push service's,
/// 409 on a server without a key), 502 when the API cannot be reached.
/// JSON only: a cross-site form cannot send that content type without a
/// CORS preflight, which nothing here answers.
pub async fn subscription(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PushSubscriptionRequest>,
) -> Response {
    let cookie = account_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        "/account/push-subscriptions",
        cookie.as_deref(),
        Some(serde_json::to_value(body).unwrap()),
    )
    .await;
    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(resp) => StatusCode::from_u16(resp.status.as_u16())
            .unwrap_or(StatusCode::BAD_GATEWAY)
            .into_response(),
        Err(_) => StatusCode::BAD_GATEWAY.into_response(),
    }
}

/// `POST` and `DELETE /account/notifications/fcm-token`, `{"token": …}` —
/// [`LOCAL_REMINDERS_SCRIPT`]'s calls (#1004), relayed to apps/api's
/// `/account/fcm-tokens` (#1002): the Android app's push token, registered
/// on each page (which moves the device's `last_seen_at`), forgotten as
/// the member logs out. 204 once done, the API's status otherwise (401
/// without a session, 400 for what cannot be a token, 409 on a server
/// without FCM), 502 when the API cannot be reached. No `CurrentUser`, as
/// [`upcoming`]: the API's session check decides, and a redirect to
/// `/login` would read as a success. JSON only, as [`subscription`].
pub async fn fcm_token(
    State(state): State<AppState>,
    method: axum::http::Method,
    headers: HeaderMap,
    Json(body): Json<FcmTokenRequest>,
) -> Response {
    let cookie = account_cookie(&headers);
    let (relayed, done) = if method == axum::http::Method::DELETE {
        (reqwest::Method::DELETE, reqwest::StatusCode::NO_CONTENT)
    } else {
        (reqwest::Method::POST, reqwest::StatusCode::CREATED)
    };
    let result = api_request_auth(
        &state,
        relayed,
        "/account/fcm-tokens",
        cookie.as_deref(),
        Some(serde_json::to_value(body).unwrap()),
    )
    .await;
    let no_store = [(header::CACHE_CONTROL, "no-store")];
    match result {
        Ok(resp) if resp.status == done => (no_store, StatusCode::NO_CONTENT).into_response(),
        Ok(resp) => (
            no_store,
            StatusCode::from_u16(resp.status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
        )
            .into_response(),
        Err(_) => (no_store, StatusCode::BAD_GATEWAY).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(channel: &str, devices: i64) -> NotificationSettings {
        NotificationSettings {
            reminder_channel: channel.into(),
            push_subscriptions: devices,
            vapid_public_key: Some("BKey-_0".into()),
            expiry_digest_enabled: true,
            expiry_digest_hour: 9,
            notify_agenda_added: true,
            notify_agenda_changed: true,
            notify_agenda_deleted: true,
        }
    }

    // -- the switches (#853) ----------------------------------------------

    /// The form as the browser sends it: a box ticked is `name=on`, a box
    /// left off is not sent at all.
    fn form(on: &[&str], hour: Option<&str>) -> SettingsForm {
        let mut pairs: Vec<String> = on.iter().map(|name| format!("{name}=on")).collect();
        if let Some(hour) = hour {
            pairs.push(format!("expiry_digest_hour={hour}"));
        }
        serde_urlencoded::from_str(&pairs.join("&")).unwrap()
    }

    #[test]
    fn the_two_reminder_switches_are_one_channel() {
        assert_eq!(channel_of(true, false), "push");
        assert_eq!(channel_of(false, true), "email");
        assert_eq!(channel_of(true, true), "both");
        assert_eq!(channel_of(false, false), "none");
        for (push, email) in [(true, false), (false, true), (true, true), (false, false)] {
            let channel = channel_of(push, email);
            assert_eq!(
                (includes_push(channel), includes_email(channel)),
                (push, email),
                "{channel}"
            );
        }
    }

    /// The whole form goes to the API on every change: every switch, on or
    /// off, and the recap's hour — kept, and changed, while the recap is
    /// off.
    #[test]
    fn the_whole_form_is_one_update() {
        let all = form(
            &[
                "reminder_push",
                "reminder_email",
                "expiry_digest",
                "notify_agenda_added",
                "notify_agenda_changed",
                "notify_agenda_deleted",
            ],
            Some("21"),
        );
        let update = settings_update(&all).unwrap();
        assert_eq!(update.reminder_channel, "both");
        assert_eq!(
            (update.expiry_digest_enabled, update.expiry_digest_hour),
            (Some(true), Some(21))
        );
        assert_eq!(
            (
                update.notify_agenda_added,
                update.notify_agenda_changed,
                update.notify_agenda_deleted
            ),
            (Some(true), Some(true), Some(true))
        );

        let none = settings_update(&form(&[], Some("18"))).unwrap();
        assert_eq!(none.reminder_channel, "none");
        assert_eq!(
            (none.expiry_digest_enabled, none.expiry_digest_hour),
            (Some(false), Some(18))
        );
        assert_eq!(
            (
                none.notify_agenda_added,
                none.notify_agenda_changed,
                none.notify_agenda_deleted
            ),
            (Some(false), Some(false), Some(false))
        );

        let some =
            settings_update(&form(&["reminder_email", "notify_agenda_changed"], None)).unwrap();
        assert_eq!(some.reminder_channel, "email");
        assert_eq!(
            (some.expiry_digest_enabled, some.expiry_digest_hour),
            (Some(false), None)
        );
        assert_eq!(
            (
                some.notify_agenda_added,
                some.notify_agenda_changed,
                some.notify_agenda_deleted
            ),
            (Some(false), Some(true), Some(false))
        );
    }

    /// An hour sent, recap on or off, must be one of the day's; an hour
    /// left out is left as it is.
    #[test]
    fn an_hour_sent_is_one_of_the_day_s() {
        for on in [&["expiry_digest"][..], &[]] {
            for hour in ["24", "", "neuf", "-1"] {
                assert_eq!(
                    settings_update(&form(on, Some(hour))).unwrap_err(),
                    "invalid_hour",
                    "{on:?} {hour:?}"
                );
            }
            let update = |hour| settings_update(&form(on, hour)).unwrap().expiry_digest_hour;
            assert_eq!(update(Some("0")), Some(0), "{on:?}");
            assert_eq!(update(None), None, "{on:?}");
        }
    }

    /// The input of the switch named `name`, up to its `>`.
    fn switch_input<'a>(body: &'a str, name: &str) -> &'a str {
        let at = body
            .find(&format!(r#"name="{name}""#))
            .unwrap_or_else(|| panic!("{name}: {body}"));
        let start = body[..at].rfind("<input").unwrap();
        &body[start..at + body[at..].find('>').unwrap()]
    }

    /// Every setting is a switch at the left of its words, on one row: a
    /// native box with `role="switch"`, ticked when on, sent as soon as it
    /// changes. Labelled by the row it sits in.
    #[test]
    fn every_setting_is_a_switch_sent_on_change() {
        let body = page_body(&settings("push", 0), "", "");
        for (name, words) in [
            ("reminder_push", "Par notification sur mes appareils"),
            ("reminder_email", "Par email"),
            ("expiry_digest", "Récapitulatif quotidien"),
            ("notify_agenda_added", "Ajouts"),
            ("notify_agenda_changed", "Modifications"),
            ("notify_agenda_deleted", "Suppressions"),
        ] {
            let input = switch_input(&body, name);
            assert!(
                input.contains(r#"type="checkbox" role="switch""#),
                "{input}"
            );
            assert!(input.contains(" data-submit-on-change"), "{input}");
            assert!(
                body.contains(&format!(r#"{input}><span>{words}</span></label>"#)),
                "{name}: {body}"
            );
            let row = &body[..body.find(input).unwrap()];
            assert!(row.ends_with(r#"<label class="switch">"#), "{name}");
        }
        // The radios are gone, and their « Les deux ».
        assert!(!body.contains(r#"type="radio""#), "{body}");
        assert!(!body.contains("Les deux"), "{body}");
        // The email switch is where the warnings send.
        assert!(switch_input(&body, "reminder_email").contains(r#"id="rappels-email""#));
        // The hour is sent on change too; the form keeps the scroll, and
        // its fields wait locked for the page to come back.
        let select = &body[body.find(r#"<select name="expiry_digest_hour""#).unwrap()..];
        assert!(
            select.starts_with(r#"<select name="expiry_digest_hour" data-submit-on-change>"#),
            "{select}"
        );
        assert!(
            body.contains(r#"<form method="post" action="/account/notifications" data-keep-scroll data-lock-on-change>"#),
            "{body}"
        );
    }

    #[test]
    fn each_switch_is_on_as_the_settings_say() {
        let checked = |s: &NotificationSettings, name: &str| {
            switch_input(&page_body(s, "", ""), name).contains(" checked")
        };
        let mut s = settings("push", 0);
        assert!(checked(&s, "reminder_push"));
        assert!(!checked(&s, "reminder_email"));
        assert!(checked(&s, "expiry_digest"));
        s = settings("both", 0);
        assert!(checked(&s, "reminder_push") && checked(&s, "reminder_email"));
        s = settings("none", 0);
        assert!(!checked(&s, "reminder_push") && !checked(&s, "reminder_email"));
        s.expiry_digest_enabled = false;
        s.expiry_digest_hour = 18;
        s.notify_agenda_changed = false;
        assert!(!checked(&s, "expiry_digest"));
        assert!(checked(&s, "notify_agenda_added"));
        assert!(!checked(&s, "notify_agenda_changed"));
        assert!(checked(&s, "notify_agenda_deleted"));
        // The recap off, the hour shown is the member's, and may be changed.
        let body = page_body(&s, "", "");
        let (_, selected) = hour_options(&body);
        assert_eq!(selected, ["18"]);
        assert!(
            body.contains(r#"<select name="expiry_digest_hour" data-submit-on-change>"#),
            "{body}"
        );
    }

    /// No « Enregistrer » with JavaScript (#853): a sentence says each
    /// setting applies as it changes; the button and its sentence are the
    /// fallback without the script.
    #[test]
    fn the_button_is_the_fallback_without_the_script() {
        let body = page_body(&settings("push", 0), "", "");
        assert!(
            body.contains(
                r#"<p class="muted" data-shown-with-js hidden>Chaque réglage s’applique dès que vous le changez.</p>"#
            ),
            "{body}"
        );
        let form = &body[body
            .find("<form method=\"post\" action=\"/account/notifications\"")
            .unwrap()..];
        let form = &form[..form.find("</form>").unwrap()];
        assert!(
            form.contains(&format!(
                "\n{}\n",
                crate::app::submit_fallback("Enregistrer")
            )),
            "{form}"
        );
        assert_eq!(
            body.matches(r#"<button type="submit">"#).count(),
            0,
            "{body}"
        );
    }

    /// The agenda's card: three switches under its legend, the import not
    /// named (#849 leaves it to the additions and changes).
    #[test]
    fn the_agenda_changes_have_their_card() {
        let body = page_body(&settings("push", 0), "", "");
        let card = &body[body
            .find("<legend>Changements de l’agenda</legend>")
            .expect("the card")..];
        let card = &card[..card.find("</fieldset>").unwrap()];
        for name in [
            "notify_agenda_added",
            "notify_agenda_changed",
            "notify_agenda_deleted",
        ] {
            assert!(card.contains(&format!(r#"name="{name}""#)), "{name}");
        }
        assert!(!card.to_lowercase().contains("import"), "{card}");
    }

    #[test]
    fn a_saved_setting_is_announced_and_a_failure_alerts() {
        assert_eq!(
            notice_html(Some("channel_saved")),
            r#"<p class="notice success" role="status">Préférence enregistrée.</p>"#
        );
        let failed = error_html(Some("not_saved"));
        assert!(
            failed.starts_with(r#"<p class="notice error" role="alert">"#),
            "{failed}"
        );
        assert!(failed.contains("revenu à son état précédent"), "{failed}");
        // No form sends `invalid_channel` since the switches: any refusal
        // of the API is `not_saved`.
        assert_eq!(error_html(Some("invalid_channel")), "");
    }

    /// The `<option>`s of the recap's hour, and the value of the selected
    /// ones.
    fn hour_options(body: &str) -> (usize, Vec<String>) {
        let select = body
            .split(r#"<select name="expiry_digest_hour""#)
            .nth(1)
            .and_then(|rest| rest.split("</select>").next())
            .expect("the page offers the recap's hour");
        let options: Vec<&str> = select.split("<option").skip(1).collect();
        let selected = options
            .iter()
            .filter(|o| o.contains(" selected"))
            .filter_map(|o| o.split(r#"value=""#).nth(1)?.split('"').next())
            .map(str::to_string)
            .collect();
        (options.len(), selected)
    }

    /// Every hour of the day, 0 to 23, the member's selected and no other.
    #[test]
    fn the_page_offers_every_hour_and_selects_the_members() {
        for hour in [0, 9, 23] {
            let mut s = settings("push", 0);
            s.expiry_digest_hour = hour;
            let (count, selected) = hour_options(&page_body(&s, "", ""));
            assert_eq!(count, 24, "{hour}");
            assert_eq!(selected, [hour.to_string()], "{hour}");
        }
        let body = page_body(&settings("push", 0), "", "");
        assert!(body.contains(r#"<option value="7">7 h</option>"#), "{body}");
        // Labelled, in the form that saves the switches.
        assert!(
            body.contains(
                r#"<label class="field">Heure du récapitulatif <select name="expiry_digest_hour" data-submit-on-change>"#
            ),
            "{body}"
        );
        assert!(body.contains("heure de Paris"), "{body}");
        assert!(error_html(Some("invalid_hour")).contains("entre 0 h et 23 h"));
    }

    #[test]
    fn only_an_hour_of_the_day_is_a_recap_hour() {
        for hour in 0..=23 {
            assert_eq!(parse_digest_hour(&hour.to_string()), Some(hour), "{hour}");
        }
        for text in ["", "24", "-1", "9.5", " 9", "neuf", "99999999999999999999"] {
            assert_eq!(parse_digest_hour(text), None, "{text:?}");
        }
    }

    /// The `id`s of the elements of `html` carrying the `hidden` attribute.
    fn hidden_ids(html: &str) -> Vec<String> {
        html.split('<')
            .filter(|tag| tag.contains(" hidden"))
            .filter_map(|tag| {
                let id = tag.split(r#"id=""#).nth(1)?;
                Some(id[..id.find('"')?].to_string())
            })
            .collect()
    }

    #[test]
    fn push_and_both_send_notifications_and_email_does_not() {
        assert!(includes_push("push"));
        assert!(includes_push("both"));
        assert!(!includes_push("email"));
        assert!(!includes_push("none"));
        assert!(!includes_push(""));
    }

    #[test]
    fn a_member_on_notifications_without_a_device_is_warned_they_get_no_reminder() {
        let html = push_block(&settings("push", 0), false);
        assert!(html.contains(r#"id="push-warning""#), "{html}");
        assert!(!hidden_ids(&html).contains(&"push-warning".to_string()));
        assert!(html.contains("Vous ne recevrez pas vos rappels"), "{html}");
        assert!(
            html.contains(&format!(r#"href="{EMAIL_CHOICE_HREF}""#)),
            "{html}"
        );
    }

    #[test]
    fn a_member_on_both_without_a_device_is_warned_too() {
        let html = push_block(&settings("both", 0), false);
        assert!(html.contains(r#"id="push-warning""#), "{html}");
    }

    #[test]
    fn a_member_with_a_device_gets_no_server_warning() {
        let html = push_block(&settings("push", 1), false);
        assert!(!html.contains(r#"id="push-warning""#), "{html}");
    }

    #[test]
    fn the_browser_side_warnings_wait_hidden_for_the_script() {
        let html = push_block(&settings("push", 1), false);
        let hidden = hidden_ids(&html);
        for id in [
            "push-blocked",
            "push-unsupported",
            "push-enable",
            "push-enabled",
            "push-failed",
        ] {
            assert!(hidden.contains(&id.to_string()), "{id} in {hidden:?}");
        }
        assert!(html.contains(r#"data-warn="true""#), "{html}");
        assert!(html.contains(r#"data-vapid-key="BKey-_0""#), "{html}");
        // The refusal is final on the browser's side: the warning says where
        // to lift it, and offers the email instead.
        let blocked = &html[html.find(r#"id="push-blocked""#).unwrap()..];
        let blocked = &blocked[..blocked.find("</div>").unwrap()];
        assert!(blocked.contains("réglages"), "{blocked}");
        assert!(blocked.contains(EMAIL_CHOICE_HREF), "{blocked}");
        assert!(html.contains(&Script::Push.tag()));
    }

    #[test]
    fn the_enable_button_does_not_submit_the_form_it_sits_in() {
        let html = push_block(&settings("push", 0), false);
        assert!(
            html.contains(r#"<button type="button" class="secondary" id="push-enable-button">"#),
            "{html}"
        );
    }

    #[test]
    fn a_member_on_email_sees_nothing_where_reminders_are_set() {
        assert_eq!(push_block(&settings("email", 0), false), "");
    }

    #[test]
    fn a_member_on_email_may_still_subscribe_a_device_here_without_warnings() {
        let html = push_block(&settings("email", 0), true);
        assert!(!html.contains(r#"id="push-warning""#), "{html}");
        assert!(!html.contains(r#"id="push-blocked""#), "{html}");
        assert!(html.contains(r#"data-warn="false""#), "{html}");
        assert!(html.contains(r#"id="push-enable-button""#), "{html}");
    }

    #[test]
    fn a_server_without_a_key_says_notifications_are_unavailable_and_offers_no_control() {
        let mut s = settings("push", 0);
        s.vapid_public_key = None;
        let html = push_block(&s, false);
        assert!(html.contains("pas disponibles sur ce serveur"), "{html}");
        assert!(html.contains(EMAIL_CHOICE_HREF), "{html}");
        assert!(!html.contains("data-vapid-key"), "{html}");
        assert!(!html.contains("push-enable-button"), "{html}");
        assert!(!html.contains("<script"), "{html}");
    }

    #[test]
    fn the_page_says_what_a_notification_shows() {
        let body = page_body(&settings("push", 0), "", "");
        assert!(body.contains("Rappel d'un événement à venir"), "{body}");
    }

    #[test]
    fn the_page_offers_to_unsubscribe_the_devices_only_when_there_are() {
        assert!(!page_body(&settings("push", 0), "", "")
            .contains("/account/notifications/devices/remove"));
        let body = page_body(&settings("push", 2), "", "");
        assert!(
            body.contains("/account/notifications/devices/remove"),
            "{body}"
        );
        assert!(body.contains("2 appareils"), "{body}");
        assert!(page_body(&settings("push", 1), "", "").contains("1 appareil "));
    }

    #[test]
    fn a_dismissed_prompt_keeps_the_enable_button() {
        // After the prompt, the button is hidden on a refusal only: in the
        // permission callback, every `hide("push-enable")` sits inside the
        // `denied` branch. A textual check — the script is never run here.
        let callback = PUSH_SCRIPT
            .split("requestPermission().then(")
            .nth(1)
            .and_then(|rest| rest.split(".catch(").next())
            .expect("the click handler asks for the permission");
        let denied = callback
            .split(r#"if (permission === "denied") {"#)
            .nth(1)
            .expect("a refusal is handled");
        let before_denied = &callback[..callback.len() - denied.len()];
        assert!(
            !before_denied.contains(r#"hide("push-enable")"#),
            "{callback}"
        );
        let denied_block = &denied[..denied.find('}').unwrap()];
        assert!(
            denied_block.contains(r#"hide("push-enable")"#),
            "{denied_block}"
        );
        assert_eq!(callback.matches(r#"hide("push-enable")"#).count(), 1);
    }

    #[test]
    fn a_subscription_made_with_another_key_is_replaced_not_reposted() {
        // After a VAPID key rotation the browser still holds a subscription
        // bound to the old key: the push services refuse every push to it.
        // The script must compare that key with the page's, drop the old
        // subscription and make a new one — never post the old endpoint.
        // A textual check — the script is never run here.
        let subscribe = PUSH_SCRIPT
            .split("function subscribe()")
            .nth(1)
            .and_then(|rest| rest.split("\n  }\n").next())
            .expect("the script subscribes");
        assert!(
            subscribe.contains("applicationServerKey"),
            "the stored subscription's key is not read: {subscribe}"
        );
        let compare = PUSH_SCRIPT
            .split("function sameKey(")
            .nth(1)
            .expect("a key comparison exists");
        assert!(compare.contains("sub.options"), "{compare}");
        assert!(compare.contains("bytes(key)"), "{compare}");
        let reuse = subscribe
            .find("sameKey(sub)")
            .expect("the key decides reuse");
        let unsubscribe = subscribe
            .find(".unsubscribe()")
            .expect("a stale subscription is dropped");
        let resubscribe = subscribe
            .rfind("pushManager.subscribe(")
            .expect("a new one is made");
        assert!(
            reuse < unsubscribe && unsubscribe < resubscribe,
            "{subscribe}"
        );
    }

    /// The page as the app's WebView gets it (#668).
    fn app_page(s: &NotificationSettings) -> String {
        crate::mobile_app::serving(true, || page_body(s, "", ""))
    }

    #[test]
    fn in_the_app_the_local_reminders_replace_the_browsers_notifications() {
        let body = app_page(&settings("push", 0));
        assert!(body.contains(r#"id="local-reminders""#), "{body}");
        assert!(!body.contains(r#"id="push-device""#), "{body}");
        assert!(!body.contains(&Script::Push.tag()), "{body}");
        // The server's warning is about browsers: not shown in the app.
        assert!(!body.contains(r#"id="push-warning""#), "{body}");
        // What a local reminder shows is said, and its limit.
        assert!(body.contains("titre de l'événement et son heure"), "{body}");
        assert!(body.contains("hors ligne"), "{body}");
        // Out of the app, the page is the browser's.
        let browser = page_body(&settings("push", 0), "", "");
        assert!(!browser.contains("local-reminders"), "{browser}");
        assert!(browser.contains(r#"id="push-device""#), "{browser}");
    }

    #[test]
    fn the_local_states_wait_hidden_for_the_script() {
        let html = local_block(&settings("push", 0));
        let hidden = hidden_ids(&html);
        for id in [
            "local-unsupported",
            "local-granted",
            "local-ask",
            "local-denied",
            "local-exact",
            "local-count",
            "local-failed",
        ] {
            assert!(hidden.contains(&id.to_string()), "{id} in {hidden:?}");
        }
        for id in ["local-ask-button", "local-exact-button"] {
            assert!(
                html.contains(&format!(
                    r#"<button type="button" class="secondary" id="{id}">"#
                )),
                "{id}: {html}"
            );
        }
        // Exact alarms are asked for with an explanation.
        let exact = &html[html.find(r#"id="local-exact""#).unwrap()..];
        assert!(exact.contains("Alarmes et rappels"), "{exact}");
        // A refusal says where to lift it, and offers the email instead.
        let denied = &html[html.find(r#"id="local-denied""#).unwrap()..];
        let denied = &denied[..denied.find("</div>").unwrap()];
        assert!(denied.contains("Réglages"), "{denied}");
        assert!(denied.contains(EMAIL_CHOICE_HREF), "{denied}");
        // The script is the shell's, in the head: none here.
        assert!(!html.contains("<script"), "{html}");
    }

    #[test]
    fn in_the_app_a_member_on_email_is_told_nothing_is_scheduled() {
        let told = "Vos rappels partent par email";
        assert!(local_block(&settings("email", 0)).contains(told));
        assert!(!local_block(&settings("push", 0)).contains(told));
        assert!(!local_block(&settings("both", 0)).contains(told));
        // Both switches off (#853): not by email either.
        let none = local_block(&settings("none", 0));
        assert!(!none.contains(told), "{none}");
        assert!(
            none.contains("aucun n'est programmé sur ce téléphone"),
            "{none}"
        );
    }

    fn api(status: u16, body: serde_json::Value) -> Result<crate::state::ApiResponse, String> {
        Ok(crate::state::ApiResponse {
            status: reqwest::StatusCode::from_u16(status).unwrap(),
            set_cookie: None,
            body,
        })
    }

    #[tokio::test]
    async fn the_upcoming_reminders_pass_through_as_the_api_lists_them() {
        let entry = serde_json::json!({
            "id": "0f8fad5b-d9cb-469f-a165-70867728950e",
            "event_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
            "group_id": "16fd2706-8baf-433b-82eb-8c7fada847da",
            "title": "Dentiste",
            "occurrence_at": "2026-11-10T13:30:00Z",
            "fire_at": "2026-11-09T13:30:00Z",
            "all_day": false,
        });
        let resp = upcoming_answer(api(200, serde_json::json!([entry.clone()])));
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-store");
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body, serde_json::json!([entry]));
        let empty = upcoming_answer(api(200, serde_json::json!([])));
        assert_eq!(empty.status(), StatusCode::OK);
    }

    #[test]
    fn signed_out_is_a_401_and_anything_else_a_502() {
        // The script cancels every reminder on a 401, and on that only: a
        // failure of the server must not wipe the phone's reminders.
        let signed_out = upcoming_answer(api(401, serde_json::json!({"error": "unauthorized"})));
        assert_eq!(signed_out.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(signed_out.headers()[header::CACHE_CONTROL], "no-store");
        for answer in [
            api(500, serde_json::Value::Null),
            api(403, serde_json::Value::Null),
            api(404, serde_json::Value::Null),
            api(200, serde_json::json!({"not": "a list"})),
            api(200, serde_json::json!([{"id": "nope"}])),
            Err("connection refused".to_string()),
        ] {
            let resp = upcoming_answer(answer);
            assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        }
    }

    /// The API's answer passes through when it is a kind pushed; anything
    /// else — `null`, a kind never pushed, no answer at all — is the
    /// neutral text.
    #[test]
    fn only_a_kind_pushed_passes_through() {
        for kind in PUSHED_KINDS {
            assert_eq!(
                pushed_kind(&serde_json::json!({ "kind": kind })),
                Some(kind)
            );
        }
        // A change to the agenda by another member (#851).
        assert_eq!(
            pushed_kind(&serde_json::json!({ "kind": "agenda_change" })),
            Some("agenda_change")
        );
        assert_eq!(pushed_kind(&serde_json::json!({ "kind": null })), None);
        assert_eq!(pushed_kind(&serde_json::json!({ "kind": "message" })), None);
        assert_eq!(pushed_kind(&serde_json::json!({ "kind": "mixed" })), None);
        assert_eq!(pushed_kind(&serde_json::json!({ "kind": 1 })), None);
        assert_eq!(pushed_kind(&serde_json::json!({})), None);
        assert_eq!(pushed_kind(&serde_json::Value::Null), None);
    }

    /// One fixed text per kind pushed, chosen on what `last_push` says
    /// about this very device.
    #[test]
    fn the_service_worker_shows_a_fixed_text_per_kind() {
        assert!(SERVICE_WORKER.contains(r#"fetch("/account/notifications/last-push""#));
        // Taking the push off the device's count is a change: a `POST`, never
        // a `GET` (#783).
        assert!(SERVICE_WORKER.contains(r#"method: "POST""#));
        assert!(SERVICE_WORKER.contains("pushManager.getSubscription()"));
        assert!(SERVICE_WORKER.contains(r#""X-Push-Endpoint": subscription.endpoint"#));
        for kind in PUSHED_KINDS {
            assert!(SERVICE_WORKER.contains(&format!("{kind}: {{")), "{kind}");
        }
        assert!(SERVICE_WORKER.contains(r#"title: "Rappel d'un événement à venir""#));
        assert!(SERVICE_WORKER.contains(r#"title: "Alerte de budget""#));
        assert!(SERVICE_WORKER.contains(r#"title: "Produits bientôt périmés""#));
        // Neither the event's title nor its author: the family's agenda.
        assert!(SERVICE_WORKER.contains(r#"title: "Agenda de la famille modifié""#));
        // Of the push's data, the identifiers of its entries alone (#862):
        // the text shown is always one of the fixed ones above.
        assert!(SERVICE_WORKER.contains("pushedEntries(event.data)"));
        assert_eq!(SERVICE_WORKER.matches("event.data").count(), 1);
        assert!(SERVICE_WORKER.contains("showNotification(text.title, {"));
    }

    /// A click on a push marks the entries it named read (#862), in a
    /// `POST` of JSON to the route that does it, before the page opens.
    #[test]
    fn a_click_on_a_push_marks_its_entries_read_before_opening() {
        assert!(SERVICE_WORKER.contains(r#"fetch("/notifications/pushed/read""#));
        assert!(SERVICE_WORKER.contains(r#""Content-Type": "application/json""#));
        let click = SERVICE_WORKER
            .find(r#"addEventListener("notificationclick""#)
            .unwrap();
        let handler = &SERVICE_WORKER[click..];
        let marked = handler.find("markRead(data.entries)").unwrap();
        let opened = handler.find("openWindow(").unwrap();
        assert!(marked < opened);
        // As many as the API puts in a push at most.
        assert!(SERVICE_WORKER.contains(&format!(
            "var MAX_ENTRIES = {};",
            crate::routes::notifications::MAX_PUSHED_ENTRIES
        )));
    }

    /// The page registers the subscription's keys with its endpoint (#862):
    /// without them no payload can be encrypted for the device.
    #[test]
    fn the_subscription_is_registered_with_its_keys() {
        assert!(PUSH_SCRIPT
            .contains("JSON.stringify({ endpoint: sub.endpoint, keys: sub.toJSON().keys })"));
    }
}
