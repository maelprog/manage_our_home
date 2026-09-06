import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  executedTests,
  floorViolation,
  parseTapSummary,
  wiringViolation,
} from "./test-floor.ts";

// ---------------------------------------------------------------------------
// Ces cas sont bâtis sur du TAP réellement produit par `node --test
// --test-reporter=tap` (Node 24.20.0), pas sur une idée du format. Les deux
// rapports ci-dessous sont copiés d'une exécution.
// ---------------------------------------------------------------------------

// Ce que rend `node --test "scripts/lib/*.test.ts" "lib/*.test.ts"` quand
// AUCUN fichier ne matche : c'est la panne que #123 décrit, et `node --test`
// sort en 0 dessus.
const TAP_ZERO_MATCH = `TAP version 13
1..0
# tests 0
# suites 0
# pass 0
# fail 0
# cancelled 0
# skipped 0
# todo 0
# duration_ms 11.040575
`;

// Un rapport avec un sous-test imbriqué (dont le plan `1..1` est indenté) et
// un test sauté : les deux formes qui pourraient tromper une lecture naïve.
const TAP_MIXED = `TAP version 13
# Subtest: ok
ok 1 - ok
  ---
  duration_ms: 0.993893
  type: 'test'
  ...
# Subtest: groupe
    # Subtest: sous-cas
    ok 1 - sous-cas
      ---
      duration_ms: 0.273278
      type: 'test'
      ...
    1..1
ok 2 - groupe
  ---
  duration_ms: 0.946712
  type: 'test'
  ...
# Subtest: saute
ok 3 - saute # SKIP
  ---
  duration_ms: 0.119064
  type: 'test'
  ...
1..3
# tests 4
# suites 0
# pass 3
# fail 0
# cancelled 0
# skipped 1
# todo 0
# duration_ms 155.842602
`;

// ---------------------------------------------------------------------------
// parseTapSummary
// ---------------------------------------------------------------------------

test("parseTapSummary lit le bloc de résumé d'un rapport vide", () => {
  assert.deepEqual(parseTapSummary(TAP_ZERO_MATCH), {
    tests: 0,
    pass: 0,
    fail: 0,
    skipped: 0,
  });
});

test("parseTapSummary lit le résumé malgré des sous-tests imbriqués", () => {
  assert.deepEqual(parseTapSummary(TAP_MIXED), {
    tests: 4,
    pass: 3,
    fail: 0,
    skipped: 1,
  });
});

test("parseTapSummary ignore les lignes indentées d'un sous-test", () => {
  // Un sous-test n'émet pas de bloc de résumé, mais s'il en émettait un jour
  // il serait indenté : seules les lignes en colonne 0 comptent.
  const tap = `TAP version 13
    # tests 99
    # pass 99
    # fail 0
    # skipped 0
1..1
# tests 1
# suites 0
# pass 1
# fail 0
# cancelled 0
# skipped 0
# todo 0
`;
  assert.deepEqual(parseTapSummary(tap), {
    tests: 1,
    pass: 1,
    fail: 0,
    skipped: 0,
  });
});

test("parseTapSummary rend null quand le résumé manque", () => {
  assert.equal(parseTapSummary(""), null);
  assert.equal(parseTapSummary("TAP version 13\n1..0\n"), null);
  // Résumé tronqué : `# pass` sans `# fail` ne permet pas de conclure.
  assert.equal(parseTapSummary("# tests 3\n# pass 3\n"), null);
});

// ---------------------------------------------------------------------------
// executedTests — un test sauté n'a rien exécuté.
// ---------------------------------------------------------------------------

test("executedTests compte les tests passés et échoués, pas les sautés", () => {
  assert.equal(
    executedTests({ tests: 4, pass: 3, fail: 0, skipped: 1 }),
    3,
  );
  assert.equal(
    executedTests({ tests: 5, pass: 2, fail: 1, skipped: 2 }),
    3,
  );
  assert.equal(
    executedTests({ tests: 0, pass: 0, fail: 0, skipped: 0 }),
    0,
  );
});

// ---------------------------------------------------------------------------
// floorViolation — null = plancher tenu, sinon le message à afficher.
// ---------------------------------------------------------------------------

test("floorViolation ne dit rien quand le plancher est tenu", () => {
  assert.equal(floorViolation(TAP_MIXED, 1), null);
  assert.equal(floorViolation(TAP_MIXED, 3), null);
});

test("floorViolation signale un glob qui ne matche plus rien", () => {
  const message = floorViolation(TAP_ZERO_MATCH, 1);
  assert.ok(message, "un rapport à zéro test doit violer le plancher");
  // Le message doit nommer le compte constaté et le seuil, sinon il n'aide
  // pas à distinguer « glob cassé » de « suite vide ».
  assert.match(message, /0/);
  assert.match(message, /1/);
});

test("floorViolation mord quand tous les tests sont sautés", () => {
  const tap = `1..2
# tests 2
# suites 0
# pass 0
# fail 0
# cancelled 0
# skipped 2
# todo 0
`;
  const message = floorViolation(tap, 1);
  assert.ok(
    message,
    "deux tests sautés n'exécutent rien : la porte doit être rouge",
  );
  // Deux tests ont été trouvés : accuser le glob enverrait sur une fausse
  // piste. Le diagnostic doit désigner les tests sautés.
  assert.match(message, /saut/i);
  assert.doesNotMatch(message, /glob/i);
});

