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

  // `data-dialog="<id>"` on a link (`recipes::missing::missing_badge`,
  // #560): opens the `<dialog>` of that id as a modal instead of following
  // the link. The browser moves the focus into the dialog, keeps it there
  // and closes it on Escape; once closed, the focus goes back to the link.
  // Without JavaScript, or without `showModal`, the link leads to a page
  // with the same content.
  document.addEventListener("click", function (event) {
    var link = closest(event.target, "[data-dialog]");
    if (!link) return;
    var dialog = document.getElementById(link.getAttribute("data-dialog"));
    if (!dialog || typeof dialog.showModal !== "function") return;
    event.preventDefault();
    dialog.addEventListener(
      "close",
      function () {
        link.focus();
      },
      { once: true }
    );
    dialog.showModal();
  });

  // `data-viewer="<id>"` on a tile of an event's attachments
  // (`agenda::detail::attachment_tile`, #776): opens the viewer `<dialog>`
  // of that id as a modal instead of following the link to the file. The
  // viewer's media wait in `data-src` until it opens. « Pièce précédente »
  // and « Pièce suivante » (`data-viewer-step`), and the ← and → keys, go
  // to the viewer its `data-viewer-prev` / `data-viewer-next` names. The
  // browser closes it on Escape; the focus then goes back to the tile of
  // the file last shown. Without JavaScript the tile leads to the file.
  function showViewer(dialog) {
    Array.prototype.forEach.call(dialog.querySelectorAll("[data-src]"), function (media) {
      if (!media.hasAttribute("src")) media.setAttribute("src", media.getAttribute("data-src"));
    });
    dialog.showModal();
  }

  function stepViewer(dialog, step) {
    var other = dialog && document.getElementById(dialog.getAttribute("data-viewer-" + step) || "");
    if (!other) return;
    dialog.close();
    showViewer(other);
  }

  document.addEventListener("click", function (event) {
    var tile = closest(event.target, "[data-viewer]");
    if (tile) {
      var dialog = document.getElementById(tile.getAttribute("data-viewer"));
      if (!dialog || typeof dialog.showModal !== "function") return;
      event.preventDefault();
      showViewer(dialog);
      return;
    }
    var button = closest(event.target, "[data-viewer-step]");
    if (button) stepViewer(closest(button, "dialog"), button.getAttribute("data-viewer-step"));
  });

  // Not on a video, whose own controls take the arrows to seek.
  document.addEventListener("keydown", function (event) {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    var dialog = closest(event.target, "dialog.viewer[open]");
    if (!dialog || closest(event.target, "video")) return;
    event.preventDefault();
    stepViewer(dialog, event.key === "ArrowLeft" ? "prev" : "next");
  });

  // A closed viewer stops its video. Its tile gets the focus back unless
  // another viewer has opened in its place (a step).
  function viewers() {
    Array.prototype.forEach.call(document.querySelectorAll("dialog.viewer"), function (dialog) {
      dialog.addEventListener("close", function () {
        Array.prototype.forEach.call(dialog.querySelectorAll("video"), function (video) {
          video.pause();
        });
        if (document.querySelector("dialog.viewer[open]")) return;
        var tile = document.querySelector('[data-viewer="' + dialog.id + '"]');
        if (tile) tile.focus();
      });
    });
  }

  // `data-confirm="…"` on a form: asks before it is submitted, and stops
  // the submission on a refusal. A form already on its way is not sent
  // twice, and its button waits (`busy`).
  document.addEventListener("submit", function (event) {
    var form = event.target;
    if (!form.getAttribute) return;
    if (form.hasAttribute("data-busy")) {
      event.preventDefault();
      return;
    }
    var question = form.getAttribute("data-confirm");
    if (question && !window.confirm(question)) event.preventDefault();
    else {
      keepScroll(form);
      busy(form, event.submitter);
    }
  });

  // A form being sent (#778): every page is a full load, so a slow post —
  // a 20 MB attachment, a poor connection — showed nothing until the next
  // page came, and a second click sent it again. Its button now waits:
  // `aria-busy`, a wheel drawn by the sheet, the label of its
  // `data-busy-label` or « Envoi en cours », then disabled; any other
  // sending of the form is stopped. Disabled a turn later, not here: the
  // browser builds the form's data after this event, and a disabled
  // submitter would drop its own `name=value` (the recipes' « Chercher »,
  // `intent=search`). Not a `<form method="dialog">`, which only closes a
  // dialog, nor a form sent to another window. `form.submit()`
  // (`data-submit-on-change`) fires no `submit` event, so none of this.
  // Without JavaScript the button stays as it is.
  var BUSY_LABEL = "Envoi en cours";

  function busy(form, submitter) {
    var button =
      submitter || form.querySelector('button:not([type]), button[type="submit"], input[type="submit"]');
    var method = (button && button.getAttribute("formmethod")) || form.getAttribute("method") || "";
    var target = (button && button.getAttribute("formtarget")) || form.getAttribute("target") || "";
    if (method.toLowerCase() === "dialog" || (target && target !== "_self")) return;
    form.setAttribute("data-busy", "");
    if (!button) return;
    var label = button.getAttribute("data-busy-label") || BUSY_LABEL;
    button.setAttribute("aria-busy", "true");
    if (button.tagName === "INPUT") {
      button.idleLabel = button.value;
      button.value = label;
    } else {
      button.idleLabel = button.innerHTML;
      button.textContent = label;
    }
    setTimeout(function () {
      if (form.hasAttribute("data-busy")) button.disabled = true;
    }, 0);
  }

  // A page the back button brings out of the browser's cache comes back as
  // it was left, its button waiting for a page that has already come: it is
  // given back its label, and the form can be sent again.
  window.addEventListener("pageshow", function () {
    Array.prototype.forEach.call(document.querySelectorAll("form[data-busy]"), function (form) {
      form.removeAttribute("data-busy");
    });
    Array.prototype.forEach.call(document.querySelectorAll('[aria-busy="true"]'), function (button) {
      if (button.idleLabel === undefined) return;
      if (button.tagName === "INPUT") button.value = button.idleLabel;
      else button.innerHTML = button.idleLabel;
      delete button.idleLabel;
      button.removeAttribute("aria-busy");
      button.disabled = false;
    });
  });

  document.addEventListener("change", function (event) {
    var input = event.target;
    if (!input.hasAttribute) return;
    // `data-submit-on-change` (the stocks' barcode photo, #402; the grocery
    // list's check boxes until #526; the profile's families, #751).
    // `form.submit()` fires no `submit` event, hence
    // the explicit `keepScroll`.
    if (input.hasAttribute("data-submit-on-change")) {
      keepScroll(input.form, input);
      input.form.submit();
    }
    // `data-all-day` (`agenda::new::schedule_fields`, #117).
    else if (input.hasAttribute("data-all-day")) allDay(input);
    // `data-min-of` (`agenda::new::schedule_fields`, #697).
    else if (input.hasAttribute("data-min-of")) bound(input, true);
    // `data-recurring` (`agenda::new::recurrence_picker`, #698).
    else if (input.hasAttribute("data-recurring")) recurring(input);
    // `data-families` (`agenda::calendar::family_selector`, #750).
    else if (input.form && input.form.hasAttribute("data-families")) families(input);
  });

  // `data-families="<id>"` on the agenda's selector of families, `<id>`
  // being the active family: each box sends the form, so « Afficher » is
  // hidden. Ticking « Toutes » ticks every family; unticking it leaves the
  // active family alone; a family box ticks « Toutes » when it completes
  // the set, and unticks it otherwise. The last family ticked cannot be
  // unticked: a modal says so and the box stays ticked, nothing sent.
  // Without JavaScript the boxes wait for « Afficher », as before.
  var ALL_FAMILIES = "toutes";
  var LAST_FAMILY = "Au moins une famille doit rester affichée.";

  function families(box) {
    var form = box.form;
    var all = form.querySelector('input[value="' + ALL_FAMILIES + '"]');
    var boxes = Array.prototype.filter.call(form.querySelectorAll('input[type="checkbox"]'), function (b) {
      return b !== all;
    });
    function ticked(b) {
      return b.checked;
    }
    if (box === all) {
      var active = form.getAttribute("data-families");
      boxes.forEach(function (b) {
        b.checked = all.checked || b.value === active;
      });
      // An active family the selector does not list: the first one stays.
      if (!boxes.some(ticked)) boxes[0].checked = true;
    } else if (!boxes.some(ticked)) {
      box.checked = true;
      lastFamily(box);
      return;
    } else if (all) {
      all.checked = boxes.every(ticked);
    }
    keepScroll(form);
    form.submit();
  }

  // The modal of the last family (DESIGN.md → `dialog`), made the first
  // time, after the form. The browser moves the focus into it and closes
  // it on Escape; the focus then goes back to the box.
  function lastFamily(box) {
    var dialog = document.getElementById("last-family");
    if (!dialog) {
      dialog = document.createElement("dialog");
      dialog.id = "last-family";
      dialog.setAttribute("aria-labelledby", "last-family-text");
      dialog.innerHTML =
        '<p id="last-family-text">' +
        LAST_FAMILY +
        '</p><form method="dialog" class="actions"><button type="submit" class="secondary">Fermer</button></form>';
      box.form.insertAdjacentElement("afterend", dialog);
    }
    if (typeof dialog.showModal !== "function") {
      window.alert(LAST_FAMILY);
      return;
    }
    dialog.addEventListener(
      "close",
      function () {
        box.focus();
      },
      { once: true }
    );
    dialog.showModal();
  }

  // `data-hidden-with-js`: the fallback of a form that sends itself
  // without it (a button: the profile's families, #751), or of what a
  // script offers otherwise (the attachments' list and its « Supprimer »,
  // which the viewers carry, #776). Rendered, then hidden here, so that a
  // page whose script failed to load keeps it.
  function familySelectors() {
    var buttons = document.querySelectorAll(
      'form[data-families] button[type="submit"], [data-hidden-with-js]'
    );
    Array.prototype.forEach.call(buttons, function (button) {
      button.hidden = true;
    });
  }

  // `data-recurring` on the « Événement récurrent » box: the box is shown,
  // and it and its `recurring_box` marker — both rendered `disabled`, so a
  // page without JavaScript sends neither — are enabled; the marker tells
  // the server that an unchecked box means a one-off event; the settings
  // block it names in
  // `aria-controls` is `hidden` — out of the tab order and of the
  // accessibility tree — while it is unchecked, and `aria-expanded` follows.
  // « Aucune » is no longer a frequency to pick, the box says it: checking
  // the box over it selects « Hebdomadaire », the frequency the server
  // takes for a checked box without one. Without JavaScript the box stays
  // hidden and unsent, and every setting shows as it always did.
  function recurring(box) {
    var settings = document.getElementById(box.getAttribute("aria-controls"));
    if (!settings) return;
    var on = box.checked;
    settings.hidden = !on;
    box.setAttribute("aria-expanded", on ? "true" : "false");
    var freq = box.form && box.form.elements.freq;
    if (on && freq && freq.value === "none") freq.value = "weekly";
  }

  function recurringBoxes() {
    var boxes = document.querySelectorAll("input[data-recurring]");
    Array.prototype.forEach.call(boxes, function (box) {
      var wrapper = closest(box, "[data-recurring-box]");
      if (!wrapper || !document.getElementById(box.getAttribute("aria-controls"))) return;
      var marker = wrapper.querySelector('input[name="recurring_box"]');
      if (marker) marker.disabled = false;
      box.disabled = false;
      var none = box.form && box.form.querySelector('select[name="freq"] option[value="none"]');
      if (none) none.disabled = true;
      wrapper.hidden = false;
      recurring(box);
    });
  }

  // `data-min-of="<name>"` on a field (the event's Début, #697): the field
  // of that name in the same form (Fin) gets this one's value as its `min`,
  // so the browser's own picker and validation hold it there. When this
  // field is changed and has passed the other, the other is moved onto it;
  // an end still after the start is left alone (no duration is kept). The
  // `min` takes the other field's shape — `YYYY-MM-DD` for a `date`, a
  // start of day for a `datetime-local` whose start is a bare date — since a
  // `min` the type cannot read is ignored. Same-shaped values compare as
  // strings. Without JavaScript there is no `min`, and the server's
  // `ends_before_starts` stays the only check.
  function bound(field, follow) {
    var other = field.form && field.form.elements[field.getAttribute("data-min-of")];
    if (!other) return;
    var value = field.value;
    var min = !value
      ? ""
      : other.type === "date"
        ? value.slice(0, 10)
        : value.length > 10
          ? value.slice(0, 16)
          : value + "T00:00";
    if (!min) {
      other.removeAttribute("min");
      return;
    }
    other.setAttribute("min", min);
    if (follow && other.value && other.value < min) other.value = min;
  }

  function bounds() {
    var fields = document.querySelectorAll("input[data-min-of]");
    Array.prototype.forEach.call(fields, function (field) {
      bound(field, false);
    });
  }

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
  // the grocery list's tick boxes, where the ticked row also moves down.
  // Only when that page has the sender's path and shows no error: a
  // failed post comes back to the same list with its `.notice.error`
  // banner at the top (`?error=…`), which the reader must see, and a
  // redirect elsewhere opens at the top as usual. Kept in sessionStorage,
  // which can be missing or refuse writes: the page then simply opens at
  // the top. `field`, a radio sent by `data-submit-on-change` (the
  // profile's families, #751): the field of the same name and value gets
  // the focus back, so the arrow keys go on from it after the reload.
  var KEEP_SCROLL = "keep-scroll";

  function keepScroll(form, field) {
    if (!form || !form.hasAttribute("data-keep-scroll")) return;
    var kept = { path: location.pathname, y: window.scrollY };
    if (field && field.type === "radio") kept.focus = [field.name, field.value];
    try {
      sessionStorage.setItem(KEEP_SCROLL, JSON.stringify(kept));
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
      if (kept.focus) {
        var fields = document.getElementsByName(kept.focus[0]);
        for (var i = 0; i < fields.length; i++) {
          if (fields[i].value === kept.focus[1]) fields[i].focus({ preventScroll: true });
        }
      }
    }
  }

  // The phone tab bar (`.tabs`, below 861 px) scrolls sideways when its
  // tabs do not fit — nine of them did not at 390px (#456), the six of
  // #523 do — and every tab is a full page load,
  // so the bar comes back scrolled to its start and a tab near its end is
  // off screen once followed. Brings the current tab to the middle of the
  // bar, instantly; nothing happens when the bar does not overflow.
  function revealCurrentTab() {
    var bar = document.querySelector(".tabs");
    // `:not(.pc)`: on the stocks and the recipes, a PC link is current too,
    // and the phone does not draw it (#523).
    var tab = bar && bar.querySelector('[aria-current="page"]:not(.pc)');
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
    bounds();
    recurringBoxes();
    familySelectors();
    viewers();
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
    // The end's `min` takes the new shape; the box moves no bound itself.
    if (start.hasAttribute("data-min-of")) bound(start, false);
  }
})();
