// Les minuteurs de cuisine hors de la page Cuisiner (#1018,
// `assets::Script::CookWatch`), chargé par toutes les pages de l'application
// (`app::shell_with_header` : jamais sur la connexion ni une page publique),
// dans le <head> après `enhance.js`. La page Cuisiner (`enhance.js`, `data-cook`) garde ses
// minuteurs par recette dans localStorage (`mhome.cuisine.<id>` :
// `{at, step, timers, title, url}`) ; ce script les relit deux fois par
// seconde, et :
//
// - remplit la carte « Minuteurs en cours » de l'accueil
//   (`data-home-timers`, `hidden` sans JavaScript ni minuteur) ;
// - à la fin d'un minuteur, sur toute page qui n'est pas la page Cuisiner de
//   sa recette, tant que celle-ci n'est ouverte dans aucun onglet, ouvre une fenêtre modale (« Minuteur terminé »), bipe
//   jusqu'à « Arrêter », et ouvre la page Cuisiner de la recette 20 s
//   après (arbitrage du 2026-11-27), sauf « Arrêter » ;
// - prévient par une notification du navigateur, si elle a été accordée
//   par « Prévenir à la fin » (`data-notify`) — jamais d'invite sans appui.
//
// Un seul propriétaire par minuteur : la page Cuisiner de sa recette le
// sonne elle-même, ce script s'en tient alors à la notification ; ouverte
// dans un autre onglet, même caché, elle le dit sous `mhome.cuisine-ouverte.
// <id>` et personne ne le réclame. Sinon, l'onglet qui le voit échu le
// marque fini à son nom (`owner`, gardé en sessionStorage) ; il ne le montre
// qu'au passage suivant, s'il est encore à son nom : de deux onglets qui le
// marquent à la fois, seul le dernier écrit le sonne et notifie.
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
  // L'adresse gardée n'est jamais lue : elle se déduit de la clé, la page
  // Cuisiner de sa recette, et rien d'autre ne mène hors du site.
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
  // que gardés, avec tous leurs champs) ; `ids` nomme ceux-là seuls, pas
  // ceux que l'onglet avait déjà.
  function claim(timers, now, owner) {
    var claimed = false;
    var ids = [];
    var out = (Array.isArray(timers) ? timers : []).map(function (timer) {
      if (!valid(timer) || !isDue(timer, now)) return timer;
      claimed = true;
      ids.push(timer.id);
      var copy = {};
      Object.keys(timer).forEach(function (k) {
        copy[k] = timer[k];
      });
      copy.done = true;
      copy.owner = owner;
      return copy;
    });
    return { timers: out, claimed: claimed, ids: ids };
  }

  // Ce qu'« Arrêter » retire, par recette : les minuteurs finis que la
  // fenêtre montrait (`shownKeys`), pas un réclamé depuis et jamais montré.
  function toStop(finished, shownKeys) {
    var byRecipe = {};
    finished.forEach(function (item) {
      if (shownKeys.indexOf(item.key) === -1) return;
      (byRecipe[item.recipe] = byRecipe[item.recipe] || []).push(item.id);
    });
    return byRecipe;
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

  // La page Cuisiner ouverte d'une recette le dit sous sa propre clé, hors
  // de celles que lit `entry` : l'heure de son dernier passage. Tant qu'elle
  // a moins de `OPEN_FOR`, cette page sonne elle-même ses minuteurs, même
  // cachée : un onglet en arrière-plan peut ne passer qu'une fois par
  // minute, et personne d'autre ne les réclame.
  var OPEN_PREFIX = "mhome.cuisine-ouverte.";
  var OPEN_FOR = 90 * 1000;

  function openKey(id) {
    return OPEN_PREFIX + id;
  }

  function cookOpen(raw, now) {
    var at = Number(raw);
    return raw != null && raw !== "" && at > 0 && now - at < OPEN_FOR;
  }

  // Les minuteurs finis qui ne viennent pas d'être réclamés à ce passage
  // (`claimedOn[key] === pass`) : deux onglets qui réclament ensemble, le
  // dernier écrit l'emporte, et seul celui-là les montre, au passage d'après.
  function settled(list, claimedOn, pass) {
    return list.filter(function (item) {
      return claimedOn[item.key] !== pass;
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

  // -- l'alarme native de l'application Android (#1019) ----------------------
  //
  // Dans l'application, le plugin CookTimers (#1017, apps/mobile/README.md
  // « Minuteurs de cuisine ») sonne le minuteur à son heure, sur le flux
  // alarme, l'application fermée. La page lui confie chaque minuteur gardé
  // qui court ou a sonné sans être arrêté (`alarms`) ; ce qu'elle lui a
  // confié, avec l'heure de fin, est gardé sous `NATIVE_KEY` d'une page à
  // l'autre, et chaque passage ne demande que la différence (`alarmPlan`) :
  // un minuteur lancé ou repris est programmé à sa fin, un minuteur mis en
  // pause, arrêté ou oublié (« Terminer », la demi-journée) est annulé. Un
  // minuteur échu n'est pas annulé : ce serait taire sa sonnerie, qui ne
  // cesse qu'à « Arrêter » (`stop`). Le plugin là, la page ne bipe pas,
  // sauf quand il a dit les notifications refusées à l'application : il ne
  // sonne alors rien (`ringsNatively`). Un navigateur, ou un APK d'avant le
  // plugin, n'a pas de plugin : le bip, comme avant.
  var NATIVE_KEY = "mhome.minuteurs-natifs";
  var NATIVE_ID = /^[A-Za-z0-9_-]{1,64}$/;

  function alarmPlugin(win) {
    try {
      var plugins = win && win.Capacitor && win.Capacitor.Plugins;
      return (plugins && plugins.CookTimers) || null;
    } catch (e) {
      return null;
    }
  }

  // L'identifiant de l'alarme d'un minuteur, tel que le plugin l'accepte ;
  // `null` sinon.
  function nativeId(recipe, timer) {
    if (typeof recipe !== "string" || typeof timer !== "string" || !recipe || !timer) return null;
    var id = recipe + "_" + timer;
    return NATIVE_ID.test(id) ? id : null;
  }

  // Les alarmes voulues, par identifiant : `{id, endAt, title, text, url}`.
  function alarms(entries) {
    var out = {};
    (Array.isArray(entries) ? entries : []).forEach(function (e) {
      if (!e) return;
      e.timers.forEach(function (timer) {
        var id = nativeId(e.id, timer.id);
        if (!id || !(timer.end > 0)) return;
        out[id] = { id: id, endAt: timer.end, title: e.title, text: timer.what, url: e.url };
      });
    });
    return out;
  }

  // Ce qu'il faut demander au plugin : `kept` donne, par identifiant,
  // l'heure de fin déjà programmée. `options.fresh` : le premier passage
  // depuis le lancement de l'application. Android oublie ses alarmes à un
  // redémarrage du téléphone ou à un arrêt forcé, et la page ne peut pas
  // lui demander lesquelles restent : elle reprogramme alors chaque
  // minuteur encore à venir (`endAt` après `options.now`), jamais un
  // minuteur échu, dont la sonnerie serait coupée.
  function alarmPlan(wanted, kept, options) {
    var fresh = !!(options && options.fresh);
    var now = options && options.now;
    var plan = { schedule: [], cancel: [] };
    Object.keys(kept).forEach(function (id) {
      if (!Object.prototype.hasOwnProperty.call(wanted, id)) plan.cancel.push(id);
    });
    Object.keys(wanted).forEach(function (id) {
      if (kept[id] !== wanted[id].endAt || (fresh && wanted[id].endAt > now)) plan.schedule.push(wanted[id]);
    });
    return plan;
  }

  // Ce qui est gardé sous `NATIVE_KEY` : `{ids, notifications?, exact?}`,
  // les deux derniers tels que le plugin (ou la permission lue) les a dits.
  function nativeRecord(raw) {
    var ids = {};
    if (raw && typeof raw === "object" && raw.ids && typeof raw.ids === "object") {
      Object.keys(raw.ids).forEach(function (id) {
        if (typeof raw.ids[id] === "number") ids[id] = raw.ids[id];
      });
    }
    var record = { ids: ids };
    if (raw && typeof raw.notifications === "boolean") record.notifications = raw.notifications;
    if (raw && typeof raw.exact === "boolean") record.exact = raw.exact;
    return record;
  }

  // Deux écrivains : la synchronisation (les `ids`, et ce que le plugin a
  // répondu) et la lecture de la permission (`notifications`). Chacun relit
  // ce qui est gardé et n'y change que ses champs, dans le même tour : une
  // synchronisation commencée avant une permission accordée ne la défait
  // pas.
  function keepRecord(current, update) {
    var record = nativeRecord(current);
    if (update && update.ids && typeof update.ids === "object") record.ids = nativeRecord({ ids: update.ids }).ids;
    if (update && typeof update.notifications === "boolean") record.notifications = update.notifications;
    if (update && typeof update.exact === "boolean") record.exact = update.exact;
    return record;
  }

  // Une synchronisation : rend ce qui est désormais programmé, à garder,
  // avec ce que le plugin a répondu (`notifications`, `exact`). Un refus du
  // plugin compte pour fait : redemandé deux fois par seconde, il serait
  // refusé autant.
  function syncAlarms(plugin, wanted, raw, options) {
    var record = nativeRecord(raw);
    var plan = alarmPlan(wanted, record.ids, options);
    var done = Promise.resolve();
    plan.cancel.forEach(function (id) {
      done = done
        .then(function () {
          return plugin.cancel({ id: id });
        })
        .catch(function () {})
        .then(function () {
          delete record.ids[id];
        });
    });
    plan.schedule.forEach(function (alarm) {
      done = done
        .then(function () {
          return plugin.schedule({ id: alarm.id, endAt: alarm.endAt, title: alarm.title, text: alarm.text, url: alarm.url });
        })
        .then(
          function (result) {
            if (result && typeof result.notifications === "boolean") record.notifications = result.notifications;
            if (result && result.scheduled && typeof result.exact === "boolean") record.exact = result.exact;
          },
          function () {}
        )
        .then(function () {
          record.ids[alarm.id] = alarm.endAt;
        });
    });
    return done.then(function () {
      return record;
    });
  }

  // Le plugin sonne-t-il les minuteurs de cette page ? Sinon, le bip.
  function ringsNatively(win) {
    if (!alarmPlugin(win)) return false;
    try {
      return nativeRecord(JSON.parse(win.localStorage.getItem(NATIVE_KEY))).notifications !== false;
    } catch (e) {
      return true;
    }
  }

  // « Prévenir à la fin » dans l'application : la permission des
  // notifications d'Android (`display` du plugin Local Notifications),
  // demandée sur ce bouton seulement. Accordée ou refusée, plus de bouton :
  // refusée, Android ne la redemande plus.
  function appNotifyState(display) {
    return display === "prompt" || display === "prompt-with-rationale" ? "ask" : "none";
  }

  // Le bouton « Alarmes et rappels » de la page Cuisiner : les
  // notifications accordées (sans elles rien ne sonne, leur bouton passe
  // d'abord), et les alarmes exactes refusées — le réglage d'Android lu par
  // Local Notifications (`exact_alarm`), sinon le dernier `exact` du plugin.
  function appExactState(display, setting, exact) {
    if (display !== "granted") return "none";
    if (setting) return setting === "granted" ? "none" : "ask";
    return exact === false ? "ask" : "none";
  }

  return {
    PREFIX: PREFIX,
    NATIVE_KEY: NATIVE_KEY,
    keepRecord: keepRecord,
    appExactState: appExactState,
    alarmPlugin: alarmPlugin,
    nativeId: nativeId,
    alarms: alarms,
    alarmPlan: alarmPlan,
    syncAlarms: syncAlarms,
    ringsNatively: ringsNatively,
    appNotifyState: appNotifyState,
    recipeOf: recipeOf,
    entry: entry,
    isDue: isDue,
    live: live,
    rowLabel: rowLabel,
    claim: claim,
    toStop: toStop,
    finished: finished,
    redirectTo: redirectTo,
    openKey: openKey,
    cookOpen: cookOpen,
    settled: settled,
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
  var claimedKeys = {}; // clé du minuteur → le passage qui l'a réclamé
  var pass = 0;
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

  // Le bip de la page Cuisiner (`beep`, enhance.js) ; aucun quand
  // l'alarme native sonne (#1019) : jamais les deux.
  function beep() {
    if (W.ringsNatively(window)) return;
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

  // Dans l'application (#1019), le bouton demande la permission des
  // notifications d'Android, sans laquelle l'alarme native ne sonne rien ;
  // les lignes d'état restent cachées. Une fois accordée, si les alarmes
  // exactes ne le sont pas, un second bouton (`data-notify-exact`) ouvre
  // « Alarmes et rappels » : sans elles, Android peut retarder le minuteur.
  // Le plugin Local Notifications lit et demande l'une et l'autre, comme le
  // bloc de « Mon compte › Notifications de rappel »
  // (`account::notifications::local_block`).
  function appPermissions() {
    try {
      var plugins = window.Capacitor && window.Capacitor.Plugins;
      return W.alarmPlugin(window) && plugins.LocalNotifications ? plugins.LocalNotifications : null;
    } catch (e) {
      return null;
    }
  }

  function showExact(state) {
    Array.prototype.forEach.call(document.querySelectorAll("[data-notify-exact]"), function (el) {
      el.hidden = state !== "ask";
    });
  }

  function showNotify(state) {
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

  function appNotifyUi(local) {
    Promise.resolve()
      .then(function () {
        return local.checkPermissions();
      })
      .then(
        function (permission) {
          var display = permission && permission.display;
          showNotify(W.appNotifyState(display));
          // Accordée depuis, l'alarme native sonne : plus de bip ; retirée,
          // elle ne sonne rien : le bip.
          if (display) keep({ notifications: display === "granted" });
          if (display !== "granted") return showExact("none");
          return Promise.resolve()
            .then(function () {
              return local.checkExactNotificationSetting();
            })
            .catch(function () {
              return null;
            })
            .then(function (setting) {
              showExact(W.appExactState(display, setting && setting.exact_alarm, kept().exact));
            });
        },
        function () {
          showNotify("none");
          showExact("none");
        }
      );
  }

  function notifyUi() {
    var local = appPermissions();
    if (local) return appNotifyUi(local);
    var api = typeof Notification !== "undefined";
    showNotify(W.notifyState(api, api ? Notification.permission : ""));
  }

  document.addEventListener("click", function (event) {
    var exact = event.target && event.target.closest && event.target.closest("[data-notify-exact]");
    var local = appPermissions();
    if (exact && local) {
      exact.disabled = true;
      Promise.resolve()
        .then(function () {
          return local.changeExactNotificationSetting();
        })
        .catch(function () {})
        .then(function () {
          exact.disabled = false;
          // Accordées, les alarmes déjà programmées ne le deviennent qu'en
          // l'étant de nouveau.
          relaunch();
          notifyUi();
        });
      return;
    }
    var button = event.target && event.target.closest && event.target.closest("[data-notify]");
    if (!button) return;
    if (local) {
      button.disabled = true;
      Promise.resolve()
        .then(function () {
          return local.requestPermissions();
        })
        .catch(function () {})
        .then(function () {
          button.disabled = false;
          notifyUi();
        });
      return;
    }
    if (typeof Notification === "undefined") return;
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
  // l'« Arrêter » de la page Cuisiner ; ni bip, ni redirection, et leur
  // alarme native se tait — la leur seule.
  function stop() {
    var byRecipe = W.toStop(finishedNow(), shownKeys);
    Object.keys(byRecipe).forEach(function (id) {
      cancelNative(id, byRecipe[id]);
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

  // -- l'alarme native (#1019) ---------------------------------------------

  // L'alarme des minuteurs `timers` de la recette `recipe` : ne sonnera
  // pas, ou cesse de sonner. Les autres sonnent toujours.
  function cancelNative(recipe, timers) {
    var plugin = W.alarmPlugin(window);
    if (!plugin) return;
    var record = kept();
    timers.forEach(function (timer) {
      var id = W.nativeId(recipe, timer);
      if (!id) return;
      delete record.ids[id];
      try {
        Promise.resolve(plugin.cancel({ id: id })).catch(function () {});
      } catch (e) {}
    });
    // Annulée ici, le passage suivant n'a pas à la redemander.
    keep({ ids: record.ids });
  }
  // La page Cuisiner (`enhance.js`) en a besoin pour son « Arrêter ».
  W.cancelNative = cancelNative;

  // Ce qui est gardé sous `NATIVE_KEY`, lisible.
  function kept() {
    try {
      return W.keepRecord(JSON.parse(localStorage.getItem(W.NATIVE_KEY)), {});
    } catch (e) {
      return W.keepRecord(null, {});
    }
  }

  // Relit et n'écrit que les champs d'`update`, dans le même tour.
  function keep(update) {
    try {
      var before = localStorage.getItem(W.NATIVE_KEY);
      var after = JSON.stringify(W.keepRecord(JSON.parse(before), update));
      if (after !== before) localStorage.setItem(W.NATIVE_KEY, after);
    } catch (e) {}
  }

  // Le premier passage depuis le lancement de l'application reprogramme
  // les minuteurs à venir (`alarmPlan`, `fresh`) : sessionStorage ne
  // survit pas à l'application tuée, ni au téléphone redémarré. Sans
  // sessionStorage, à chaque page.
  var FRESH_KEY = "mhome.minuteurs-relances";
  var relaunched = false;

  function fresh() {
    try {
      return !sessionStorage.getItem(FRESH_KEY);
    } catch (e) {
      return !relaunched;
    }
  }

  function launched() {
    relaunched = true;
    try {
      sessionStorage.setItem(FRESH_KEY, "1");
    } catch (e) {}
  }

  function relaunch() {
    relaunched = false;
    try {
      sessionStorage.removeItem(FRESH_KEY);
    } catch (e) {}
  }

  // Une synchronisation à la fois ; la suivante part du passage d'après.
  var syncing = false;

  function syncNative(all, now) {
    var plugin = W.alarmPlugin(window);
    if (!plugin || syncing) return;
    var before = kept();
    var options = { fresh: fresh(), now: now };
    var wanted = W.alarms(all);
    var plan = W.alarmPlan(wanted, before.ids, options);
    if (!plan.schedule.length && !plan.cancel.length) {
      if (options.fresh) launched();
      return;
    }
    syncing = true;
    W.syncAlarms(plugin, wanted, before, options)
      .then(function (record) {
        // Ce que le plugin a répondu, s'il a changé : une permission lue
        // pendant la synchronisation reste.
        var update = { ids: record.ids };
        if (record.notifications !== before.notifications) update.notifications = record.notifications;
        if (record.exact !== before.exact) update.exact = record.exact;
        keep(update);
        if (options.fresh) launched();
        if (record.exact !== before.exact) notifyUi();
      })
      .catch(function () {})
      .then(function () {
        syncing = false;
      });
  }

  // -- le passage -----------------------------------------------------------

  // Ouverte ailleurs, la page Cuisiner de la recette sonne elle-même.
  function openElsewhere(id, now) {
    try {
      return W.cookOpen(localStorage.getItem(W.openKey(id)), now);
    } catch (e) {
      return false;
    }
  }

  function tick() {
    var now = Date.now();
    pass += 1;
    if (own) {
      try {
        localStorage.setItem(W.openKey(own), String(now));
      } catch (e) {}
    }
    var all = entries(now);
    var claimed = false;
    all.forEach(function (e) {
      if (e.id === own || openElsewhere(e.id, now)) return;
      if (
        !e.timers.some(function (timer) {
          return W.isDue(timer, now);
        })
      )
        return;
      rewrite(e.id, function (timers) {
        var out = W.claim(timers, now, owner);
        if (!out.claimed) return null;
        out.ids.forEach(function (id) {
          claimedKeys[e.id + ":" + id] = pass;
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
    syncNative(all, now);
    showCard(W.live(all, now));
    popup(W.settled(W.finished(all, owner, own), claimedKeys, pass), now);
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
    // Quittée, la page Cuisiner rend ses minuteurs aux autres pages tout de
    // suite ; revenue de l'historique, le passage suivant la redit ouverte.
    window.addEventListener("pagehide", function () {
      if (!own) return;
      try {
        localStorage.removeItem(W.openKey(own));
      } catch (e) {}
    });
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
