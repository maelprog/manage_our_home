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
var MHomeLocalReminders = (function () {
  "use strict";

  var UPCOMING_URL = "/account/notifications/upcoming";
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

  // In the app's page: synchronise now and on each resume, follow a touch
  // on a notification, and drive the block of `/account/notifications`.
  function start(win) {
    var cap = win.Capacitor;
    var plugins = cap && cap.Plugins;
    var plugin = plugins && plugins.LocalNotifications;
    var doc = win.document;
    var last = null;
    var running = Promise.resolve();

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
    start: start
  };
})();

if (typeof window !== "undefined" && window.document) MHomeLocalReminders.start(window);
