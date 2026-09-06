import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  createRng,
  cycle,
  GRID_DAYS,
  missingCount,
  monthGridWindow,
  pick,
  spreadOverMonth,
} from "./seed-core.ts";

// ---------------------------------------------------------------------------
// createRng — determinism is a hard requirement: byte measurements taken on
// two different days must be comparable, so nothing in the seed may come from
// an unseeded Math.random().
// ---------------------------------------------------------------------------

test("createRng yields the same sequence for the same seed", () => {
  const a = createRng(1234);
  const b = createRng(1234);
  assert.deepEqual([a(), a(), a(), a()], [b(), b(), b(), b()]);
});

test("createRng yields a different sequence for a different seed", () => {
  assert.notEqual(createRng(1)(), createRng(2)());
});

test("createRng stays in [0, 1)", () => {
  const rng = createRng(7);
  for (let i = 0; i < 500; i += 1) {
    const v = rng();
    assert.ok(v >= 0 && v < 1, `out of range: ${v}`);
  }
});

// ---------------------------------------------------------------------------
// pick / cycle — how the French corpora turn into rows.
// ---------------------------------------------------------------------------

test("pick draws from the array using the rng", () => {
  const rng = createRng(42);
  const drawn = [pick(rng, ["a", "b", "c"]), pick(rng, ["a", "b", "c"])];
  const rng2 = createRng(42);
  assert.deepEqual(drawn, [pick(rng2, ["a", "b", "c"]), pick(rng2, ["a", "b", "c"])]);
  drawn.forEach((v) => assert.ok(["a", "b", "c"].includes(v)));
});

test("pick refuses an empty corpus rather than returning undefined", () => {
  assert.throws(() => pick(createRng(1), []), /vide/);
});

test("cycle walks the corpus in order and wraps around", () => {
  const c = ["x", "y"];
  assert.deepEqual([cycle(c, 0), cycle(c, 1), cycle(c, 2), cycle(c, 3)], ["x", "y", "x", "y"]);
});

// ---------------------------------------------------------------------------
// missingCount — the idempotence rule: re-running tops a collection back up
// to its target instead of doubling it.
// ---------------------------------------------------------------------------

test("missingCount asks for the full target on an empty collection", () => {
  assert.equal(missingCount(40, 0), 40);
});

test("missingCount asks only for the shortfall", () => {
  assert.equal(missingCount(40, 31), 9);
});

test("missingCount asks for nothing once the target is reached or passed", () => {
  assert.equal(missingCount(40, 40), 0);
  assert.equal(missingCount(40, 57), 0);
});

// ---------------------------------------------------------------------------
// spreadOverMonth — the agenda renders the *current* month grid, so events
// seeded outside it would leave the page as empty as an unseeded stack.
// ---------------------------------------------------------------------------

test("spreadOverMonth keeps every date inside the reference month", () => {
  const days = spreadOverMonth(new Date("2026-02-14T00:00:00Z"), 40);
  assert.equal(days.length, 40);
  for (const d of days) {
    assert.equal(d.getUTCFullYear(), 2026);
    assert.equal(d.getUTCMonth(), 1);
    assert.ok(d.getUTCDate() >= 1 && d.getUTCDate() <= 28);
  }
});

test("spreadOverMonth is deterministic for a given month and count", () => {
  const a = spreadOverMonth(new Date("2026-07-30T00:00:00Z"), 12).map((d) => d.toISOString());
  const b = spreadOverMonth(new Date("2026-07-02T00:00:00Z"), 12).map((d) => d.toISOString());
  assert.deepEqual(a, b);
});

test("spreadOverMonth covers the whole 1..28 window rather than piling up on one day", () => {
  // 1..28 only, so the spread — and therefore the byte counts — don't depend
  // on whether the month has 28, 30 or 31 days.
  const days = spreadOverMonth(new Date("2026-07-30T00:00:00Z"), 28);
  assert.equal(new Set(days.map((d) => d.getUTCDate())).size, 28);
});

