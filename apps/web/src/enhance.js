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

  // `data-count="<id>"` on a field (`recipes::outcome::taste_fieldset`,
  // #822): the hint of that id says how many characters are typed, as the
  // page writes it — « 200 caractères au plus · 40 saisis » —, counted once
  // trimmed and by code point. `value` holds each line break as one `\n`,
  // as `maxlength` counts it; the form posts it as `\r\n`, which apps/web
  // and apps/api count as one too, so the three agree. Not a live region:
  // the hint is the field's description, read with it. Without JavaScript
  // the count is the one the page was shown with.
  document.addEventListener("input", function (event) {
    var field = event.target;
    if (!field.hasAttribute || !field.hasAttribute("data-count")) return;
    var hint = document.getElementById(field.getAttribute("data-count"));
    if (!hint) return;
    var typed = Array.from(field.value.trim()).length;
    var bound = hint.textContent.split(" · ")[0];
    hint.textContent = typed ? bound + " · " + typed + " saisis" : bound;
  });

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

  // `data-pick` (`agenda::pick::pick_button`, #795): « Choisir dans le
  // calendrier » opens the choice in a `<dialog>` instead of sending the
  // form to the choice page: that page's `[data-pick-body]`, read from the
  // button's `formaction` (`GET`, `dialogue=1`, with the day asked and the
  // form's Début). A day or a month reads it again, the arrows move through
  // the days. A half hour fills Début, and Fin as long after it as the form
  // already said — an hour when it said nothing usable —, counted by the
  // clock; an entry of the banner ticks « Journée entière » on that day.
  // Both fields wear `picked` until changed. Escape, the cross and
  // « Fermer » close it untouched; the focus goes back to the button.
  // Without `showModal`, `fetch`, or an answer, the form goes to the page.
  var picking = null;

  function clock(value) {
    var m = /^(\d{4})-(\d\d)-(\d\d)T(\d\d):(\d\d)$/.exec(value || "");
    return m ? Date.UTC(+m[1], m[2] - 1, +m[3], +m[4], +m[5]) / 60000 : NaN;
  }

  function stamp(minutes) {
    return new Date(minutes * 60000).toISOString().slice(0, 16);
  }

  function pickLength(form) {
    var start = form.elements.starts_at;
    var length = start.type === "date" ? NaN : clock(form.elements.ends_at.value) - clock(start.value);
    return length > 0 ? length : 60;
  }

  // « 1 h », « 1 h 30 », « 45 min ».
  function lengthWords(minutes) {
    var hours = Math.floor(minutes / 60);
    var rest = minutes % 60;
    if (!hours) return rest + " min";
    return hours + " h" + (rest ? " " + (rest < 10 ? "0" : "") + rest : "");
  }

  function choose(form, value) {
    var box = form.elements.all_day;
    var start = form.elements.starts_at;
    var end = form.elements.ends_at;
    var whole = value.length === 10;
    var length = pickLength(form);
    if (box && box.checked !== whole) {
      box.checked = whole;
      allDay(box);
    }
    start.value = value;
    end.value = whole ? value : stamp(clock(value) + length);
    start.classList.add("picked");
    end.classList.add("picked");
    if (start.hasAttribute("data-min-of")) bound(start, false);
  }

  function loadPick(day) {
    var form = picking.form;
    var url = picking.url + "&dialogue=1&debut=" + encodeURIComponent(form.elements.starts_at.value) + (day ? "&jour=" + day : "");
    return fetch(url, { credentials: "same-origin" })
      .then(function (answer) {
        if (!answer.ok) throw new Error(answer.status);
        return answer.text();
      })
      .then(function (html) {
        if (!picking) return;
        var body = new DOMParser().parseFromString(html, "text/html").querySelector("[data-pick-body]");
        if (!body) throw new Error("no choice");
        picking.dialog.replaceChildren(document.adoptNode(body));
        Array.prototype.forEach.call(picking.dialog.querySelectorAll(".mm button"), function (d) {
          d.tabIndex = d.getAttribute("aria-pressed") === "true" ? 0 : -1;
        });
        // At rest, the line says the length a half hour will be given.
        var status = picking.dialog.querySelector("[data-pick-status]");
        if (status) status.textContent = status.textContent.replace(/\.$/, " (" + lengthWords(pickLength(form)) + ").");
        if (picking.dialog.open) morningOf(picking.dialog);
      });
  }

  // The hours open on 08:00, as the day view's do (`morningHours`).
  function morningOf(dialog) {
    var hours = dialog.querySelector(".hours-scroll");
    var eight = dialog.querySelector("#h8");
    if (hours && eight) hours.scrollTop += eight.getBoundingClientRect().top - hours.getBoundingClientRect().top;
  }

  function openPick(button) {
    var dialog = document.getElementById("pick");
    if (!dialog) {
      dialog = document.createElement("dialog");
      dialog.id = "pick";
      dialog.setAttribute("aria-label", "Choisir dans le calendrier");
      document.body.appendChild(dialog);
      dialog.addEventListener("close", function () {
        if (!picking) return;
        picking.button.focus();
        picking = null;
      });
    }
    picking = { button: button, form: button.form, dialog: dialog, url: button.formAction };
    loadPick("").then(
      function () {
        dialog.showModal();
        morningOf(dialog);
      },
      function () {
        picking = null;
        button.removeAttribute("data-pick");
        button.click();
      }
    );
  }

  // A button of the choice: a day or a month reads the choice again, the
  // focus kept on what was pressed, a day bringing its recap into view;
  // `choix` closes, filling the form first when it holds a value.
  function pickButton(button) {
    var name = button.getAttribute("name");
    if (name === "jour") {
      var label = button.getAttribute("aria-label");
      loadPick(button.value).then(function () {
        if (!picking) return;
        var again = label
          ? picking.dialog.querySelector('[aria-label="' + label + '"]')
          : picking.dialog.querySelector('.mm [aria-pressed="true"]');
        if (again) again.focus();
        var recap = !label && picking.dialog.querySelector("[data-pick-day]");
        if (recap) recap.scrollIntoView({ block: "nearest" });
      });
    } else if (name === "choix") {
      if (button.value) choose(picking.form, button.value);
      picking.dialog.close();
    }
  }

  document.addEventListener("click", function (event) {
    var button = closest(event.target, "[data-pick]");
    if (button) {
      if (typeof HTMLDialogElement !== "function" || !window.fetch || !button.form) return;
      event.preventDefault();
      openPick(button);
      return;
    }
    var inside = picking && closest(event.target, "#pick button");
    if (inside) {
      event.preventDefault();
      pickButton(inside);
    }
  });

  // ← → a day, ↑ ↓ a week, Début and Fin the ends of the week, Page up and
  // Page down a month, the same day or the month's last. A day of another
  // month reads that month.
  document.addEventListener("keydown", function (event) {
    var day = picking && closest(event.target, "#pick .mm button");
    if (!day) return;
    var date = new Date(day.value + "T12:00Z");
    var weekday = (date.getUTCDay() + 6) % 7;
    var step = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -7, ArrowDown: 7, Home: -weekday, End: 6 - weekday }[event.key];
    var months = { PageUp: -1, PageDown: 1 }[event.key];
    if (step === undefined && months === undefined) return;
    event.preventDefault();
    if (months) {
      var dayOfMonth = date.getUTCDate();
      date.setUTCDate(1);
      date.setUTCMonth(date.getUTCMonth() + months);
      var last = new Date(Date.UTC(date.getUTCFullYear(), date.getUTCMonth() + 1, 0)).getUTCDate();
      date.setUTCDate(Math.min(dayOfMonth, last));
    } else {
      date.setUTCDate(date.getUTCDate() + step);
    }
    var value = date.toISOString().slice(0, 10);
    var target = picking.dialog.querySelector('.mm button[value="' + value + '"]');
    if (target) {
      day.tabIndex = -1;
      target.tabIndex = 0;
      target.focus();
      return;
    }
    loadPick(value).then(function () {
      if (!picking) return;
      var chosen = picking.dialog.querySelector('.mm [aria-pressed="true"]');
      if (chosen) chosen.focus();
    });
  });

  // Over a half hour, what it would give: the line under the hours says
  // it, and what it overlaps of the day's blocks (`data-span`, minutes
  // since the day opened) — « libre » otherwise; a frame (`ghost`) draws
  // it over the hours' column.
  function preview(event) {
    var slot = picking && closest(event.target, "#pick .slots button");
    if (!slot) return;
    var length = pickLength(picking.form);
    var span = Array.prototype.indexOf.call(slot.parentNode.children, slot) * 30;
    var from = slot.value.slice(11);
    var to = stamp(clock(slot.value) + length).slice(11);
    var clash = [];
    Array.prototype.forEach.call(picking.dialog.querySelectorAll("[data-span]"), function (block) {
      var at = block.getAttribute("data-span").split(" ");
      if (+at[0] < span + length && Math.max(+at[1], +at[0] + 1) > span) clash.push(block.querySelector("strong").textContent);
    });
    var status = picking.dialog.querySelector("[data-pick-status]");
    if (status) {
      // The line never gets shorter while the day is shown: below the
      // hours, a line that lost one would pull them down under a pointer
      // at the bottom of the scrolled body, onto another half hour, whose
      // line could be longer again — the preview would swing between two.
      var tall = function () {
        status.style.minHeight = Math.max(parseFloat(status.style.minHeight) || 0, status.offsetHeight) + "px";
      };
      tall();
      var said = document.createElement("b");
      said.textContent = "Début " + from + " · Fin " + to;
      var rest = document.createElement("span");
      if (clash.length) rest.className = "clash";
      rest.textContent = clash.length ? "Chevauche : " + clash.join(", ") : "libre";
      status.replaceChildren(said, " · ", rest);
      tall();
    }
    var scroller = slot.parentNode.parentNode;
    var ghost = scroller.querySelector(".ghost");
    if (!ghost) {
      ghost = document.createElement("span");
      ghost.className = "ghost";
      scroller.appendChild(ghost);
    }
    var label = scroller.querySelector("th");
    ghost.style.left = (label ? label.offsetWidth : 0) + "px";
    ghost.style.top = slot.parentNode.offsetTop + slot.offsetTop + "px";
    ghost.style.height = (slot.offsetHeight * length) / 30 + "px";
    ghost.textContent = "Nouvel événement " + from + "–" + to;
  }
  document.addEventListener("mouseover", preview);
  document.addEventListener("focusin", preview);

  document.addEventListener("input", function (event) {
    var field = event.target;
    if (field.classList && field.classList.contains("picked")) field.classList.remove("picked");
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
  // Without JavaScript the button stays as it is. A button named by its
  // `aria-label` (an icon, the list's tick box) gets no label: its box
  // would not hold the text, and a visible label its name lacks fails
  // WCAG 2.5.3. It keeps its content and its name; the sheet lays the
  // wheel over it.
  var BUSY_LABEL = "Envoi en cours";
  var SAVING = "Enregistrement…";

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
    } else if (button.hasAttribute("aria-label")) {
      button.idleLabel = button.innerHTML;
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
      // A form of switches locked while it was sent (#853): unlocked.
      if (button.hasAttribute("data-lock-on-change")) {
        button.removeAttribute("aria-busy");
        Array.prototype.forEach.call(button.querySelectorAll("[data-submit-on-change]"), function (field) {
          field.disabled = false;
        });
        // And its fieldset's line, emptied (#867).
        Array.prototype.forEach.call(button.querySelectorAll("[data-busy-status]"), function (said) {
          said.textContent = "";
          said.parentNode.removeAttribute("aria-busy");
        });
        return;
      }
      // A number locked while it was sent (#846): unlocked as it was.
      if (button.type === "number") {
        button.removeAttribute("aria-busy");
        button.disabled = false;
        return;
      }
      if (button.idleLabel === undefined) return;
      if (button.tagName === "INPUT") button.value = button.idleLabel;
      else button.innerHTML = button.idleLabel;
      delete button.idleLabel;
      button.removeAttribute("aria-busy");
      button.disabled = false;
    });
  });

  // A notification opened (#861) is read from then on, and the count every
  // page's navigation wears goes down — but a page the back button brings
  // out of the back/forward cache comes back as it was left, that count
  // and the list of `/notifications` the old ones. Such a page, shown
  // before the last notification was opened in this tab, is fetched again.
  // `/notifications` itself is also sent `no-store`, for the browsers that
  // keep no such cache and would take it from their HTTP cache instead.
  // The time of the opening is kept in sessionStorage, which can be missing
  // or refuse writes: the page then simply comes back as it was.
  var NOTIFICATION_OPENED = "notification-opened";
  var shownAt = Date.now();

  document.addEventListener("submit", function (event) {
    var action = event.target.getAttribute && event.target.getAttribute("action");
    if (!action || !/^\/notifications\/[^/]+\/open$/.test(action)) return;
    try {
      sessionStorage.setItem(NOTIFICATION_OPENED, String(Date.now()));
    } catch (e) {}
  });

  window.addEventListener("pageshow", function (event) {
    if (!event.persisted) return;
    var opened = 0;
    try {
      opened = Number(sessionStorage.getItem(NOTIFICATION_OPENED)) || 0;
    } catch (e) {}
    if (opened >= shownAt && document.querySelector('a[href="/notifications"]')) location.reload();
  });

  document.addEventListener("change", function (event) {
    var input = event.target;
    if (!input.hasAttribute) return;
    // `data-submit-on-change` (the stocks' barcode photo, #402; the grocery
    // list's check boxes until #526; the profile's families, #751; the
    // theme, the family's colour and calendar, #844).
    // `form.submit()` fires no `submit` event, hence
    // the explicit `keepScroll`.
    if (input.hasAttribute("data-submit-on-change")) {
      keepScroll(input.form, input);
      input.form.submit();
      // A number (a planned recipe's people, #846) is locked until the
      // page comes back: Chrome sends one `change` per click of its
      // arrows, and a value typed meanwhile would be lost. Disabled after
      // `submit()`, which has already read the form's data.
      if (input.type === "number") {
        input.setAttribute("aria-busy", "true");
        input.disabled = true;
      }
      // `data-lock-on-change` (the notification settings' switches,
      // #853): the whole form waits, each field sent on change disabled,
      // so that a second switch is not flipped while the first is saved.
      if (input.form.hasAttribute("data-lock-on-change")) {
        input.form.setAttribute("aria-busy", "true");
        Array.prototype.forEach.call(input.form.querySelectorAll("[data-submit-on-change]"), function (field) {
          field.disabled = true;
        });
        // The family's thresholds (#867): the fieldset of the radio says
        // the choice is on its way, in its live `data-busy-status` line.
        var group = input.closest("fieldset");
        var said = group && group.querySelector("[data-busy-status]");
        if (said) {
          group.setAttribute("aria-busy", "true");
          said.textContent = SAVING;
        }
      }
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

  // `data-combo` (`stocks::new::combo_field`, #803): the ▾ beside a free
  // field that has a datalist. Phones show a datalist only on a touch
  // (Android) or as one types (iPhone); the ▾ shows it all, at will. The
  // button and its list (`aria-controls`) are rendered `hidden` and empty:
  // here the list is filled from the datalist — its one source, a value's
  // `data-note` written beside it — and the button shown. Without
  // JavaScript the field keeps its datalist alone. Open, the focus is on
  // the list: ↓ ↑ Début Fin move, Entrée or Espace picks, Échap closes and
  // gives the focus back to ▾, Tab or a click elsewhere closes. A pick fills
  // the field, closes, and puts the focus in the field.
  function comboOf(node) {
    var box = closest(node, ".combo");
    if (!box) return null;
    var button = box.querySelector("button[data-combo]");
    var list = button && document.getElementById(button.getAttribute("aria-controls"));
    var input = box.querySelector("input");
    return list && input ? { button: button, list: list, input: input } : null;
  }

  function combos() {
    var buttons = document.querySelectorAll("button[data-combo]");
    Array.prototype.forEach.call(buttons, function (button) {
      var combo = comboOf(button);
      var source = combo && combo.input.list;
      if (!source || combo.list.children.length) return;
      Array.prototype.forEach.call(source.options, function (option, i) {
        var item = document.createElement("li");
        item.id = combo.list.id + "-" + i;
        item.setAttribute("role", "option");
        item.setAttribute("data-value", option.value);
        item.textContent = option.value;
        var note = option.getAttribute("data-note");
        if (note) {
          // The space keeps the two words apart in the option's name.
          item.appendChild(document.createTextNode(" "));
          var span = document.createElement("span");
          span.className = "muted";
          span.textContent = note;
          item.appendChild(span);
        }
        combo.list.appendChild(item);
      });
      combo.list.tabIndex = -1;
      button.hidden = false;
    });
  }

  function comboOn(combo, item) {
    Array.prototype.forEach.call(combo.list.children, function (other) {
      other.classList.toggle("cur", other === item);
    });
    if (!item) return;
    combo.list.setAttribute("aria-activedescendant", item.id);
    if (item.scrollIntoView) item.scrollIntoView({ block: "nearest" });
  }

  function closeCombo(combo) {
    if (combo.list.hidden) return;
    combo.list.hidden = true;
    combo.button.setAttribute("aria-expanded", "false");
    combo.list.removeAttribute("aria-activedescendant");
  }

  function closeCombos(except) {
    var open = document.querySelectorAll('button[data-combo][aria-expanded="true"]');
    Array.prototype.forEach.call(open, function (button) {
      var combo = comboOf(button);
      if (combo && combo.list !== except) closeCombo(combo);
    });
  }

  function openCombo(combo) {
    closeCombos(combo.list);
    var value = combo.input.value.trim();
    var current = null;
    Array.prototype.forEach.call(combo.list.children, function (item) {
      var on = item.getAttribute("data-value") === value;
      item.setAttribute("aria-selected", on ? "true" : "false");
      if (on) current = item;
    });
    combo.list.hidden = false;
    combo.button.setAttribute("aria-expanded", "true");
    comboOn(combo, current || combo.list.firstElementChild);
    combo.list.focus();
  }

  function pickCombo(combo, item) {
    combo.input.value = item.getAttribute("data-value");
    combo.input.dispatchEvent(new Event("input", { bubbles: true }));
    combo.input.dispatchEvent(new Event("change", { bubbles: true }));
    closeCombo(combo);
    combo.input.focus();
  }

  document.addEventListener("click", function (event) {
    var button = closest(event.target, "button[data-combo]");
    var item = closest(event.target, ".combo-list [role=option]");
    var combo = comboOf(button || item);
    if (button && combo) {
      if (combo.list.hidden) openCombo(combo);
      else closeCombo(combo);
    } else if (item && combo) {
      pickCombo(combo, item);
    } else {
      closeCombos(null);
    }
  });

  document.addEventListener("keydown", function (event) {
    var target = event.target;
    var combo = target.matches && target.matches(".combo-list") && comboOf(target);
    if (!combo) {
      // ↓ on a closed ▾ opens it, as a native list would.
      combo = target.matches && target.matches("button[data-combo]") && comboOf(target);
      if (combo && event.key === "ArrowDown" && combo.list.hidden) {
        event.preventDefault();
        openCombo(combo);
      }
      return;
    }
    var items = combo.list.children;
    var at = Array.prototype.indexOf.call(items, combo.list.querySelector(".cur"));
    var next = null;
    if (event.key === "ArrowDown") next = items[(at + 1) % items.length];
    else if (event.key === "ArrowUp") next = items[(at - 1 + items.length) % items.length];
    else if (event.key === "Home") next = items[0];
    else if (event.key === "End") next = items[items.length - 1];
    else if ((event.key === "Enter" || event.key === " ") && at >= 0) {
      event.preventDefault();
      pickCombo(combo, items[at]);
      return;
    } else if (event.key === "Escape") {
      event.preventDefault();
      closeCombo(combo);
      combo.button.focus();
      return;
    } else if (event.key === "Tab") {
      closeCombo(combo);
      return;
    } else return;
    event.preventDefault();
    comboOn(combo, next);
  });

  // `data-hidden-with-js`: the fallback of a form that sends itself
  // without it (a button: the profile's families, #751), or of what a
  // script offers otherwise (the attachments' list and its « Supprimer »,
  // which the viewers carry, #776). Rendered, then hidden here, so that a
  // page whose script failed to load keeps it. `data-shown-with-js`, the
  // reverse: rendered `hidden`, shown here — the sentence that says a
  // setting applies on the click (`app::APPLIES_ON_CHANGE`, #844).
  function familySelectors() {
    var buttons = document.querySelectorAll(
      'form[data-families] button[type="submit"], [data-hidden-with-js]'
    );
    Array.prototype.forEach.call(buttons, function (button) {
      button.hidden = true;
    });
    Array.prototype.forEach.call(document.querySelectorAll("[data-shown-with-js]"), function (shown) {
      shown.hidden = false;
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
        button.setAttribute("data-expiry-shortcut", "");
        button.textContent = shortcut[0];
        button.addEventListener("click", function () {
          field.value = later(new Date(), shortcut[1], shortcut[2]);
        });
        group.appendChild(button);
      });
      // After the label, not in it: a label holds one control.
      (closest(field, "label") || field).insertAdjacentElement("afterend", group);
      noExpiry(field, group);
    });
  }

  // "Pas de date de péremption" (#804): checked, the date field is emptied
  // and disabled, its shortcuts too, and the note saying where a proposed
  // date came from is hidden; unchecked, the field gets back the date it
  // held. The server ignores the field when the box is checked, so the
  // script only shows what will be saved.
  function noExpiry(field, group) {
    var box = field.form && field.form.querySelector('input[name="no_expiry"]');
    if (!box) return;
    var label = closest(field, "label");
    var notes = label ? label.querySelectorAll("[data-expiry-note]") : [];
    var buttons = group.querySelectorAll("button");
    var kept = "";
    function sync() {
      if (box.checked) {
        kept = field.value || kept;
        field.value = "";
      } else if (kept) {
        field.value = kept;
      }
      field.disabled = box.checked;
      Array.prototype.forEach.call(buttons, function (button) {
        button.disabled = box.checked;
      });
      Array.prototype.forEach.call(notes, function (note) {
        note.hidden = box.checked;
      });
    }
    box.addEventListener("change", sync);
    sync();
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
  // the top. `field`, a radio or a box sent by `data-submit-on-change`
  // (the profile's families, #751; the theme, the family's colour and
  // calendar, #844): the field of the same name and value gets the focus
  // back, so the arrow keys go on from it after the reload. A number (a
  // planned recipe's people, #846) is found again by its form's action, a
  // list holding one such field per row, whose value has changed.
  var KEEP_SCROLL = "keep-scroll";

  function keepScroll(form, field) {
    if (!form || !form.hasAttribute("data-keep-scroll")) return;
    var kept = { path: location.pathname, y: window.scrollY };
    if (field && (field.type === "radio" || field.type === "checkbox")) kept.focus = [field.name, field.value];
    else if (field && field.type === "number") kept.focusIn = [form.getAttribute("action"), field.name];
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
      if (kept.focusIn) {
        var forms = document.querySelectorAll("form[action]");
        for (var j = 0; j < forms.length; j++) {
          if (forms[j].getAttribute("action") !== kept.focusIn[0]) continue;
          var named = forms[j].elements.namedItem(kept.focusIn[1]);
          if (named && named.focus) named.focus({ preventScroll: true });
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

  // The first questions about meals (#820, `routes::recipes::start`), once
  // per member: the recipe list (`data-depart`, the member's id) opens them
  // until the member has finished them (`data-depart-done` on the end's
  // card, read when it loads) or put them off (the same attribute on
  // « Plus tard », read when it is followed). Seen is kept in localStorage;
  // where it is missing or refuses writes, in a cookie for the session.
  // Without JavaScript nothing opens them: they are never forced.
  var MEAL_START = "mhome.depart-repas.";
  var MEAL_START_COOKIE = "depart_repas";

  function mealStartSeen(user) {
    try {
      if (localStorage.getItem(MEAL_START + user)) return true;
    } catch (e) {}
    return document.cookie.split("; ").indexOf(MEAL_START_COOKIE + "=" + user) !== -1;
  }

  function mealStartDone(user) {
    try {
      localStorage.setItem(MEAL_START + user, "1");
      return;
    } catch (e) {}
    document.cookie = MEAL_START_COOKIE + "=" + user + "; Path=/; SameSite=Lax";
  }

  document.addEventListener("click", function (event) {
    var link = closest(event.target, "a[data-depart-done]");
    if (link) mealStartDone(link.getAttribute("data-depart-done"));
  });

  function mealStart() {
    var done = document.querySelector("[data-depart-done]:not(a)");
    if (done) mealStartDone(done.getAttribute("data-depart-done"));
    var list = document.querySelector("[data-depart]");
    if (list && !mealStartSeen(list.getAttribute("data-depart"))) location.replace("/recipes/depart");
  }

  // The day view's hours (#793) open on 08:00: the sheet's
  // `scroll-initial-target` does it without a script where the browser
  // knows it, and this where it does not. Not over a position the reader
  // or the browser has already scrolled to.
  function morningHours() {
    var hours = document.querySelector(".hours-scroll");
    var eight = document.getElementById("h8");
    if (!hours || !eight || hours.scrollTop !== 0) return;
    if (window.CSS && CSS.supports && CSS.supports("scroll-initial-target", "nearest")) return;
    hours.scrollTop = eight.getBoundingClientRect().top - hours.getBoundingClientRect().top;
  }

  // `data-cook="<recipe id>"` (`recipes::cook`, #823): cooking a recipe one
  // step at a time. The readable list the page shows without JavaScript
  // (`data-cook-plain`) gives way to the stepper the server wrote hidden
  // (`data-cook-ui`): Précédent and Suivant, ← and →, a swipe on the step;
  // « Minuteur 8 min » starts a timer (`data-cook-start`, its seconds),
  // several may run at once. A timer that ends rings — a beep made by Web
  // Audio, no file, and a vibration where there is one — until « Arrêter »;
  // never a notification. The step shown and the timers, by the time they
  // end, are kept per recipe in localStorage (`mhome.cuisine.<id>`) so that
  // leaving the page and coming back finds them; « Terminer » forgets them.
  // So does half a day counted from the later of the last change (`at`,
  // written on a step moved or a timer started, paused, stopped or ended —
  // not on a visit) and the end of the last timer; a timer still running or
  // paused is never forgotten. Storage that throws is left
  // alone: the page works the same, it only forgets. The screen is kept on
  // (Screen Wake Lock) while the page is shown, and released when it is
  // left; without the API, nothing is said and nothing breaks.
  var COOK = "mhome.cuisine.";
  var COOK_KEPT = 12 * 3600 * 1000;
  var cooking = null;
  var audio = null;
  var ringing = null;
  var wakeLock = null;

  function cookSave() {
    if (cooking.over) return;
    try {
      localStorage.setItem(COOK + cooking.id, JSON.stringify({ at: Date.now(), step: cooking.step, timers: cooking.timers }));
    } catch (e) {}
  }

  // When the half day starts for what was kept: its last change or its
  // last timer's end, whichever is later — never, while a timer runs or
  // waits in pause.
  function cookSince(kept) {
    var since = Number(kept.at) || 0;
    (Array.isArray(kept.timers) ? kept.timers : []).forEach(function (timer) {
      if (!timer) return;
      if (!timer.done && !(timer.end > 0) && timer.left > 0) since = Infinity;
      else if (timer.end > since) since = timer.end;
    });
    return since;
  }

  function cookKept(id) {
    try {
      var kept = JSON.parse(localStorage.getItem(COOK + id));
      if (kept && Date.now() - cookSince(kept) < COOK_KEPT) return kept;
      if (kept) localStorage.removeItem(COOK + id);
    } catch (e) {}
    return {};
  }

  // What a timer still has to run, in milliseconds: from its end while it
  // runs, as it was left while it is paused.
  function remaining(timer) {
    return timer.end ? timer.end - Date.now() : timer.left;
  }

  // « 07:52 », « 1:29:05 ».
  function clockOf(ms) {
    var s = Math.max(0, Math.ceil(ms / 1000));
    var h = Math.floor(s / 3600);
    var two = function (n) {
      return (n < 10 ? "0" : "") + n;
    };
    var rest = two(Math.floor((s % 3600) / 60)) + ":" + two(s % 60);
    return h ? h + ":" + rest : rest;
  }

  // `quiet` when the page opens: showing the step kept is no change.
  function cookShow(index, quiet) {
    var n = cooking.steps.length;
    cooking.step = Math.max(0, Math.min(n - 1, index));
    cooking.steps.forEach(function (step, i) {
      step.hidden = i !== cooking.step;
    });
    cooking.at.textContent = "Étape " + (cooking.step + 1) + " sur " + n;
    cooking.gauge.value = cooking.step + 1;
    var last = cooking.step === n - 1;
    // The focus leaves a button about to vanish or be disabled for the one
    // that takes its place.
    var focused = document.activeElement;
    cooking.prev.disabled = cooking.step === 0;
    cooking.next.hidden = last;
    cooking.end.hidden = !last;
    if (focused === cooking.next && last) cooking.end.focus();
    if (focused === cooking.prev && cooking.prev.disabled) cooking.next.focus();
    if (!quiet) cookSave();
  }

  // The rows of the timers, written again from `cooking.timers`; the focus
  // stays on the control it was on, in the same row.
  function cookRows() {
    var focused = document.activeElement;
    var keep = closest(focused, "[data-timer]") && focused.getAttribute("data-cook-key");
    var keepRow = keep && closest(focused, "[data-timer]").getAttribute("data-timer");
    var rows = cooking.timers.map(function (timer) {
      var row = cooking.row.content.firstElementChild.cloneNode(true);
      var what = row.querySelector(".what");
      var left = row.querySelector(".left");
      var pause = row.querySelector("[data-cook-pause]");
      var stop = row.querySelector("[data-cook-stop]");
      var ring = row.querySelector("[data-cook-ring]");
      pause.setAttribute("data-cook-key", "pause");
      stop.setAttribute("data-cook-key", "stop");
      ring.setAttribute("data-cook-key", "ring");
      row.setAttribute("data-timer", timer.id);
      if (timer.done) {
        row.classList.add("ended");
        row.setAttribute("role", "alert");
        what.textContent = timer.what;
        left.removeAttribute("role");
        left.textContent = "Terminé !";
        pause.hidden = true;
        stop.hidden = true;
      } else {
        var paused = !timer.end;
        row.classList.toggle("paused", paused);
        what.textContent = timer.what + (paused ? " · en pause" : "");
        left.setAttribute("aria-label", timer.what);
        left.textContent = clockOf(remaining(timer));
        pause.textContent = paused ? "Reprendre" : "Pause";
        stop.setAttribute("aria-label", "Arrêter le minuteur : " + timer.what);
        ring.hidden = true;
      }
      return row;
    });
    cooking.list.replaceChildren.apply(cooking.list, rows);
    cooking.box.hidden = !rows.length;
    var again = keepRow && cooking.list.querySelector('[data-timer="' + keepRow + '"] [data-cook-key="' + keep + '"]');
    if (again && !again.hidden) again.focus();
    else if (keepRow) (cooking.next.hidden ? cooking.end : cooking.next).focus();
  }

  function beep() {
    try {
      if (!audio) {
        var Context = window.AudioContext || window.webkitAudioContext;
        if (!Context) return;
        audio = new Context();
      }
      if (audio.state === "suspended") audio.resume();
      [0, 0.3, 0.6].forEach(function (offset) {
        var at = audio.currentTime + offset;
        var tone = audio.createOscillator();
        var gain = audio.createGain();
        tone.frequency.value = 880;
        gain.gain.setValueAtTime(0.0001, at);
        gain.gain.exponentialRampToValueAtTime(0.4, at + 0.02);
        gain.gain.exponentialRampToValueAtTime(0.0001, at + 0.2);
        tone.connect(gain);
        gain.connect(audio.destination);
        tone.start(at);
        tone.stop(at + 0.22);
      });
    } catch (e) {}
    try {
      if (navigator.vibrate) navigator.vibrate([200, 100, 200]);
    } catch (e) {}
  }

  // Rings every two seconds while a timer is over and not yet stopped.
  function cookRing() {
    var over = cooking.timers.some(function (timer) {
      return timer.done;
    });
    if (over && !ringing) {
      beep();
      ringing = setInterval(beep, 2000);
    } else if (!over && ringing) {
      clearInterval(ringing);
      ringing = null;
    }
  }

  function cookTick() {
    var ended = false;
    cooking.timers.forEach(function (timer) {
      if (!timer.done && timer.end && remaining(timer) <= 0) {
        timer.done = true;
        ended = true;
      }
    });
    if (ended) {
      cookRows();
      cookSave();
    } else {
      Array.prototype.forEach.call(cooking.list.querySelectorAll("[data-timer]"), function (row, i) {
        var timer = cooking.timers[i];
        if (timer && !timer.done) row.querySelector(".left").textContent = clockOf(remaining(timer));
      });
    }
    cookRing();
  }

  // `wakeLock` is the lock held, `true` while one is asked for.
  function cookWake() {
    if (!cooking || wakeLock || document.visibilityState !== "visible" || !navigator.wakeLock) return;
    wakeLock = true;
    try {
      navigator.wakeLock.request("screen").then(
        function (lock) {
          wakeLock = lock;
          cooking.wake.hidden = false;
          lock.addEventListener("release", function () {
            if (wakeLock === lock) wakeLock = null;
            cooking.wake.hidden = true;
          });
        },
        function () {
          wakeLock = null;
        }
      );
    } catch (e) {
      wakeLock = null;
    }
  }

  document.addEventListener("visibilitychange", cookWake);
  window.addEventListener("pageshow", cookWake);
  window.addEventListener("pagehide", function () {
    try {
      if (wakeLock && wakeLock !== true) wakeLock.release();
    } catch (e) {}
    wakeLock = null;
  });

  document.addEventListener("click", function (event) {
    if (!cooking) return;
    var target = event.target;
    var button;
    if (closest(target, "[data-cook-prev]")) cookShow(cooking.step - 1);
    else if (closest(target, "[data-cook-next]")) cookShow(cooking.step + 1);
    else if ((button = closest(target, "[data-cook-start]"))) {
      // A press is what lets a page make a sound: the context is made here,
      // so that it can ring later.
      if (!audio) {
        try {
          var Context = window.AudioContext || window.webkitAudioContext;
          if (Context) audio = new Context();
        } catch (e) {}
      }
      cooking.timers.push({
        id: String(Date.now()) + cooking.timers.length,
        what: button.getAttribute("data-cook-what"),
        end: Date.now() + 1000 * Number(button.getAttribute("data-cook-start")),
        left: 0,
        done: false
      });
      cookRows();
      cookSave();
    } else if ((button = closest(target, "[data-cook-pause], [data-cook-stop], [data-cook-ring]"))) {
      var id = closest(button, "[data-timer]").getAttribute("data-timer");
      var timer = cooking.timers.filter(function (t) {
        return t.id === id;
      })[0];
      if (!timer) return;
      if (button.hasAttribute("data-cook-pause")) {
        if (timer.end) {
          timer.left = remaining(timer);
          timer.end = 0;
        } else {
          timer.end = Date.now() + timer.left;
        }
      } else {
        cooking.timers.splice(cooking.timers.indexOf(timer), 1);
      }
      cookRows();
      cookSave();
      cookRing();
    } else if (closest(target, "[data-cook-end]")) {
      cooking.over = true;
      try {
        localStorage.removeItem(COOK + cooking.id);
      } catch (e) {}
    }
  });

  document.addEventListener("keydown", function (event) {
    if (!cooking || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
    if (document.querySelector("dialog[open]") || closest(event.target, "input, select, textarea")) return;
    if (event.key === "ArrowRight") cookShow(cooking.step + 1);
    else if (event.key === "ArrowLeft") cookShow(cooking.step - 1);
    else return;
    event.preventDefault();
  });

  // A swipe across the step: to the left the next one, to the right the
  // one before, as a page is turned.
  var swipe = null;
  document.addEventListener(
    "touchstart",
    function (event) {
      var touch = event.touches[0];
      swipe = cooking && event.touches.length === 1 && closest(event.target, "[data-step]") ? [touch.clientX, touch.clientY] : null;
    },
    { passive: true }
  );
  document.addEventListener(
    "touchend",
    function (event) {
      if (!swipe) return;
      var touch = event.changedTouches[0];
      var dx = touch.clientX - swipe[0];
      var dy = touch.clientY - swipe[1];
      swipe = null;
      if (Math.abs(dx) > 60 && Math.abs(dx) > 2 * Math.abs(dy)) cookShow(cooking.step + (dx < 0 ? 1 : -1));
    },
    { passive: true }
  );

  function cook() {
    var root = document.querySelector("[data-cook]");
    if (!root) return;
    var ui = root.querySelector("[data-cook-ui]");
    var id = root.getAttribute("data-cook");
    var kept = cookKept(id);
    cooking = {
      id: id,
      steps: Array.prototype.slice.call(ui.querySelectorAll("[data-step]")),
      at: ui.querySelector("[data-cook-at]"),
      gauge: ui.querySelector("progress"),
      wake: ui.querySelector(".wake"),
      box: ui.querySelector(".timers"),
      list: ui.querySelector("[data-cook-timers]"),
      row: ui.querySelector("[data-cook-row]"),
      prev: ui.querySelector("[data-cook-prev]"),
      next: ui.querySelector("[data-cook-next]"),
      end: ui.querySelector("[data-cook-end]"),
      step: 0,
      timers: (Array.isArray(kept.timers) ? kept.timers : []).filter(function (timer) {
        return timer && typeof timer.id === "string" && typeof timer.what === "string" && (timer.end > 0 || timer.left > 0 || timer.done);
      })
    };
    root.querySelector("[data-cook-plain]").hidden = true;
    ui.hidden = false;
    cookShow(Number(kept.step) || 0, true);
    cookRows();
    cookTick();
    setInterval(cookTick, 500);
    cookWake();
  }

  // Loaded from the <head>: the fields are there once the document is. The
  // script comes after the sheet's <link>, so the layout the scroll
  // positions are read from is the styled one. The two scroll fixes run
  // apart: one throwing (a browser refusing `behavior: "instant"`) must not
  // cost the other.
  function ready() {
    try {
      mealStart();
    } catch (e) {}
    expiryShortcuts();
    resendCooldowns();
    strengthMeters();
    bounds();
    recurringBoxes();
    combos();
    familySelectors();
    viewers();
    try {
      revealCurrentTab();
    } catch (e) {}
    try {
      restoreScroll();
    } catch (e) {}
    morningHours();
    try {
      cook();
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
