// The reminders of the Android app, scheduled on the phone (#668).
//
// Loaded by `app::document` on the pages the app's WebView asks for, and
// on those only (`mobile_app::is_mobile_app`): a browser never gets it.
// It reaches the Local Notifications plugin through the global the
// Capacitor bridge puts in the page (`window.Capacitor.Plugins`), since
// the site is not bundled into the app (apps/mobile/README.md).
//
// On each page and each return to the foreground (`App` `resume`), it reads
// the member's next reminders (`GET /account/notifications/upcoming`, which
// relays apps/api's `/account/reminders/upcoming`, #667), cancels every
// notification the app scheduled, and schedules the list again. Android
// then shows each one at its time with no network, the app killed. A 401
// (logged out, account deactivated or deleted) cancels them all; no answer
// at all (offline) leaves them as they are.
//
// It never opens a prompt by itself: without the notification permission
// it schedules nothing (the plugin's `schedule` would ask for it), and
// without exact alarms it schedules them inexact (the plugin would open
// Android's settings). Both are asked for on `/account/notifications`, on
// a button, by the block `account::notifications::local_block` renders.
//
// The pure part (`notificationId`, `toNotification`, `plan`, `countText`,
// `touchTarget`) and `sync` are tested by apps/mobile/scripts/
// local-reminders.test.mjs, which runs this file in a context with no
// `window`: only `start` touches the page.
//
// It also registers the phone for the notifications the server pushes by
// Firebase Cloud Messaging (#1004): the agenda changes, the budget alerts
// and the expiry recap — never a reminder, which stays local. With the
// notification permission granted and the member logged in, it asks the
// Push Notifications plugin for the phone's token on each page and each
// resume, and hands it to the site (`POST /account/notifications/fcm-token`,
// relayed to apps/api's `/account/fcm-tokens`, #1002), which moves the
// device's `last_seen_at`. Logged out (a 401 above), it withdraws the
// token from Firebase; on the logout form, it has the site forget it
// first. The notification itself is the app's (PushMessagingService.java,
// the fixed texts of sw.js): the page is not there when it comes. A touch
// on it opens the app, and the page then marks its entries read and goes
// to the kind's page. An APK built without Firebase (no
// google-services.json) says so through the app's PushSupport plugin, and
// the Push Notifications plugin is then never called: its `register`
// would throw out of the bridge. `pushPage`, `pushedEntries`,
// `pushTouch`, `pushSync`, `saveToken`, `forgetToken` and `isLogoutForm`
// are tested by apps/mobile/scripts/app-pushes.test.mjs.
var MHomeLocalReminders = (function () {
  "use strict";

  var UPCOMING_URL = "/account/notifications/upcoming";
  var PUSH_TOKEN_URL = "/account/notifications/fcm-token";
  var PUSHED_READ_URL = "/notifications/pushed/read";
  // The page of each kind of push, as sw.js opens it; "/" for any other.
  var PUSH_PAGES = {
    agenda_reminder: "/agenda",
    budget_alert: "/budget",
    stock_expiry: "/stocks",
    agenda_change: "/agenda"
  };
  // At most what the API ever names in a push (push::MAX_PAYLOAD_ENTRIES).
  var MAX_PUSHED_ENTRIES = 20;
  var ENTRY_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
  // How long the logout and a touch wait for the site before going on.
  var PUSH_WAIT_MS = 1500;
  // The plugin's "no notification" in the intent that opens the activity.
  var INT_MIN = -2147483648;
  var UUID = /^[0-9a-f]{8}-?[0-9a-f]{4}-?[0-9a-f]{4}-?[0-9a-f]{4}-?[0-9a-f]{12}$/i;

  // The notification's identifier: Android's is a 32-bit int, the
  // reminder's a UUID. The XOR of its four 32-bit words, so that every
  // part counts; the same UUID always gives the same int. `null` for what
  // is not a UUID.
  function notificationId(uuid) {
    if (typeof uuid !== "string" || !UUID.test(uuid)) return null;
    var hex = uuid.replace(/-/g, "");
    var id = 0;
    for (var i = 0; i < 32; i += 8) id ^= parseInt(hex.slice(i, i + 8), 16);
    id |= 0;
    return id === INT_MIN ? 0 : id;
  }

  function instant(text) {
    if (typeof text !== "string") return null;
    var at = new Date(text);
    return isNaN(at.getTime()) ? null : at;
  }

  // `14:30`, as the agenda writes a time.
  function timeText(at, timeZone) {
    return new Intl.DateTimeFormat("fr-FR", {
      timeZone: timeZone, hour: "2-digit", minute: "2-digit", hourCycle: "h23"
    }).format(at);
  }

  // `2026-11-10T13:30:00Z`, the agenda's `occ` parameter.
  function occParam(at) {
    return at.toISOString().replace(/\.\d{3}Z$/, "Z");
  }

  // The notification for one entry of the API's list, `null` when the
  // entry cannot be read. `options`: `timeZone` (the phone's when
  // undefined), `exact` (exact alarms allowed).
  //
  // Its text: the event's title, then the time of the occurrence in the
  // phone's zone, or « toute la journée ». No date: title and time only
  // (choice of 2026-11-01, confirmed on 2026-11-07, #724).
  function toNotification(entry, options) {
    if (!entry || typeof entry !== "object") return null;
    var id = notificationId(entry.id);
    var occurrence = instant(entry.occurrence_at);
    var fire = instant(entry.fire_at);
    if (id === null || !occurrence || !fire) return null;
    if (typeof entry.title !== "string" || !UUID.test(String(entry.event_id)) || !UUID.test(String(entry.group_id))) {
      return null;
    }
    return {
      id: id,
      title: entry.title,
      body: entry.all_day === true ? "Toute la journée" : "À " + timeText(occurrence, options.timeZone),
      schedule: { at: fire, allowWhileIdle: true },
      isExactNotification: options.exact === true,
      extra: {
        url: "/agenda/" + entry.event_id +
          "?famille=" + entry.group_id +
          "&occ=" + occParam(occurrence)
      }
    };
  }

  // The notifications to schedule: those of `entries` still to fire at
  // `options.now`, by time, one per identifier (the first). An entry
  // whose time has passed would fire at once on Android.
  function plan(entries, options) {
    if (!Array.isArray(entries)) return [];
    var seen = {};
    return entries
      .map(function (entry) { return toNotification(entry, options); })
      .filter(function (n) { return n && n.schedule.at.getTime() > options.now; })
      .sort(function (a, b) { return a.schedule.at.getTime() - b.schedule.at.getTime(); })
      .filter(function (n) {
        if (seen[n.id]) return false;
        seen[n.id] = true;
        return true;
      });
  }

  function countText(n) {
    if (n === 0) return "Aucun rappel programmé sur ce téléphone.";
    if (n === 1) return "1 rappel programmé sur ce téléphone.";
    return n + " rappels programmés sur ce téléphone.";
  }

  // Where a touch on a notification leads: its agenda link, and nothing
  // else (`null`).
  function touchTarget(action) {
    var url = action && action.notification && action.notification.extra && action.notification.extra.url;
    return typeof url === "string" && /^\/agenda\/[0-9a-f-]+\?/i.test(url) ? url : null;
  }

  // One synchronisation. `env`: `plugin` (Local Notifications), `fetch`,
  // `now` (ms), `timeZone`. Resolves to what happened, never rejects:
  // - `signed-out`: 401, everything cancelled;
  // - `unreachable`: no usable answer, nothing touched;
  // - `not-allowed`: no notification permission (`display` says which
  //   state), everything cancelled, nothing scheduled;
  // - `scheduled`: `count` scheduled, `exact` or not;
  // - `failed`: the plugin refused.
  function sync(env) {
    var plugin = env.plugin;
    return Promise.resolve()
      .then(function () {
        return env.fetch(UPCOMING_URL, {
          credentials: "same-origin",
          headers: { Accept: "application/json" }
        });
      })
      .then(function (res) {
        if (res.status === 401) return { signedOut: true };
        if (!res.ok) return null;
        return res.json().then(function (body) {
          return Array.isArray(body) ? { entries: body } : null;
        });
      }, function () { return null; })
      .then(function (answer) {
        if (!answer) return { state: "unreachable" };
        if (answer.signedOut) {
          return Promise.resolve(plugin.cancelAll()).then(function () {
            return { state: "signed-out", count: 0 };
          });
        }
        return Promise.resolve(plugin.checkPermissions()).then(function (permission) {
          var display = permission && permission.display;
          if (display !== "granted") {
            return Promise.resolve(plugin.cancelAll()).then(function () {
              return { state: "not-allowed", count: 0, display: display };
            });
          }
          return Promise.resolve(plugin.checkExactNotificationSetting()).then(function (setting) {
            var exact = !!setting && setting.exact_alarm === "granted";
            var notifications = plan(answer.entries, { timeZone: env.timeZone, now: env.now, exact: exact });
            return Promise.resolve(plugin.cancelAll())
              .then(function () {
                if (notifications.length) return plugin.schedule({ notifications: notifications });
              })
              .then(function () {
                return { state: "scheduled", count: notifications.length, exact: exact };
              });
          });
        });
      })
      .catch(function () { return { state: "failed" }; });
  }

  // -- the pushes (#1004) ----------------------------------------------------

  function pushPage(kind) {
    return typeof kind === "string" && Object.prototype.hasOwnProperty.call(PUSH_PAGES, kind)
      ? PUSH_PAGES[kind]
      : "/";
  }

  // The entries a push names, its `entries` data: ids separated by commas,
  // [] for anything else or for more than the API sends. What is not an id
  // is left out.
  function pushedEntries(text) {
    if (typeof text !== "string" || text === "") return [];
    var ids = text.split(",");
    if (ids.length > MAX_PUSHED_ENTRIES) return [];
    return ids.filter(function (id) { return ENTRY_ID.test(id); });
  }

  // A touch on a push (`pushNotificationActionPerformed`): the page to
  // open and the entries to mark read, `null` for what is not a push.
  function pushTouch(action) {
    var data = action && action.notification && action.notification.data;
    if (!data || typeof data.kind !== "string") return null;
    return { url: pushPage(data.kind), entries: pushedEntries(data.entries) };
  }

  // One registration. `env`: `push` (Push Notifications), `support` (the
  // app's PushSupport), `signedOut`. Resolves to what happened, never
  // rejects: `unsupported` (an APK without the plugins), `unavailable` (no
  // Firebase in the APK), `forgotten` (signed out: the token withdrawn),
  // `not-allowed` (no permission, and no prompt), `registered` (the token
  // will come by the plugin's `registration` event), `failed`.
  function pushSync(env) {
    var push = env.push;
    var support = env.support;
    if (!push || !support) return Promise.resolve("unsupported");
    return Promise.resolve()
      .then(function () { return support.status(); })
      .then(function (status) {
        if (!status || status.available !== true) return "unavailable";
        if (env.signedOut) {
          return Promise.resolve(push.unregister()).then(function () { return "forgotten"; });
        }
        return Promise.resolve(push.checkPermissions()).then(function (permission) {
          if (!permission || permission.receive !== "granted") return "not-allowed";
          return Promise.resolve(push.register()).then(function () { return "registered"; });
        });
      })
      .catch(function () { return "failed"; });
  }

  function tokenRequest(method, token) {
    return {
      method: method,
      credentials: "same-origin",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ token: token })
    };
  }

  // Hands the phone's token to the site: true once stored.
  function saveToken(env, token) {
    if (typeof token !== "string" || token === "") return Promise.resolve(false);
    return Promise.resolve()
      .then(function () { return env.fetch(PUSH_TOKEN_URL, tokenRequest("POST", token)); })
      .then(function (res) { return !!res && res.ok === true; }, function () { return false; });
  }

  // As the member logs out: the site forgets the token (while the session
  // still holds), then Firebase withdraws it. Never rejects: the logout
  // goes on whatever fails.
  function forgetToken(env, token) {
    var told = typeof token === "string" && token !== ""
      ? Promise.resolve()
        .then(function () { return env.fetch(PUSH_TOKEN_URL, tokenRequest("DELETE", token)); })
        .catch(function () {})
      : Promise.resolve();
    return told
      .then(function () { if (env.push) return env.push.unregister(); })
      .catch(function () {});
  }

  // The forms that end the session: `POST /logout`.
  function isLogoutForm(form) {
    if (!form || typeof form.getAttribute !== "function") return false;
    return form.getAttribute("action") === "/logout" &&
      String(form.getAttribute("method")).toLowerCase() === "post";
  }

  function within(ms, promise) {
    return Promise.race([promise, new Promise(function (resolve) { setTimeout(resolve, ms); })]);
  }

  // In the app's page: synchronise now and on each resume, follow a touch
  // on a notification, and drive the block of `/account/notifications`.
  function start(win) {
    var cap = win.Capacitor;
    var plugins = cap && cap.Plugins;
    var plugin = plugins && plugins.LocalNotifications;
    var push = plugins && plugins.PushNotifications;
    var support = plugins && plugins.PushSupport;
    var doc = win.document;
    var last = null;
    var running = Promise.resolve();
    // The phone's token, once the plugin gave it on this page.
    var token = null;
    var pushes = "unsupported";
    var leaving = false;

    function el(id) { return doc.getElementById(id); }
    function show(id, on) { var e = el(id); if (e) e.hidden = !on; }
    function onReady(fn) {
      if (doc.readyState === "loading") doc.addEventListener("DOMContentLoaded", fn);
      else fn();
    }

    function render() {
      if (!el("local-reminders") || !last) return;
      var r = last;
      var allowed = r.state === "scheduled";
      show("local-granted", allowed);
      show("local-ask", r.state === "not-allowed" && r.display !== "denied");
      show("local-denied", r.state === "not-allowed" && r.display === "denied");
      show("local-exact", allowed && !r.exact);
      var count = el("local-count");
      if (count) {
        count.hidden = !allowed;
        count.textContent = allowed ? countText(r.count) : "";
      }
      show("local-failed", r.state === "failed" || r.state === "unreachable");
    }

    function run() {
      running = running.then(function () {
        return sync({
          plugin: plugin,
          fetch: function (url, init) { return win.fetch(url, init); },
          now: Date.now(),
          timeZone: undefined
        });
      }).then(function (result) {
        last = result;
        onReady(render);
        // Offline, nothing to tell the site; signed out, the token goes.
        if (result.state === "unreachable") return null;
        return pushSync({ push: push, support: support, signedOut: result.state === "signed-out" })
          .then(function (state) { pushes = state; });
      });
      return running;
    }

    if (!plugin) {
      // An APK built before #668: no plugin, nothing can be scheduled.
      onReady(function () { show("local-unsupported", true); });
      return;
    }

    plugin.addListener("localNotificationActionPerformed", function (action) {
      var target = touchTarget(action);
      if (target) win.location.assign(target);
    });
    if (push && support) {
      push.addListener("registration", function (event) {
        token = event && typeof event.value === "string" ? event.value : null;
        saveToken({ fetch: function (url, init) { return win.fetch(url, init); } }, token);
      });
      // A push received with the page open: already shown by the app
      // (PushMessagingService). Heard all the same, or the plugin would
      // keep each one for a listener that never comes.
      push.addListener("pushNotificationReceived", function () {});
      // A touch on a push: its entries marked read (#862), then its page.
      push.addListener("pushNotificationActionPerformed", function (action) {
        var touched = pushTouch(action);
        if (!touched) return;
        var marked = touched.entries.length
          ? win.fetch(PUSHED_READ_URL, {
            method: "POST",
            credentials: "same-origin",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ entries: touched.entries })
          }).catch(function () {})
          : Promise.resolve();
        within(PUSH_WAIT_MS, marked).then(function () { win.location.assign(touched.url); });
      });
      // The logout form waits for the site to forget the token, a moment
      // at most, then goes.
      doc.addEventListener("submit", function (event) {
        var form = event.target;
        if (leaving || pushes !== "registered" || !isLogoutForm(form)) return;
        event.preventDefault();
        leaving = true;
        var go = function () { form.submit(); };
        within(PUSH_WAIT_MS, forgetToken({
          fetch: function (url, init) { return win.fetch(url, init); },
          push: push
        }, token)).then(go, go);
      });
    }
    if (plugins.App) plugins.App.addListener("resume", run);
    run();

    onReady(function () {
      var ask = el("local-ask-button");
      if (ask) {
        ask.addEventListener("click", function () {
          ask.disabled = true;
          Promise.resolve(plugin.requestPermissions())
            .then(run, run)
            .then(function () { ask.disabled = false; });
        });
      }
      var exact = el("local-exact-button");
      if (exact) {
        exact.addEventListener("click", function () {
          exact.disabled = true;
          Promise.resolve(plugin.changeExactNotificationSetting())
            .then(run, run)
            .then(function () { exact.disabled = false; });
        });
      }
    });
  }

  return {
    UPCOMING_URL: UPCOMING_URL,
    notificationId: notificationId,
    toNotification: toNotification,
    plan: plan,
    countText: countText,
    touchTarget: touchTarget,
    sync: sync,
    PUSH_TOKEN_URL: PUSH_TOKEN_URL,
    pushPage: pushPage,
    pushedEntries: pushedEntries,
    pushTouch: pushTouch,
    pushSync: pushSync,
    saveToken: saveToken,
    forgetToken: forgetToken,
    isLogoutForm: isLogoutForm,
    start: start
  };
})();

if (typeof window !== "undefined" && window.document) MHomeLocalReminders.start(window);
