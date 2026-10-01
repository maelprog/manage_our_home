// Service worker of the reminder notifications (#306), served at /sw.js
// by apps/web/src/routes/account/notifications.rs, out of the binary.
//
// A push carries no payload (apps/api/src/notifications/push.rs): every
// one shows the same neutral text, like the subject of the reminder emails
// (#146). The event's title is never on the lock screen; the member reads
// it in the agenda, once the click has opened it.
"use strict";

self.addEventListener("push", function (event) {
  event.waitUntil(
    self.registration.showNotification("Rappel d'un événement à venir", {
      body: "Ouvrez l'agenda pour le voir.",
      lang: "fr",
    })
  );
});

self.addEventListener("notificationclick", function (event) {
  event.notification.close();
  event.waitUntil(self.clients.openWindow("/agenda"));
});
