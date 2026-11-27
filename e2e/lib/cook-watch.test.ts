// La surveillance des minuteurs de cuisine hors de la page Cuisiner (#1018) :
// apps/web/src/cook-watch.js, chargé par toutes les pages. Ses fonctions
// pures (ce que garde la page Cuisiner, la carte de l'accueil, le minuteur
// échu, celui où rediriger, les libellés) tournent ici dans un contexte `vm`
// nu, sans `window` ni `document` : rien ne démarre seul.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const SOURCE = readFileSync(new URL("../../apps/web/src/cook-watch.js", import.meta.url), "utf8");

function load() {
  const context = vm.createContext({});
  vm.runInContext(SOURCE, context, { filename: "cook-watch.js" });
  const api = vm.runInContext("MHomeCookWatch", context);
  assert.ok(api, "the script defines MHomeCookWatch");
  return api;
}

const W = load();

// Ce qui sort du contexte `vm` a les prototypes d'un autre royaume.
function plain(value: unknown): unknown {
  return JSON.parse(JSON.stringify(value));
}

const A = "7b1c2d44-5a0e-4f1b-9c3a-0d6e8f21a977";
const B = "ef5a0c8e-b6d9-4c62-9051-0ebbcd646b24";
const NOW = 1_800_000_000_000;
const MIN = 60_000;
const H = 3_600_000;

function kept(fields: Record<string, unknown>): string {
  return JSON.stringify({ at: NOW - MIN, step: 1, ...fields });
}

// -- ce que garde la page Cuisiner ------------------------------------------

test("une clé de cuisine porte l'identifiant de sa recette, et rien d'autre ne l'est", () => {
  assert.equal(W.recipeOf("mhome.cuisine." + A), A);
  assert.equal(W.recipeOf("mhome.depart-repas." + A), null);
  assert.equal(W.recipeOf("mhome.cuisine.pas-un-id"), null);
  assert.equal(W.recipeOf("mhome.cuisine." + A + "/x"), null);
});

test("l'entrée lue garde le titre, l'adresse et les minuteurs valides", () => {
  const raw = kept({
    title: "Gratin dauphinois",
    url: `/recipes/${A}/cook`,
    timers: [
      { id: "t1", what: "Étape 4 · 1 h", end: NOW + H, left: 0, done: false },
      { id: 2, what: "pas d'id texte", end: NOW + H, left: 0, done: false },
      null,
      { id: "t3", what: "Étape 2 · 8 min", end: 0, left: 0, done: false },
    ],
  });
  assert.deepEqual(plain(W.entry("mhome.cuisine." + A, raw, NOW)), {
    id: A,
    title: "Gratin dauphinois",
    url: `/recipes/${A}/cook`,
    timers: [{ id: "t1", what: "Étape 4 · 1 h", end: NOW + H, left: 0, done: false }],
  });
});

test("une entrée sans titre (écrite avant #1018) prend « Recette », et l'adresse se déduit de la clé", () => {
  const e = W.entry("mhome.cuisine." + A, kept({ timers: [] }), NOW);
  assert.equal(e.title, "Recette");
  assert.equal(e.url, `/recipes/${A}/cook`);
});

test("une adresse gardée qui n'est pas la page Cuisiner de la recette est ignorée", () => {
  for (const url of ["https://ailleurs.example/", "//ailleurs.example/x", `/recipes/${B}/cook`, "javascript:alert(1)"]) {
    const e = W.entry("mhome.cuisine." + A, kept({ url, timers: [] }), NOW);
    assert.equal(e.url, `/recipes/${A}/cook`, url);
  }
});

test("illisible, ou d'une autre clé : pas d'entrée", () => {
  assert.equal(W.entry("mhome.cuisine." + A, "{pas du json", NOW), null);
  assert.equal(W.entry("mhome.cuisine." + A, "null", NOW), null);
  assert.equal(W.entry("autre." + A, kept({ timers: [] }), NOW), null);
});

test("la demi-journée de la page Cuisiner vaut ici : une cuisine oubliée n'a plus de minuteur", () => {
  // Laissée il y a 13 h, son dernier minuteur fini depuis autant.
  const old = JSON.stringify({
    at: NOW - 13 * H,
    timers: [{ id: "t", what: "Étape 1 · 8 min", end: NOW - 13 * H, left: 0, done: true }],
  });
  assert.equal(W.entry("mhome.cuisine." + A, old, NOW), null);
  // Un minuteur en pause depuis un mois n'est jamais oublié.
  const paused = JSON.stringify({
    at: NOW - 720 * H,
    timers: [{ id: "t", what: "Étape 1 · 1 h", end: 0, left: H, done: false }],
  });
  assert.equal(W.entry("mhome.cuisine." + A, paused, NOW).timers.length, 1);
});

