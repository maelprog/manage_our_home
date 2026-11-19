// Service worker of the notifications pushed to the devices: the agenda
// reminders (#306), the budget alerts (#536), the daily recap of the
// stock items nearing their expiry date (#566) and the changes another
// member made to the agenda (#851). Served at /sw.js by
// apps/web/src/routes/account/notifications.rs, out of the binary.
//
// A push carries no text (apps/api/src/notifications/push.rs). The
// worker asks the site which kind of push this device got (a POST to
// /account/notifications/last-push, since the answer takes the push off
// the device's count (#783); the member's session going along, the device
// named by its subscription's endpoint) and shows that kind's fixed
// text, like the subject of the reminder emails (#146): never an event's
// title, a category or an amount on the lock screen. The member reads them
// in the application, once the click has opened it. When the site cannot
// tell — pushes of two kinds on their way to this device at once, no
// session, no network — the text is a neutral one.
//
// What a push may carry (#862): the identifiers of the entries of the
// notification centre it stands for, `{"entries": [...]}`, encrypted for
// this browser alone. Never a text: the worker shows none of it. On the
// click, it asks the site to mark them read (POST /notifications/pushed/read)
// before it opens the page, so that the count the page shows is already
// down.
"use strict";

var TEXTS = {
  agenda_reminder: {
    title: "Rappel d'un événement à venir",
    body: "Ouvrez l'agenda pour le voir.",
    url: "/agenda",
  },
  budget_alert: {
    title: "Alerte de budget",
    body: "Les dépenses du mois ont franchi un seuil du budget. Ouvrez le budget pour le voir.",
    url: "/budget",
  },
  stock_expiry: {
    title: "Produits bientôt périmés",
    body: "Des produits du stock arrivent à leur date. Ouvrez les stocks pour les voir.",
    url: "/stocks",
  },
  // A change another member made to the agenda (#851): neither the
  // event's title nor its author's name, only that the agenda changed.
  agenda_change: {
    title: "Agenda de la famille modifié",
    body: "Un événement a été ajouté, modifié ou supprimé. Ouvrez l'agenda pour le voir.",
    url: "/agenda",
  },
};

var NEUTRAL = {
  title: "Nouvelle notification",
  body: "Ouvrez l'application pour la voir.",
  url: "/",
};

// Of the push's payload, the identifiers it names, at most MAX_ENTRIES —
// the API never sends more: [] for anything else (no payload, a device
// registered without its keys, a payload that is not one).
var MAX_ENTRIES = 20;
var ENTRY_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

function pushedEntries(data) {
  var payload = null;
  try {
    payload = data ? data.json() : null;
  } catch (e) {
    return [];
  }
  var entries = payload && payload.entries;
  if (!Array.isArray(entries) || entries.length > MAX_ENTRIES) {
    return [];
  }
  return entries.filter(function (id) {
    return typeof id === "string" && ENTRY_ID.test(id);
  });
}

function lastPushedKind() {
  return self.registration.pushManager.getSubscription()
    .then(function (subscription) {
      if (!subscription) {
        return null;
      }
      return fetch("/account/notifications/last-push", {
        method: "POST",
        credentials: "same-origin",
        headers: {
          Accept: "application/json",
          "X-Push-Endpoint": subscription.endpoint,
        },
      })
        .then(function (response) {
          return response.ok ? response.json() : null;
        })
        .then(function (answer) {
          return answer && answer.kind;
        });
    })
    .catch(function () {
      return null;
    });
}

self.addEventListener("push", function (event) {
  var entries = pushedEntries(event.data);
  event.waitUntil(
    lastPushedKind().then(function (kind) {
      var text = Object.prototype.hasOwnProperty.call(TEXTS, kind) ? TEXTS[kind] : NEUTRAL;
      return self.registration.showNotification(text.title, {
        body: text.body,
        lang: "fr",
        data: { url: text.url, entries: entries },
      });
    })
  );
});

// Marks the entries read, giving up waiting after READ_WAIT_MS: a browser
// lets a click open a window only for a moment after it (Firefox: one
// second), and the page must open whatever the network does.
var READ_WAIT_MS = 700;

function markRead(entries) {
  if (!entries || !entries.length) {
    return Promise.resolve();
  }
  var marked = fetch("/notifications/pushed/read", {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ entries: entries }),
  }).catch(function () {});
  var late = new Promise(function (resolve) {
    setTimeout(resolve, READ_WAIT_MS);
  });
  return Promise.race([marked, late]);
}

self.addEventListener("notificationclick", function (event) {
  event.notification.close();
  var data = event.notification.data || {};
  event.waitUntil(
    markRead(data.entries).then(function () {
      return self.clients.openWindow(data.url || "/");
    })
  );
});