test("floorViolation accuse le glob quand rien n'a même été trouvé", () => {
  const message = floorViolation(TAP_ZERO_MATCH, 1);
  assert.ok(message);
  assert.match(message, /glob/i);
});

test("floorViolation mord quand le rapport est illisible", () => {
  // Rapport absent ou tronqué : on ne peut pas prouver que quelque chose a
  // tourné, donc on refuse. Le défaut est rouge.
  assert.ok(floorViolation("", 1));
  assert.ok(floorViolation("TAP version 13\n", 1));
});

test("floorViolation accepte un seuil supérieur à 1", () => {
  // Le seuil est un paramètre : #123 se contente de « au moins 1 », mais rien
  // dans la logique ne le suppose.
  assert.ok(floorViolation(TAP_MIXED, 4), "3 exécutés < 4 exigés");
});

// ---------------------------------------------------------------------------
// wiringViolation — le plancher ne sert à rien s'il est débranché.
//
// Remettre `node --test …` sur la ligne `test:scripts` de package.json est une
// modification d'UNE ligne qui rouvre #123 en entier, et tous les tests
// ci-dessus continueraient de passer : ils exercent la logique pure, pas le
// câblage. Ce cas-ci lit le vrai package.json, sur le modèle du garde-fou de
// dérive de `seed-core.test.ts`, qui lit `apps/shared/src/validation/agenda.rs`.
// ---------------------------------------------------------------------------

// La ligne d'avant #123, mot pour mot : le contrôle négatif.
const PKG_AVANT_123 = JSON.stringify({
  scripts: {
    test: "playwright test",
    "test:scripts": 'node --test "scripts/lib/*.test.ts" "lib/*.test.ts"',
  },
});

test("wiringViolation refuse la ligne test:scripts d'avant #123", () => {
  const message = wiringViolation(PKG_AVANT_123);
  assert.ok(message, "un `node --test` direct débranche le plancher");
  assert.match(message, /test:scripts/);
});

test("wiringViolation refuse un package.json sans test:scripts", () => {
  assert.ok(wiringViolation(JSON.stringify({ scripts: { test: "x" } })));
  assert.ok(wiringViolation(JSON.stringify({})));
});

test("wiringViolation refuse un package.json illisible", () => {
  assert.ok(wiringViolation("{ pas du JSON"));
});

test("wiringViolation accepte une ligne qui passe par le lanceur", () => {
  const pkg = JSON.stringify({
    scripts: {
      "test:scripts":
        'node scripts/run-script-tests.mjs "scripts/lib/*.test.ts"',
    },
  });
  assert.equal(wiringViolation(pkg), null);
});

test("wiringViolation refuse un `node --test` lancé à côté du lanceur", () => {
  // Refactor ordinaire : couper `test:scripts` en deux moitiés dont une seule
  // passe par le lanceur. La première n'a plus de plancher et sort en 0 sur un
  // glob qui ne matche rien — #123 mot pour mot, sur la moitié orpheline —
  // pendant que la seconde tient la porte verte.
  const pkg = JSON.stringify({
    scripts: {
      "test:scripts":
        'node --test "lib/*.test.ts" && node scripts/run-script-tests.mjs "scripts/lib/*.test.ts"',
    },
  });
  const message = wiringViolation(pkg);
  assert.ok(message, "une moitié de porte sans plancher doit être refusée");
  assert.match(message, /--test/);
});

test("wiringViolation refuse une ligne qui ne nomme le lanceur qu'en commentaire", () => {
  // `#` est un commentaire shell : npm exécute bien la ligne, et le lanceur
  // n'est jamais appelé. La recherche de sous-chaîne, seule, l'accepterait.
  const pkg = JSON.stringify({
    scripts: {
      "test:scripts": 'node --test "scripts/lib/*.test.ts" # run-script-tests.mjs',
    },
  });
  assert.ok(wiringViolation(pkg));
});

test("wiringViolation accepte un drapeau --test-* passé au lanceur", () => {
  // Contrôle négatif, et il fixe la portée du refus : c'est le drapeau `--test`
  // SEUL qui est refusé, pas la famille `--test-reporter` / `--test-concurrency`
  // que le lanceur pourrait un jour relayer.
  const pkg = JSON.stringify({
    scripts: {
      "test:scripts":
        'node scripts/run-script-tests.mjs --test-concurrency=2 "scripts/lib/*.test.ts"',
    },
  });
  assert.equal(wiringViolation(pkg), null);
});

test("wiringViolation accepte deux invocations du lanceur", () => {
  // Contrôle négatif : couper la porte en deux est légitime tant que les deux
  // moitiés passent par le lanceur.
  const pkg = JSON.stringify({
    scripts: {
      "test:scripts":
        'node scripts/run-script-tests.mjs "a/*.test.ts" && node scripts/run-script-tests.mjs "b/*.test.ts"',
    },
  });
  assert.equal(wiringViolation(pkg), null);
});

test("le vrai package.json câble bien test:scripts sur le lanceur", () => {
  const pkgPath = join(
    dirname(fileURLToPath(import.meta.url)),
    "..",
    "..",
    "package.json",
  );
  const violation = wiringViolation(readFileSync(pkgPath, "utf8"));
  assert.equal(violation, null, `${pkgPath} : ${violation ?? ""}`);
});