// -- la carte « Minuteurs en cours » ----------------------------------------

function entryOf(id: string, title: string, timers: unknown[]) {
  return { id, title, url: `/recipes/${id}/cook`, timers };
}

test("la carte montre les minuteurs en marche et en pause, le plus proche de la fin d'abord", () => {
  const entries = [
    entryOf(A, "Ratatouille", [
      { id: "a1", what: "Étape 3 · 45 min", end: NOW + 12 * MIN, left: 0, done: false },
      { id: "a2", what: "Étape 1 · 5 min", end: NOW - 1000, left: 0, done: true },
    ]),
    entryOf(B, "Gratin dauphinois", [
      { id: "b1", what: "Étape 4 · 1 h", end: 0, left: 41 * MIN, done: false },
      { id: "b2", what: "Étape 2 · 8 min", end: NOW + 2 * MIN, left: 0, done: false },
    ]),
  ];
  assert.deepEqual(plain(W.live(entries, NOW)), [
    { key: `${B}:b2`, title: "Gratin dauphinois", url: `/recipes/${B}/cook`, what: "Étape 2 · 8 min", left: 2 * MIN, paused: false },
    { key: `${A}:a1`, title: "Ratatouille", url: `/recipes/${A}/cook`, what: "Étape 3 · 45 min", left: 12 * MIN, paused: false },
    { key: `${B}:b1`, title: "Gratin dauphinois", url: `/recipes/${B}/cook`, what: "Étape 4 · 1 h", left: 41 * MIN, paused: true },
  ]);
});

test("un minuteur échu mais pas encore marqué fini n'est plus « en cours »", () => {
  const entries = [entryOf(A, "Ratatouille", [{ id: "a1", what: "Étape 3 · 45 min", end: NOW, left: 0, done: false }])];
  assert.deepEqual(plain(W.live(entries, NOW)), []);
});

test("le libellé d'un rang dit l'étape et la recette", () => {
  assert.equal(W.rowLabel({ what: "Étape 4 · 1 h", title: "Gratin dauphinois" }), "Étape 4 · 1 h, Gratin dauphinois");
});

// -- le minuteur échu -------------------------------------------------------

test("un minuteur est échu quand sa fin est passée, en marche et pas encore fini", () => {
  assert.equal(W.isDue({ end: NOW, left: 0, done: false }, NOW), true);
  assert.equal(W.isDue({ end: NOW - H, left: 0, done: false }, NOW), true);
  assert.equal(W.isDue({ end: NOW + 1, left: 0, done: false }, NOW), false);
  assert.equal(W.isDue({ end: 0, left: MIN, done: false }, NOW), false);
  assert.equal(W.isDue({ end: NOW - H, left: 0, done: true }, NOW), false);
});

test("réclamer un minuteur échu le marque fini, au nom de l'onglet", () => {
  const timers = [
    { id: "a1", what: "x", end: NOW - 1, left: 0, done: false },
    { id: "a2", what: "y", end: NOW + MIN, left: 0, done: false },
  ];
  const out = W.claim(timers, NOW, "onglet-1");
  assert.equal(out.claimed, true);
  assert.deepEqual(plain(out.timers), [
    { id: "a1", what: "x", end: NOW - 1, left: 0, done: true, owner: "onglet-1" },
    { id: "a2", what: "y", end: NOW + MIN, left: 0, done: false },
  ]);
  assert.equal(W.claim(out.timers, NOW, "onglet-2").claimed, false);
});

// -- les minuteurs finis, et où rediriger -----------------------------------