// ---------------------------------------------------------------------------
// monthGridWindow — LA fenêtre. Une seule, partagée par le seed et la mesure.
//
// La première version de ces scripts en avait deux : le seed comptait les
// événements existants sur ±1 an, la page n'en rend que 42 jours. Un seed
// « complet » pouvait donc laisser /agenda vide, et pire, `npm run seed` —
// l'instruction de réparation que le mesureur imprime — ne créait alors plus
// rien du tout : la stack devenait irréparable. Les deux scripts appellent
// désormais cette fonction, et une seule fenêtre existe.
//
// Miroir de `month_grid` (apps/shared/src/validation/agenda.rs) : 42 jours à
// partir du lundi qui précède ou tombe sur le 1er.
// ---------------------------------------------------------------------------

test("monthGridWindow starts on the Monday on or before the 1st", () => {
  // 1er juillet 2026 = mercredi → la grille démarre le lundi 29 juin.
  const win = monthGridWindow(new Date("2026-07-15T12:00:00Z"));
  assert.equal(win.from.toISOString().slice(0, 10), "2026-06-29");
});

test("monthGridWindow spans 42 days", () => {
  const win = monthGridWindow(new Date("2026-07-15T12:00:00Z"));
  const days = (win.to.getTime() - win.from.getTime()) / 86_400_000;
  assert.ok(days > 41 && days < 42.5, `${days} jours`);
  // Juillet 2026 : du 29 juin au 9 août inclus.
  assert.equal(win.to.toISOString().slice(0, 10), "2026-08-09");
});

test("monthGridWindow keeps a 1st that already falls on a Monday", () => {
  // 1er juin 2026 = lundi : pas de jours de débordement en tête.
  const win = monthGridWindow(new Date("2026-06-10T12:00:00Z"));
  assert.equal(win.from.toISOString().slice(0, 10), "2026-06-01");
});

test("monthGridWindow covers the whole month it is asked about", () => {
  for (const day of ["2026-02-01", "2026-02-28", "2026-12-31"]) {
    const win = monthGridWindow(new Date(`${day}T12:00:00Z`));
    const d = new Date(`${day}T12:00:00Z`);
    assert.ok(win.from <= d && d <= win.to, `${day} hors de sa propre grille`);
  }
});

test("monthGridWindow is the window spreadOverMonth seeds into", () => {
  // Le contrat qui lie les deux scripts : tout ce que le seed pose est dans
  // la fenêtre que la mesure interroge, sinon A et B reviennent.
  const reference = new Date("2026-07-31T12:00:00Z");
  const win = monthGridWindow(reference);
  for (const d of spreadOverMonth(reference, 40)) {
    assert.ok(win.from <= d && d <= win.to, `${d.toISOString()} hors grille`);
  }
});

// Garde-fou de dérive : `monthGridWindow` recopie une logique qui vit dans
// Rust. Une liste recopiée à la main qui prétend suivre un original, c'est
// exactement ce que `parseNavRoutes` refuse de laisser faire pour la nav — il
// n'y a pas de raison de s'en dispenser ici. Ce test lit la source de vérité
// et casse si `month_grid` change de longueur ou d'ancre.

test("monthGridWindow still mirrors month_grid in apps/shared", () => {
  const source = readFileSync(
    join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "apps", "shared", "src", "validation", "agenda.rs"),
    "utf8",
  );
  const body = source.match(/pub fn month_grid[\s\S]*?\n}/);
  assert.ok(body, "month_grid introuvable dans apps/shared/src/validation/agenda.rs");

  const span = body[0].match(/\(0\.\.(\d+)\)/);
  assert.ok(span, "impossible de lire la longueur de la grille dans month_grid");
  assert.equal(
    Number(span[1]),
    GRID_DAYS,
    "month_grid ne fait plus 42 jours : monthGridWindow doit suivre",
  );

  assert.match(
    body[0],
    /num_days_from_monday/,
    "month_grid n'ancre plus la grille sur le lundi : monthGridWindow doit suivre",
  );
});

test("spreadOverMonth wraps past 28 instead of leaking into the next month", () => {
  const days = spreadOverMonth(new Date("2026-02-14T00:00:00Z"), 40);
  assert.equal(days[28].getUTCDate(), 1);
  assert.equal(days[28].getUTCMonth(), 1);
  // ...but the times differ, so the two events on day 1 are not duplicates.
  assert.notEqual(days[0].getUTCHours(), days[28].getUTCHours());
});

