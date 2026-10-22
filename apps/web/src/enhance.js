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

  // `data-pw-meter` (`app::password_field`, #461): a strength gauge under a
  // password being chosen, made here and only here — without JavaScript
  // there is none, and the form works the same. The score follows apps/api's
  // policy: length first, plus a bonus for variety (lower case, upper case,
  // digits, anything else). Under the field's `minlength` it stays at "trop
  // court" whatever else, so it never contradicts the server; the list of
  // common passwords stays on the server, and the gauge knows nothing of
  // it. The words carry the level, in a polite live region that speaks only
  // when the level changes; the bar and its colour repeat them.
  var STRENGTHS = ["trop court", "faible", "correct", "solide"];

  function strength(value, minimum) {
    var length = Array.from(value).length;
    if (length < minimum) return 0;
    var kinds = [/[a-z]/, /[A-Z]/, /[0-9]/, /[^a-zA-Z0-9]/].filter(function (kind) {
      return kind.test(value);
    }).length;
    var score = length + 2 * (kinds - 1);
    return score < minimum + 4 ? 1 : score < minimum + 8 ? 2 : 3;
  }

  function strengthMeters() {
    var fields = document.querySelectorAll("input[data-pw-meter]");
    Array.prototype.forEach.call(fields, function (field) {
      var meter = document.createElement("div");
      meter.className = "pw-meter";
      var bar = document.createElement("span");
      bar.className = "pw-bar";
      bar.setAttribute("aria-hidden", "true");
      var fill = document.createElement("span");
      bar.appendChild(fill);
      var words = document.createElement("span");
      words.setAttribute("aria-live", "polite");
      meter.appendChild(bar);
      meter.appendChild(words);
      // After the label, not in it: in it, the words would join the
      // field's name.
      (closest(field, "label") || field).insertAdjacentElement("afterend", meter);
      var minimum = Number(field.getAttribute("minlength")) || 0;
      var shown = null;
      field.addEventListener("input", function () {
        var level = field.value ? strength(field.value, minimum) : null;
        if (level === shown) return;
        shown = level;
        if (level === null) meter.removeAttribute("data-level");
        else meter.setAttribute("data-level", level);
        fill.style.width = level === null ? "0" : 25 * (level + 1) + "%";
        words.textContent = level === null ? "" : "Robustesse : " + STRENGTHS[level];
      });
    });
  }

  // `data-confirm="…"` on a form: asks before it is submitted, and stops
  // the submission on a refusal.
  document.addEventListener("submit", function (event) {
    var form = event.target;
    var question = form.getAttribute && form.getAttribute("data-confirm");
    if (question && !window.confirm(question)) event.preventDefault();
    else keepScroll(form);
  });

  document.addEventListener("change", function (event) {
    var input = event.target;
    if (!input.hasAttribute) return;
    // `data-submit-on-change` (the grocery list's check boxes, the stocks'
    // barcode photo, #402). `form.submit()` fires no `submit` event, hence
    // the explicit `keepScroll`.
    if (input.hasAttribute("data-submit-on-change")) {
      keepScroll(input.form);
      input.form.submit();
    }
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

  // `data-keep-scroll` on a form (#456): the page its redirect lands on
  // opens at the height the form was sent from, rather than at the top —
  // the grocery list's check boxes, where the ticked row also moves down.
  // Only when that page has the sender's path and shows no error: a
  // failed post comes back to the same list with its `.notice.error`
  // banner at the top (`?error=…`), which the reader must see, and a
  // redirect elsewhere opens at the top as usual. Kept in sessionStorage,
  // which can be missing or refuse writes: the page then simply opens at
  // the top.
  var KEEP_SCROLL = "keep-scroll";

  function keepScroll(form) {
    if (!form || !form.hasAttribute("data-keep-scroll")) return;
    try {
      sessionStorage.setItem(KEEP_SCROLL, JSON.stringify({ path: location.pathname, y: window.scrollY }));
    } catch (e) {}
  }

  // "instant" whatever the sheet's `scroll-behavior`: a restored position
  // is where the reader already was, not a move to animate.
  function restoreScroll() {
    var kept = null;
    try {
      kept = JSON.parse(sessionStorage.getItem(KEEP_SCROLL));
      sessionStorage.removeItem(KEEP_SCROLL);
    } catch (e) {}
    if (document.querySelector(".notice.error")) return;
    if (kept && kept.path === location.pathname && typeof kept.y === "number") {
      window.scrollTo({ top: kept.y, left: 0, behavior: "instant" });
    }
  }

  // The phone tab bar (`.tabs`, below 861 px) scrolls sideways and every
  // tab is a full page load, so the bar comes back scrolled to its start
  // and a tab near its end — Messagerie — is off screen once followed
  // (#456). Brings the current tab to the middle of the bar, instantly;
  // nothing happens when the bar does not overflow (the sidebar).
  function revealCurrentTab() {
    var bar = document.querySelector(".tabs");
    var tab = bar && bar.querySelector('[aria-current="page"]');
    if (!tab || bar.scrollWidth <= bar.clientWidth) return;
    var barBox = bar.getBoundingClientRect();
    var tabBox = tab.getBoundingClientRect();
    var offset = tabBox.left + tabBox.width / 2 - (barBox.left + barBox.width / 2);
    bar.scrollTo({ left: bar.scrollLeft + offset, top: 0, behavior: "instant" });
  }

  // Loaded from the <head>: the fields are there once the document is. The
  // script comes after the sheet's <link>, so the layout the scroll
  // positions are read from is the styled one. The two scroll fixes run
  // apart: one throwing (a browser refusing `behavior: "instant"`) must not
  // cost the other.
  function ready() {
    expiryShortcuts();
    resendCooldowns();
    strengthMeters();
    try {
      revealCurrentTab();
    } catch (e) {}
    try {
      restoreScroll();
    } catch (e) {}
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