test("la fenêtre liste les minuteurs finis de l'onglet, le plus ancien d'abord, hors de la recette ouverte", () => {
  const entries = [
    entryOf(A, "Ratatouille", [
      { id: "a1", what: "Étape 3 · 45 min", end: NOW - 1000, left: 0, done: true, owner: "moi" },
      { id: "a2", what: "Étape 1 · 5 min", end: NOW - 5 * MIN, left: 0, done: true, owner: "autre" },
    ]),
    entryOf(B, "Gratin dauphinois", [
      { id: "b1", what: "Étape 4 · 1 h", end: NOW - 2 * MIN, left: 0, done: true, owner: "moi" },
      { id: "b2", what: "Étape 2 · 8 min", end: NOW - 3 * MIN, left: 0, done: true },
    ]),
  ];
  const all = plain(W.finished(entries, "moi", null)) as { key: string }[];
  assert.deepEqual(all.map((f) => f.key), [`${B}:b1`, `${A}:a1`]);
  assert.deepEqual(plain(W.finished(entries, "moi", null)[0]), {
    key: `${B}:b1`,
    recipe: B,
    id: "b1",
    title: "Gratin dauphinois",
    url: `/recipes/${B}/cook`,
    what: "Étape 4 · 1 h",
    end: NOW - 2 * MIN,
  });
  // Sur la page Cuisiner de B, ses minuteurs sont les siens.
  const onB = plain(W.finished(entries, "moi", B)) as { key: string }[];
  assert.deepEqual(onB.map((f) => f.key), [`${A}:a1`]);
});

test("on redirige vers la recette du plus ancien minuteur fini, et nulle part sans minuteur", () => {
  const finished = W.finished(
    [
      entryOf(A, "Ratatouille", [{ id: "a1", what: "x", end: NOW - 1000, left: 0, done: true, owner: "moi" }]),
      entryOf(B, "Gratin", [{ id: "b1", what: "y", end: NOW - 2 * MIN, left: 0, done: true, owner: "moi" }]),
    ],
    "moi",
    null
  );
  assert.equal(W.redirectTo(finished), `/recipes/${B}/cook`);
  assert.equal(W.redirectTo([]), null);
});

test("« Arrêter » retire de ce qui est gardé les minuteurs nommés, et eux seuls", () => {
  const timers = [
    { id: "a1", what: "x", end: NOW - 1, left: 0, done: true },
    { id: "a2", what: "y", end: NOW + MIN, left: 0, done: false },
  ];
  assert.deepEqual(plain(W.without(timers, ["a1", "zz"])), [{ id: "a2", what: "y", end: NOW + MIN, left: 0, done: false }]);
});

// -- les libellés -----------------------------------------------------------

test("le temps restant s'écrit comme sur la page Cuisiner", () => {
  assert.equal(W.clock(12 * MIN + 8000), "12:08");
  assert.equal(W.clock(41 * MIN + 20_000), "41:20");
  assert.equal(W.clock(H + 29 * MIN + 5000), "1:29:05");
  assert.equal(W.clock(1), "00:01");
  assert.equal(W.clock(-5000), "00:00");
});

test("depuis quand un minuteur est fini", () => {
  assert.equal(W.ago(0), "à l'instant");
  assert.equal(W.ago(59_000), "à l'instant");
  assert.equal(W.ago(2 * MIN + 5000), "il y a 2 min");
  assert.equal(W.ago(59 * MIN), "il y a 59 min");
  assert.equal(W.ago(H + 10 * MIN), "il y a 1 h");
});

test("le titre de la fenêtre compte les minuteurs finis", () => {
  assert.equal(W.headline(1), "Minuteur terminé");
  assert.equal(W.headline(2), "2 minuteurs terminés");
});

test("le décompte nomme la recette quand il y en a plusieurs", () => {
  const one = [{ title: "Gratin dauphinois" }];
  const two = [{ title: "Gratin dauphinois" }, { title: "Ratatouille" }];
  assert.equal(W.countdown(one, 20), "Ouverture de la recette dans 20 s…");
  assert.equal(W.countdown(two, 5), "Ouverture de Gratin dauphinois, le plus ancien, dans 5 s…");
});

test("la notification dit « Minuteur terminé », la recette et l'étape", () => {
  assert.deepEqual(plain(W.notice({ title: "Gratin dauphinois", what: "Étape 4 · 1 h" })), {
    title: "Minuteur terminé",
    body: "Gratin dauphinois · Étape 4 · 1 h",
  });
});

// -- « Prévenir à la fin » --------------------------------------------------

test("le bouton n'est offert que tant que la permission n'a pas été demandée", () => {
  assert.equal(W.notifyState(true, "default"), "ask");
  assert.equal(W.notifyState(true, "granted"), "on");
  assert.equal(W.notifyState(true, "denied"), "blocked");
  assert.equal(W.notifyState(false, "default"), "none");
});
