// Les minuteurs de cuisine hors de la page Cuisiner (#1018,
// `assets::Script::CookWatch`), chargé par toutes les pages, dans le <head>
// après `enhance.js`. La page Cuisiner (`enhance.js`, `data-cook`) garde ses
// minuteurs par recette dans localStorage (`mhome.cuisine.<id>` :
// `{at, step, timers, title, url}`) ; ce script les relit deux fois par
// seconde, et :
//
// - remplit la carte « Minuteurs en cours » de l'accueil
//   (`data-home-timers`, `hidden` sans JavaScript ni minuteur) ;
// - à la fin d'un minuteur, sur toute page qui n'est pas la page Cuisiner de
//   sa recette, ouvre une fenêtre modale (« Minuteur terminé »), bipe
//   jusqu'à « Arrêter », et ouvre la page Cuisiner de la recette 20 s
//   après (arbitrage du 2026-11-27), sauf « Arrêter » ;
// - prévient par une notification du navigateur, si elle a été accordée
//   par « Prévenir à la fin » (`data-notify`) — jamais d'invite sans appui.
//
// Un seul propriétaire par minuteur : la page Cuisiner de sa recette le
// sonne elle-même, ce script s'en tient alors à la notification. Ailleurs,
// l'onglet qui le voit échu le marque fini à son nom (`owner`, gardé en
// sessionStorage) et lui seul le sonne ; si deux onglets le marquent à la
// fois, le dernier écrit l'emporte au passage suivant.
//
// `MHomeCookWatch` porte les décisions, sans DOM ;
// `e2e/lib/cook-watch.test.ts` les lit dans un contexte sans `window`.
var MHomeCookWatch = (function () {
  "use strict";

  var PREFIX = "mhome.cuisine.";
  // La demi-journée de la page Cuisiner (`COOK_KEPT`, enhance.js).
  var KEPT = 12 * 3600 * 1000;
  var KEY = /^mhome\.cuisine\.([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/i;

  function recipeOf(key) {
    var m = KEY.exec(String(key));
    return m ? m[1] : null;
  }

  function cookUrl(id) {
    return "/recipes/" + id + "/cook";
  }

  function valid(timer) {
    return !!timer && typeof timer.id === "string" && typeof timer.what === "string" && (timer.end > 0 || timer.left > 0 || !!timer.done);
  }

  // Depuis quand ce qui est gardé compte sa demi-journée : son dernier
  // changement ou la fin de son dernier minuteur — jamais, tant qu'un
  // minuteur attend en pause. Le `cookSince` d'enhance.js.
  function since(kept) {
    var at = Number(kept.at) || 0;
    (Array.isArray(kept.timers) ? kept.timers : []).forEach(function (timer) {
      if (!timer) return;
      if (!timer.done && !(timer.end > 0) && timer.left > 0) at = Infinity;
      else if (timer.end > at) at = timer.end;
    });
    return at;
  }

  // Ce qu'une clé garde, lisible : `{id, title, url, timers}`, ou `null`.
  // L'adresse gardée n'est suivie que si c'est la page Cuisiner de la
  // recette de la clé : rien d'autre ne mène hors du site.
  function entry(key, raw, now) {
    var id = recipeOf(key);
    if (!id) return null;
    var kept;
    try {
      kept = JSON.parse(raw);
    } catch (e) {
      return null;
    }
    if (!kept || typeof kept !== "object") return null;
    if (now - since(kept) >= KEPT) return null;
    return {
      id: id,
      title: typeof kept.title === "string" && kept.title ? kept.title : "Recette",
      url: cookUrl(id),
      timers: (Array.isArray(kept.timers) ? kept.timers : []).filter(valid)
    };
  }

  function isDue(timer, now) {
    return !timer.done && timer.end > 0 && timer.end <= now;
  }

  // Les rangs de la carte : minuteurs en marche ou en pause, le plus proche
  // de la fin d'abord.
  function live(entries, now) {
    var rows = [];
    entries.forEach(function (e) {
      e.timers.forEach(function (timer) {
        if (timer.done || isDue(timer, now)) return;
        var paused = !(timer.end > 0);
        rows.push({
          key: e.id + ":" + timer.id,
          title: e.title,
          url: e.url,
          what: timer.what,
          left: paused ? timer.left : timer.end - now,
          paused: paused
        });
      });
    });
    return rows.sort(function (a, b) {
      return a.left - b.left;
    });
  }

  function rowLabel(row) {
    return row.what + ", " + row.title;
  }

  // Marque finis, au nom de `owner`, les minuteurs échus de `timers` (tels
  // que gardés, avec tous leurs champs).
  function claim(timers, now, owner) {
    var claimed = false;
    var out = (Array.isArray(timers) ? timers : []).map(function (timer) {
      if (!valid(timer) || !isDue(timer, now)) return timer;
      claimed = true;
      var copy = {};
      Object.keys(timer).forEach(function (k) {
        copy[k] = timer[k];
      });
      copy.done = true;
      copy.owner = owner;
      return copy;
    });
    return { timers: out, claimed: claimed };
  }

  // Les minuteurs finis que cet onglet sonne, le plus ancien d'abord, hors
  // de la recette dont la page est ouverte (`skip`).
  function finished(entries, owner, skip) {
    var out = [];
    entries.forEach(function (e) {
      if (e.id === skip) return;
      e.timers.forEach(function (timer) {
        if (!timer.done || timer.owner !== owner) return;
        out.push({
          key: e.id + ":" + timer.id,
          recipe: e.id,
          id: timer.id,
          title: e.title,
          url: e.url,
          what: timer.what,
          end: timer.end
        });
      });
    });
    return out.sort(function (a, b) {
      return a.end - b.end;
    });
  }

  function redirectTo(list) {
    return list.length ? list[0].url : null;
  }

  function without(timers, ids) {
    return (Array.isArray(timers) ? timers : []).filter(function (timer) {
      return !(timer && ids.indexOf(timer.id) !== -1);
    });
  }

  // « 07:52 », « 1:29:05 » : le `clockOf` d'enhance.js.
  function clock(ms) {
    var s = Math.max(0, Math.ceil(ms / 1000));
    var h = Math.floor(s / 3600);
    var two = function (n) {
      return (n < 10 ? "0" : "") + n;
    };
    var rest = two(Math.floor((s % 3600) / 60)) + ":" + two(s % 60);
    return h ? h + ":" + rest : rest;
  }

  function ago(ms) {
    var min = Math.floor(ms / 60000);
    if (min < 1) return "à l'instant";
    if (min < 60) return "il y a " + min + " min";
    return "il y a " + Math.floor(min / 60) + " h";
  }

  function headline(n) {
    return n > 1 ? n + " minuteurs terminés" : "Minuteur terminé";
  }

  function countdown(list, seconds) {
    var what = list.length > 1 ? list[0].title + ", le plus ancien," : "la recette";
    return "Ouverture de " + what + " dans " + seconds + " s…";
  }

  function notice(item) {
    return { title: "Minuteur terminé", body: item.title + " · " + item.what };
  }

  // « ask » : le bouton ; « on » et « blocked » : une ligne d'état ;
  // « none » (pas d'API) : ni l'un ni l'autre.
  function notifyState(api, permission) {
    if (!api) return "none";
    if (permission === "granted") return "on";
    if (permission === "denied") return "blocked";
    return "ask";
  }

  return {
    PREFIX: PREFIX,
    recipeOf: recipeOf,
    entry: entry,
    isDue: isDue,
    live: live,
    rowLabel: rowLabel,
    claim: claim,
    finished: finished,
    redirectTo: redirectTo,
    without: without,
    clock: clock,
    ago: ago,
    headline: headline,
    countdown: countdown,
    notice: notice,
    notifyState: notifyState
  };
})();

(function () {
  "use strict";
  // Sans `document` (les tests), seules les fonctions ci-dessus existent.
  if (typeof document === "undefined") return;

  var W = MHomeCookWatch;
  // L'arbitrage du 2026-11-27 : 20 s entre la fin et la redirection.
  var REDIRECT_AFTER = 20000;
  var CLOCK_ICON = '<svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 6v6l4 2"/></svg>';

  function newId() {
    return String(Date.now()) + Math.random().toString(36).slice(2);
  }

  // L'onglet, d'une page à l'autre : sessionStorage est propre à l'onglet.
  var owner = (function () {
    try {
      var id = sessionStorage.getItem("mhome.onglet");
      if (!id) {
        id = newId();
        sessionStorage.setItem("mhome.onglet", id);
      }
      return id;
    } catch (e) {
      return newId();
    }
  })();

  var own = null; // la recette dont la page Cuisiner est ouverte
  var card = null;
  var cardKeys = "";
  var audio = null;
  var ringing = null;
  var dialog = null;
  var deadline = 0;
  var leaving = false;
  var shownKeys = [];
  var claimedKeys = {};
  var ownRunning = {};

  function entries(now) {
    var out = [];
    try {
      for (var i = 0; i < localStorage.length; i++) {
        var key = localStorage.key(i);
        if (!W.recipeOf(key)) continue;
        var e = W.entry(key, localStorage.getItem(key), now);
        if (e) out.push(e);
      }
    } catch (e) {}
    return out;
  }

  // Réécrit les minuteurs gardés d'une recette ; `change` reçoit ceux
  // d'aujourd'hui et rend les nouveaux, ou `null` pour ne rien écrire.
  function rewrite(id, change) {
    try {
      var key = W.PREFIX + id;
      var kept = JSON.parse(localStorage.getItem(key));
      if (!kept || typeof kept !== "object") return;
      var timers = change(kept.timers);
      if (!timers) return;
      kept.timers = timers;
      kept.at = Date.now();
      localStorage.setItem(key, JSON.stringify(kept));
    } catch (e) {}
  }

  function context() {
    if (audio) return audio;
    try {
      var Context = window.AudioContext || window.webkitAudioContext;
      if (Context) audio = new Context();
    } catch (e) {}
    return audio;
  }

  // Le bip de la page Cuisiner (`beep`, enhance.js).
  function beep() {
    try {
      var ctx = context();
      if (!ctx) return;
      if (ctx.state === "suspended") ctx.resume();
      [0, 0.3, 0.6].forEach(function (offset) {
        var at = ctx.currentTime + offset;
        var tone = ctx.createOscillator();
        var gain = ctx.createGain();
        tone.frequency.value = 880;
        gain.gain.setValueAtTime(0.0001, at);
        gain.gain.exponentialRampToValueAtTime(0.4, at + 0.02);
        gain.gain.exponentialRampToValueAtTime(0.0001, at + 0.2);
        tone.connect(gain);
        gain.connect(ctx.destination);
        tone.start(at);
        tone.stop(at + 0.22);
      });
    } catch (e) {}
    try {
      if (navigator.vibrate) navigator.vibrate([200, 100, 200]);
    } catch (e) {}
  }

  function ring(on) {
    if (on && !ringing) {
      beep();
      ringing = setInterval(beep, 2000);
    } else if (!on && ringing) {
      clearInterval(ringing);
      ringing = null;
    }
  }

  function notify(item) {
    try {
      if (typeof Notification === "undefined" || Notification.permission !== "granted") return;
      var text = W.notice(item);
      var n = new Notification(text.title, { body: text.body, tag: "mhome-minuteur-" + item.key });
      n.onclick = function () {
        try {
          window.focus();
        } catch (e) {}
        n.close();
        location.assign(item.url);
      };
    } catch (e) {}
  }

  // -- la carte de l'accueil ------------------------------------------------

  function cardRow(row) {
    var li = card.querySelector("template[data-tl-row]").content.firstElementChild.cloneNode(true);
    var link = li.querySelector("a");
    link.href = row.url;
    link.classList.toggle("paused", row.paused);
    li.querySelector(".tl-what strong").textContent = row.title;
    li.querySelector(".tl-what span").textContent = row.what;
    li.querySelector(".left").setAttribute("aria-label", W.rowLabel(row));
    li.querySelector(".tl-state").hidden = !row.paused;
    return li;
  }

  function showCard(rows) {
    if (!card) return;
    var keys = rows
      .map(function (row) {
        return row.key + (row.paused ? "/p" : "");
      })
      .join("|");
    var list = card.querySelector("ul");
    if (keys !== cardKeys) {
      cardKeys = keys;
      list.replaceChildren.apply(list, rows.map(cardRow));
    }
    Array.prototype.forEach.call(list.querySelectorAll(".left"), function (left, i) {
      left.textContent = W.clock(rows[i].left);
    });
    card.hidden = !rows.length;
  }

  // -- « Prévenir à la fin » ------------------------------------------------

  function notifyUi() {
    var api = typeof Notification !== "undefined";
    var state = W.notifyState(api, api ? Notification.permission : "");
    Array.prototype.forEach.call(document.querySelectorAll("[data-notify]"), function (el) {
      el.hidden = state !== "ask";
    });
    Array.prototype.forEach.call(document.querySelectorAll("[data-notify-on]"), function (el) {
      el.hidden = state !== "on";
    });
    Array.prototype.forEach.call(document.querySelectorAll("[data-notify-off]"), function (el) {
      el.hidden = state !== "blocked";
    });
  }

  document.addEventListener("click", function (event) {
    var button = event.target && event.target.closest && event.target.closest("[data-notify]");
    if (!button || typeof Notification === "undefined") return;
    try {
      var asked = Notification.requestPermission(notifyUi);
      if (asked && asked.then) asked.then(notifyUi, notifyUi);
    } catch (e) {}
  });

  // -- la fenêtre de fin ----------------------------------------------------

  function build() {
    dialog = document.createElement("dialog");
    dialog.className = "fin";
    dialog.setAttribute("aria-labelledby", "fin-h");
    // Aucun texte venu du stockage ici : il entre par `textContent`.
    dialog.innerHTML =
      '<div class="fin-head"><span class="fin-ic">' +
      CLOCK_ICON +
      '</span><p class="eyebrow"></p></div><h2 id="fin-h"></h2><p class="fin-what"></p>' +
      '<ul class="list fin-list"></ul><p class="muted" role="timer"></p>' +
      '<div class="fin-btns"><button type="button" data-fin-stop>Arrêter</button><a class="btn secondary">Ouvrir la recette</a></div>';
    dialog.querySelector("[data-fin-stop]").addEventListener("click", stop);
    // Échap ferme la fenêtre : c'est « Arrêter », pas un bip qui continue
    // derrière.
    dialog.addEventListener("cancel", function (event) {
      event.preventDefault();
      stop();
    });
    document.body.appendChild(dialog);
  }

  function render(list, now) {
    var many = list.length > 1;
    var head = dialog.querySelector("h2");
    dialog.querySelector(".eyebrow").textContent = W.headline(list.length);
    head.textContent = many ? "Minuteurs terminés" : list[0].title;
    head.classList.toggle("sr-only", many);
    var what = dialog.querySelector(".fin-what");
    what.textContent = list[0].what;
    what.hidden = many;
    var ul = dialog.querySelector(".fin-list");
    ul.hidden = !many;
    if (many) {
      ul.replaceChildren.apply(
        ul,
        list.map(function (item) {
          var li = document.createElement("li");
          li.className = "list-row tl";
          var label = document.createElement("span");
          label.className = "tl-what";
          var title = document.createElement("strong");
          title.textContent = item.title;
          var step = document.createElement("span");
          step.textContent = item.what;
          label.append(title, step);
          var when = document.createElement("span");
          when.className = "muted";
          when.textContent = W.ago(now - item.end);
          li.append(label, when);
          return li;
        })
      );
    }
    dialog.querySelector("a").href = W.redirectTo(list);
    var seconds = Math.max(0, Math.ceil((deadline - now) / 1000));
    dialog.querySelector("[role=timer]").textContent = W.countdown(list, seconds);
    return seconds;
  }

  function close() {
    ring(false);
    deadline = 0;
    shownKeys = [];
    if (dialog && dialog.open) dialog.close();
  }

  // « Arrêter » : les minuteurs de la fenêtre sont oubliés, comme par
  // l'« Arrêter » de la page Cuisiner ; ni bip ni redirection.
  function stop() {
    var byRecipe = {};
    finishedNow().forEach(function (item) {
      (byRecipe[item.recipe] = byRecipe[item.recipe] || []).push(item.id);
    });
    Object.keys(byRecipe).forEach(function (id) {
      rewrite(id, function (timers) {
        return W.without(timers, byRecipe[id]);
      });
    });
    close();
  }

  function finishedNow() {
    return W.finished(entries(Date.now()), owner, own);
  }

  function popup(list, now) {
    if (!list.length) {
      if (deadline) close();
      return;
    }
    if (!dialog) build();
    if (!deadline) deadline = now + REDIRECT_AFTER;
    var keys = list.map(function (item) {
      return item.key;
    });
    list.forEach(function (item) {
      if (shownKeys.indexOf(item.key) === -1 && claimedKeys[item.key]) notify(item);
    });
    shownKeys = keys;
    var seconds = render(list, now);
    if (!dialog.open) {
      try {
        dialog.showModal();
      } catch (e) {
        dialog.setAttribute("open", "");
      }
      dialog.querySelector("[data-fin-stop]").focus();
    }
    ring(true);
    if (seconds === 0 && !leaving) {
      leaving = true;
      location.assign(W.redirectTo(list));
    }
  }

  // -- le passage -----------------------------------------------------------

  function tick() {
    var now = Date.now();
    var all = entries(now);
    var claimed = false;
    all.forEach(function (e) {
      if (e.id === own) return;
      if (
        !e.timers.some(function (timer) {
          return W.isDue(timer, now);
        })
      )
        return;
      rewrite(e.id, function (timers) {
        var out = W.claim(timers, now, owner);
        if (!out.claimed) return null;
        out.timers.forEach(function (timer) {
          if (timer && timer.owner === owner && timer.done) claimedKeys[e.id + ":" + timer.id] = true;
        });
        claimed = true;
        return out.timers;
      });
    });
    if (claimed) all = entries(now);
    // La page Cuisiner sonne les siens : ici, la seule notification.
    all.forEach(function (e) {
      if (e.id !== own) return;
      e.timers.forEach(function (timer) {
        var key = e.id + ":" + timer.id;
        if (!timer.done) ownRunning[key] = true;
        else if (ownRunning[key]) {
          delete ownRunning[key];
          notify({ key: key, title: e.title, what: timer.what, url: e.url });
        }
      });
    });
    showCard(W.live(all, now));
    popup(W.finished(all, owner, own), now);
  }

  // Un appui est ce qui permet à une page de faire un son : le contexte
  // audio est créé au premier, pour pouvoir sonner ensuite.
  function unlock() {
    context();
    document.removeEventListener("pointerdown", unlock, true);
    document.removeEventListener("keydown", unlock, true);
  }
  document.addEventListener("pointerdown", unlock, true);
  document.addEventListener("keydown", unlock, true);

  function start() {
    var cook = document.querySelector("[data-cook]");
    own = cook ? cook.getAttribute("data-cook") : null;
    card = document.querySelector("[data-home-timers]");
    notifyUi();
    document.addEventListener("visibilitychange", notifyUi);
    try {
      tick();
    } catch (e) {}
    setInterval(function () {
      try {
        tick();
      } catch (e) {}
    }, 500);
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
  else start();
})();