// ---------------------------------------------------------------------------
// Le mois est celui de Paris, pas celui de l'horloge du processus.
//
// `/agenda` rend la grille du mois courant **à Paris** (`DISPLAY_TZ`,
// apps/web/src/routes/agenda/mod.rs). Les deux fonctions ci-dessus lisaient
// `getUTC*` sur l'instant brut : pendant les deux dernières heures UTC du
// dernier jour d'un mois (CEST = UTC+2), Paris est déjà au mois suivant. Le
// seed posait alors ses 40 événements dans le mois précédent pendant que la
// mesure interrogeait la grille du suivant, vide — la panne que cette paire de
// scripts existe précisément pour empêcher. Même faille de fuseau que celle que
// #114 a corrigée dans les specs, restée ici parce que #114 n'a pas touché
// `scripts/`.
//
// Ce qui change est le **mois retenu**, pas l'ancrage des bords : la fenêtre
// reste bornée à minuit UTC, avec l'écart de ~2 h à chaque bord que
// `monthGridWindow` documente et assume.
// ---------------------------------------------------------------------------

test("monthGridWindow suit le mois de Paris quand UTC est encore la veille", () => {
  // 22:30 UTC le 30 septembre = 00:30 le 1er octobre à Paris (CEST, UTC+2).
  const win = monthGridWindow(new Date("2026-09-30T22:30:00Z"));
  // Grille d'octobre : le 1er octobre 2026 tombe un jeudi, elle démarre donc
  // au lundi 28 septembre et court 42 jours, jusqu'au dimanche 8 novembre.
  // En UTC on aurait eu la grille de septembre, ancrée au lundi 31 août.
  assert.equal(win.from.toISOString().slice(0, 10), "2026-09-28");
  assert.equal(win.to.toISOString().slice(0, 10), "2026-11-08");
});

test("monthGridWindow suit le mois de Paris par-dessus le changement d'année", () => {
  // 23:30 UTC le 31 décembre = 00:30 le 1er janvier à Paris (CET, UTC+1) :
  // l'hiver, la bascule est une heure plus tard.
  const win = monthGridWindow(new Date("2026-12-31T23:30:00Z"));
  // Grille de janvier 2027 : le 1er janvier 2027 tombe un vendredi, la grille
  // démarre au lundi 28 décembre 2026 et finit au dimanche 7 février 2027.
  assert.equal(win.from.toISOString().slice(0, 10), "2026-12-28");
  assert.equal(win.to.toISOString().slice(0, 10), "2027-02-07");
});

test("monthGridWindow ne bascule pas avant Paris, l'hiver", () => {
  // 22:30 UTC le 31 décembre = 23:30 le 31 décembre à Paris : toujours
  // décembre. Le correctif ne doit pas avancer d'un mois ce qui n'a pas encore
  // basculé — le 1er décembre 2026 tombe un mardi, la grille démarre au lundi
  // 30 novembre.
  const win = monthGridWindow(new Date("2026-12-31T22:30:00Z"));
  assert.equal(win.from.toISOString().slice(0, 10), "2026-11-30");
});

test("spreadOverMonth sème dans le mois de Paris, pas dans celui d'UTC", () => {
  const days = spreadOverMonth(new Date("2026-09-30T22:30:00Z"), 40);
  for (const d of days) {
    assert.equal(d.getUTCFullYear(), 2026);
    // 9 = octobre. En UTC on aurait eu 8, septembre.
    assert.equal(d.getUTCMonth(), 9);
  }
});

test("le contrat seed/mesure tient aussi sur la bascule de fuseau", () => {
  // Le même instant des deux côtés : ce que le seed pose doit rester dans la
  // fenêtre que la mesure interroge, y compris dans les deux heures où UTC et
  // Paris ne sont pas dans le même mois.
  const reference = new Date("2026-09-30T22:30:00Z");
  const win = monthGridWindow(reference);
  for (const d of spreadOverMonth(reference, 40)) {
    assert.ok(win.from <= d && d <= win.to, `${d.toISOString()} hors grille`);
  }
});
