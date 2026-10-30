// Service worker of the notifications pushed to the devices: the agenda
// reminders (#306) and the budget alerts (#536). Served at /sw.js by
// apps/web/src/routes/account/notifications.rs, out of the binary.
//
// A push carries no payload (apps/api/src/notifications/push.rs). The
// worker asks the site which kind of push this device got
// (/account/notifications/last-push, the member's session going along, the
// device named by its subscription's endpoint) and shows that kind's fixed
// text, like the subject of the reminder emails (#146): never an event's
// title, a category or an amount on the lock screen. The member reads them
// in the application, once the click has opened it. When the site cannot
// tell — pushes of two kinds on their way to this device at once, no
// session, no network — the text is a neutral one.
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
};

var NEUTRAL = {
  title: "Nouvelle notification",
  body: "Ouvrez l'application pour la voir.",
  url: "/",
};

function lastPushedKind() {
  return self.registration.pushManager.getSubscription()
    .then(function (subscription) {
      if (!subscription) {
        return null;
      }
      return fetch("/account/notifications/last-push", {
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
  event.waitUntil(
    lastPushedKind().then(function (kind) {
      var text = Object.prototype.hasOwnProperty.call(TEXTS, kind) ? TEXTS[kind] : NEUTRAL;
      return self.registration.showNotification(text.title, {
        body: text.body,
        lang: "fr",
        data: { url: text.url },
      });
    })
  );
});

self.addEventListener("notificationclick", function (event) {
  event.notification.close();
  var data = event.notification.data;
  event.waitUntil(self.clients.openWindow((data && data.url) || "/"));
});
