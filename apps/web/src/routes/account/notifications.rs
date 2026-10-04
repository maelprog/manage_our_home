//! `/account/notifications` — how reminders reach the member (#306): by
//! notification on their devices, by email, or both; and this device's
//! subscription.
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
//! The service worker is [`SERVICE_WORKER`], served at `/sw.js`: it shows
//! « Rappel d'un événement à venir » for every push, never the event's
//! title, the same choice as the reminder emails' subject (#146).

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::{Form, Json};
use manage_our_home_shared::dto::notifications::{
    NotificationSettings, PushSubscriptionRequest, UpdateNotificationSettings,
};

use crate::app::{html_escape, shell_with_header, Width};
use crate::assets::Script;
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{account_cookie, account_header, service_unavailable_page};

/// Where the warnings send a member who would rather have emails: the
/// email choice of the form below, by its `id`.
pub(crate) const EMAIL_CHOICE_HREF: &str = "/account/notifications#rappels-email";

/// The three channels, as the API spells them, with the label of each.
const CHANNELS: [(&str, &str); 3] = [
    ("push", "Par notification sur mes appareils"),
    ("email", "Par email"),
    ("both", "Les deux"),
];

/// Whether `channel` sends notifications (`push` or `both`).
pub(crate) fn includes_push(channel: &str) -> bool {
    matches!(channel, "push" | "both")
}

/// Whether `channel` is one the API takes.
pub(crate) fn is_channel(channel: &str) -> bool {
    CHANNELS.iter().any(|(value, _)| *value == channel)
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
///   endpoint stored (`POST /account/notifications/subscription`), with no
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
          body: JSON.stringify({ endpoint: sub.endpoint })
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
    format!(r#"<p class="notice success">{}</p>"#, html_escape(text))
}

fn error_html(error: Option<&str>) -> String {
    let text = match error {
        Some("invalid_channel") => "Choix invalide, merci de réessayer.",
        Some("unavailable") => "Service momentanément indisponible, merci de réessayer.",
        _ => return String::new(),
    };
    format!(r#"<p class="notice error">{}</p>"#, html_escape(text))
}

/// The body of the page.
fn page_body(settings: &NotificationSettings, notice: &str, error: &str) -> String {
    let choices: String = CHANNELS
        .iter()
        .map(|(value, label)| {
            let id = if *value == "email" {
                r#" id="rappels-email""#
            } else {
                ""
            };
            let checked = if *value == settings.reminder_channel {
                " checked"
            } else {
                ""
            };
            format!(
                r#"<label class="field inline"><input type="radio" name="reminder_channel" value="{value}"{id}{checked}/> {label}</label>
"#
            )
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
    format!(
        r#"<p><a href="/account">← Retour à mon compte</a></p>
<h1>Notifications de rappel</h1>
{notice}{error}
<p>Les rappels de vos événements vous parviennent par notification sur les appareils où vous les avez activées, par email, ou les deux. Une notification n'affiche jamais le titre de l'événement, seulement « Rappel d'un événement à venir » : le titre se lit dans l'agenda.</p>
<p class="muted">Sans appareil abonné, un rappel par notification n'est pas envoyé du tout, et aucun email ne le remplace.</p>
<form method="post" action="/account/notifications">
<fieldset class="card">
<legend>Recevoir mes rappels</legend>
{choices}</fieldset>
<button type="submit">Enregistrer</button>
</form>
<section class="card">
<h2>Mes appareils</h2>
{block}
<p>{devices}</p>
{remove}
</section>"#,
        block = push_block(settings, true),
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

#[derive(serde::Deserialize)]
pub struct ChannelForm {
    #[serde(default)]
    reminder_channel: String,
}

/// `POST /account/notifications` — the channel form.
pub async fn post(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ChannelForm>,
) -> Response {
    if !is_channel(&form.reminder_channel) {
        return Redirect::to("/account/notifications?error=invalid_channel").into_response();
    }
    let cookie = account_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::PUT,
        "/account/notifications",
        cookie.as_deref(),
        Some(
            serde_json::to_value(UpdateNotificationSettings {
                reminder_channel: form.reminder_channel,
            })
            .unwrap(),
        ),
    )
    .await;
    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            "/account/notifications?notice=channel_saved"
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            "/account/notifications?error=invalid_channel"
        }
        _ => "/account/notifications?error=unavailable",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(channel: &str, devices: i64) -> NotificationSettings {
        NotificationSettings {
            reminder_channel: channel.into(),
            push_subscriptions: devices,
            vapid_public_key: Some("BKey-_0".into()),
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
        assert!(!includes_push(""));
    }

    #[test]
    fn only_the_three_channels_are_channels() {
        for c in ["push", "email", "both"] {
            assert!(is_channel(c), "{c}");
        }
        for c in ["", "sms", "Push", "push "] {
            assert!(!is_channel(c), "{c:?}");
        }
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
    fn the_page_checks_the_current_channel_and_names_the_email_choice() {
        for (channel, _) in CHANNELS {
            let body = page_body(&settings(channel, 0), "", "");
            let checked: Vec<&str> = body
                .split("<input")
                .skip(1)
                .filter(|i| i.contains(" checked"))
                .collect();
            assert_eq!(checked.len(), 1, "{channel}: {checked:?}");
            assert!(
                checked[0].contains(&format!(r#"value="{channel}""#)),
                "{channel}"
            );
        }
        let body = page_body(&settings("push", 0), "", "");
        assert!(body.contains(r#"id="rappels-email""#), "{body}");
        assert!(body.contains(r#"value="email""#));
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

    #[test]
    fn the_service_worker_shows_the_neutral_text_only() {
        assert!(SERVICE_WORKER.contains(r#"showNotification("Rappel d'un événement à venir""#));
        // It never reads the push's data: there is none, and there must not
        // be any title to show.
        assert!(!SERVICE_WORKER.contains("event.data"));
    }
}
