// The behaviours apps/web's markup asks for (#325), loaded by every page
// from the <head> (`app::document`, `assets::Script::Enhance`). They were
// inline event handlers until then; infra/Caddyfile's CSP now runs no inline
// script at all. Each listens on the document, so it applies to whatever
// the page holds, and none of them is needed for the page to work: without
// JavaScript the forms still submit and the fields keep their types.
(function () {
  "use strict";

  function closest(node, selector) {
    return node && node.closest ? node.closest(selector) : null;
  }

  // `data-pw-toggle` (`app::password_field`): shows or hides the password
  // of the field next to the button.
  document.addEventListener("click", function (event) {
    var button = closest(event.target, "[data-pw-toggle]");
    if (!button) return;
    var input = button.parentNode.querySelector("input");
    var show = input.type === "password";
    input.type = show ? "text" : "password";
    button.setAttribute("aria-label", show ? "Masquer le mot de passe" : "Afficher le mot de passe");
    button.classList.toggle("shown", show);
  });

  // `data-confirm="…"` on a form: asks before it is submitted, and stops
  // the submission on a refusal.
  document.addEventListener("submit", function (event) {
    var form = event.target;
    var question = form.getAttribute && form.getAttribute("data-confirm");
    if (question && !window.confirm(question)) event.preventDefault();
  });

  document.addEventListener("change", function (event) {
    var input = event.target;
    if (!input.hasAttribute) return;
    // `data-submit-on-change` (the grocery list's check boxes, the stocks'
    // barcode photo, #402).
    if (input.hasAttribute("data-submit-on-change")) input.form.submit();
    // `data-all-day` (`agenda::new::schedule_fields`, #117).
    else if (input.hasAttribute("data-all-day")) allDay(input);
  });

  // `data-expiry-shortcuts` (`stocks::new::form_fields`, #404): "+3 j",
  // "+1 sem." and "+1 mois" buttons after the date's label, which fill the
  // field from the browser's own day. They exist only here: without
  // JavaScript the field is typed by hand. A month later keeps the day of
  // the month, or takes the month's last one (31 January → 28 or 29
  // February).
  var SHORTCUTS = [["+3 j", 3, 0], ["+1 sem.", 7, 0], ["+1 mois", 0, 1]];

  function expiryShortcuts() {
    var fields = document.querySelectorAll("input[data-expiry-shortcuts]");
    Array.prototype.forEach.call(fields, function (field) {
      var group = document.createElement("div");
      group.className = "actions";
      group.setAttribute("role", "group");
      group.setAttribute("aria-label", "Péremption dans");
      SHORTCUTS.forEach(function (shortcut) {
        var button = document.createElement("button");
        button.type = "button";
        button.className = "secondary sm";
        button.textContent = shortcut[0];
        button.addEventListener("click", function () {
          field.value = later(new Date(), shortcut[1], shortcut[2]);
        });
        group.appendChild(button);
      });
      // After the label, not in it: a label holds one control.
      (closest(field, "label") || field).insertAdjacentElement("afterend", group);
    });
  }

  function later(today, days, months) {
    var y = today.getFullYear();
    var m = today.getMonth() + months;
    var last = new Date(y, m + 1, 0).getDate();
    var d = new Date(y, m, Math.min(today.getDate(), last) + days);
    function pad(n) {
      return (n < 10 ? "0" : "") + n;
    }
    return d.getFullYear() + "-" + pad(d.getMonth() + 1) + "-" + pad(d.getDate());
  }

  // `data-resend-cooldown="<s>"` (`auth::resend_verification::resend_form`,
  // #420): a verification email has just left, and apps/api sends no other
  // for that many seconds. The button waits them out disabled, its label
  // counting down, then takes its own label back. The count is read from the
  // clock, not from the ticks, so a throttled tab does not stretch it.
  // Without JavaScript the button is simply usable.
  function resendCooldowns() {
    var buttons = document.querySelectorAll("button[data-resend-cooldown]");
    Array.prototype.forEach.call(buttons, function (button) {
      var label = button.textContent;
      var end = Date.now() + 1000 * Number(button.getAttribute("data-resend-cooldown"));
      (function tick() {
        var left = Math.ceil((end - Date.now()) / 1000);
        button.disabled = left > 0;
        button.textContent = left > 0 ? "Renvoyer l'email (" + left + " s)" : label;
        if (left > 0) setTimeout(tick, 1000);
      })();
    });
  }

  // Loaded from the <head>: the fields are there once the document is.
  function ready() {
    expiryShortcuts();
    resendCooldowns();
  }
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", ready);
  } else {
    ready();
  }

  // The "Journée entière" box swaps `Début`/`Fin` between `datetime-local`
  // and `date`. Ticking: each value keeps its date, except an end sitting on
  // midnight past the start's day, which becomes the day before — the same
  // reading `normalize_all_day` gives an exclusive end. Unticking: the start
  // opens its day and the end becomes the midnight after its last day, i.e.
  // the instants the date pair stands for. Ticking drops the time of day,
  // so a slot off midnight comes back as whole days (`05 10:00`–`05 11:00`
  // → `05` → `05 00:00`–`06 00:00`, intended). From dates, for a
  // well-ordered pair, the two directions are inverses (`07` → `08 00:00`
  // → `07`). A reversed pair's end moves one day later on each round trip
  // until it reaches the start's day; such a pair is refused on submit
  // anyway (`form_bounds` / `validate_event_form`). Values are
  // read before the type changes, since changing the type sanitizes a value
  // the new type cannot hold down to "". Date arithmetic runs at UTC noon,
  // where no offset can shift the calendar day.
  function allDay(box) {
    var form = box.form;
    var on = box.checked;
    var start = form.elements.starts_at;
    var end = form.elements.ends_at;
    function shift(value, days) {
      var t = new Date(value.slice(0, 10) + "T12:00Z");
      t.setUTCDate(t.getUTCDate() + days);
      return t.toISOString().slice(0, 10);
    }
    [start, end].forEach(function (field) {
      var value = field.value;
      var isEnd = field === end;
      if (on === (field.type === "date")) return;
      if (on) {
        field.type = "date";
        if (value) {
          var back = isEnd && value.slice(11, 16) === "00:00" && value.slice(0, 10) > start.value;
          field.value = shift(value, back ? -1 : 0);
        }
      } else {
        field.type = "datetime-local";
        if (value) field.value = shift(value, isEnd ? 1 : 0) + "T00:00";
      }
    });
  }
})();
