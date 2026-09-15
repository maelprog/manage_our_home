import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  executedTests,
  floorViolation,
  MINIMUM_TESTS,
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

// Une suite entièrement `test.todo`. Le cas de #128 : `tests 2`, mais
// `pass`, `fail` ET `skipped` à zéro — les compteurs affichés jusqu'ici ne
// laissaient aucun moyen de retrouver la cause. Copié d'une exécution.
const TAP_TOUT_TODO = `# Subtest: deux
ok 2 - deux # TODO
  ---
  duration_ms: 0.060506
  type: 'test'
  ...
1..2
# tests 2
# suites 0
# pass 0
# fail 0
# cancelled 0
# skipped 0
# todo 2
# duration_ms 51.164494
`;

// Un fichier réduit à une coquille `describe(...)` sans aucun cas : `tests 0`,
// mais `suites 1`. Un fichier A MATCHÉ — accuser le glob envoie chercher au
// mauvais endroit (#128). Copié d'une exécution.
const TAP_COQUILLE = `TAP version 13
# Subtest: coquille
ok 1 - coquille
  ---
  duration_ms: 0.261037
  type: 'suite'
  ...
1..1
# tests 0
# suites 1
# pass 0
# fail 0
# cancelled 0
# skipped 0
# todo 0
# duration_ms 51.206292
`;

// ---------------------------------------------------------------------------
// parseTapSummary
// ---------------------------------------------------------------------------

test("parseTapSummary lit le bloc de résumé d'un rapport vide", () => {
  assert.deepEqual(parseTapSummary(TAP_ZERO_MATCH), {
    tests: 0,
    suites: 0,
    pass: 0,
    fail: 0,
    skipped: 0,
    cancelled: 0,
    todo: 0,
  });
});

test("parseTapSummary lit le résumé malgré des sous-tests imbriqués", () => {
  assert.deepEqual(parseTapSummary(TAP_MIXED), {
    tests: 4,
    suites: 0,
    pass: 3,
    fail: 0,
    skipped: 1,
    cancelled: 0,
    todo: 0,
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
    suites: 0,
    pass: 1,
    fail: 0,
    skipped: 0,
    cancelled: 0,
    todo: 0,
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

test("floorViolation affiche les compteurs todo et cancelled (#128)", () => {
  // Le message invoque « sautés, annulés ou todo » : les trois compteurs
  // correspondants doivent figurer dans le résumé affiché, sinon le lecteur
  // voit « tests 2, pass 0, fail 0, skipped 0 » et ne peut pas retrouver la
  // cause depuis les chiffres.
  const message = floorViolation(TAP_TOUT_TODO, 1);
  assert.ok(message, "une suite entièrement todo n'exécute rien");
  assert.match(message, /todo 2/);
  assert.match(message, /cancelled 0/);
});

test("floorViolation nomme ce qui a empêché l'exécution, pas la liste entière", () => {
  // Deux tests, tous todo : le diagnostic doit dire « todo », pas réciter
  // « sautés, annulés ou todo » quand un seul des trois est en cause.
  const message = floorViolation(TAP_TOUT_TODO, 1);
  assert.ok(message);
  assert.match(message, /2 todo/);
  assert.doesNotMatch(message, /annul/i);
  assert.doesNotMatch(message, /glob/i);
});

test("floorViolation nomme les annulés quand c'est eux", () => {
  const tap = `1..1
# tests 1
# suites 0
# pass 0
# fail 0
# cancelled 1
# skipped 0
# todo 0
`;
  const message = floorViolation(tap, 1);
  assert.ok(message);
  assert.match(message, /1 annulé/);
  // Les sept compteurs sont affichés tels quels (« todo 0 ») : ce qu'on
  // interdit ici, c'est de NOMMER todo ou sauté comme cause.
  assert.doesNotMatch(message, /\d+ todo/);
  assert.doesNotMatch(message, /saut/i);
});

test("épingle : floorViolation garde la formule générique quand aucun compteur n'explique", () => {
  // Compteurs incohérents (un test trouvé, rien d'exécuté, rien de sauté ni
  // annulé ni todo) : on ne peut pas nommer la cause, on ne l'invente pas.
  //
  // Épingle de régression, PAS un cas TDD (#152) : il passe aussi sur
  // `test-floor.ts` tel qu'en 14dd30d, avant le correctif du diagnostic de
  // #128, qui récitait toujours cette formule. Il ne discrimine donc pas ce
  // correctif ; il mord si le repli générique est supprimé au profit d'un
  // `causes.join(", ")` vide.
  const tap = `1..1
# tests 1
# suites 0
# pass 0
# fail 0
# cancelled 0
# skipped 0
# todo 0
`;
  const message = floorViolation(tap, 1);
  assert.ok(message);
  assert.match(message, /sautés, annulés ou todo/);
});

test("floorViolation n'accuse pas le glob quand une coquille a matché (#128)", () => {
  // `tests 0 / suites 1` : un fichier A matché, mais il ne porte plus aucun
  // cas. Accuser le glob envoie vérifier des chemins qui sont bons.
  const message = floorViolation(TAP_COQUILLE, 1);
  assert.ok(message, "une coquille describe() n'exécute aucun test");
  assert.doesNotMatch(message, /glob/i);
  assert.match(message, /suite/i);
  assert.match(message, /describe/);
});

test("floorViolation accuse le glob seulement quand aucune suite n'a matché", () => {
  // La discrimination tient sur `suites` : 0 suite ET 0 test = plus rien ne
  // matche ; au moins une suite = les chemins sont bons.
  assert.match(floorViolation(TAP_ZERO_MATCH, 1) ?? "", /glob/i);
  // Pas de `?? ""` de ce côté-ci (#152) : si le plancher cessait de mordre sur
  // une coquille, `null` deviendrait `""`, qui ne matche pas /glob/, et
  // l'assertion passerait sur la régression même. D'où le `assert.ok`
  // d'abord. La ligne du dessus n'a pas le problème : `""` ne matche pas
  // /glob/i, un `null` y sort rouge.
  const coquille = floorViolation(TAP_COQUILLE, 1);
  assert.ok(coquille, "une coquille describe() doit violer le plancher");
  assert.doesNotMatch(coquille, /glob/i);
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

// Au-dessus du seuil 1, « rien n'a exécuté » n'est plus la seule façon d'être
// sous le plancher : des tests peuvent avoir tourné, juste pas assez (#152).
// Le diagnostic ne doit alors dire ni « aucun », ni « couverture nulle ».
// Inatteignable tant que `MINIMUM_TESTS` vaut 1, mais `minimum` est un
// paramètre et le cas ci-dessus le traite comme un contrat.

test("floorViolation ne dit pas « aucun » quand des tests ont tourné sous un seuil > 1 (#152)", () => {
  // La reproduction de #152 : 2 exécutés, 3 sautés, 5 exigés.
  const tap = `1..5
# tests 5
# suites 0
# pass 2
# fail 0
# cancelled 0
# skipped 3
# todo 0
`;
  const message = floorViolation(tap, 5);
  assert.ok(message, "2 exécutés < 5 exigés");
  assert.doesNotMatch(message, /aucun/i);
  assert.doesNotMatch(message, /nulle/i);
  // Ce qui n'a pas tourné reste nommé.
  assert.match(message, /3 sauté/);
  // Le diagnostic donne le nombre d'exécutés, pas celui des trouvés. La
  // première ligne dit « 2 test(s) exécuté(s) », que ni l'une ni l'autre de
  // ces deux formes ne matche : c'est bien le diagnostic qui est lu.
  assert.match(message, /\b2 exécuté/);
  assert.doesNotMatch(message, /\b5 exécuté/);
});

test("floorViolation compte un test en échec comme exécuté dans le diagnostic (#152)", () => {
  // `fail` a exécuté son corps autant que `pass` : un échec, deux sautés,
  // deux exigés. (Dans le lanceur, un vrai échec sort avant le plancher ; ici
  // on n'exerce que le texte.)
  const tap = `1..3
# tests 3
# suites 0
# pass 0
# fail 1
# cancelled 0
# skipped 2
# todo 0
`;
  const message = floorViolation(tap, 2);
  assert.ok(message, "1 exécuté < 2 exigés");
  assert.doesNotMatch(message, /aucun/i);
  assert.doesNotMatch(message, /nulle/i);
});

test("floorViolation n'invente pas de cause quand tout a tourné sous un seuil > 1 (#152)", () => {
  // Cinq tests, cinq exécutés, dix exigés : rien n'a été sauté, annulé ni
  // todo. La suite est simplement plus petite que le seuil ; réciter
  // « sautés, annulés ou todo » enverrait chercher une cause qui n'existe pas.
  const tap = `1..5
# tests 5
# suites 0
# pass 5
# fail 0
# cancelled 0
# skipped 0
# todo 0
`;
  const message = floorViolation(tap, 10);
  assert.ok(message, "5 exécutés < 10 exigés");
  assert.doesNotMatch(message, /aucun/i);
  assert.doesNotMatch(message, /nulle/i);
  assert.doesNotMatch(message, /sautés, annulés ou todo/);
  assert.doesNotMatch(message, /glob/i);
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

// ---------------------------------------------------------------------------
// MINIMUM_TESTS — le seuil lui-même (#128).
//
// Le passer à 0 tue le plancher sans qu'aucun autre test ne bouge : toutes les
// assertions ci-dessus passent un seuil explicite en paramètre et ne touchent
// jamais à la constante. Ces deux cas-ci l'épinglent, pour que le
// débranchement coûte la modification d'un test et se voie en revue de diff.
//
// Ce qu'ils ne couvrent PAS : que `run-script-tests.mjs` passe encore cette
// constante à `floorViolation` plutôt qu'un littéral. Même classe de trou que
// le câblage de `wiringViolation`, assumée pour la raison que #128 donne — le
// changement reste visible dans le diff.
// ---------------------------------------------------------------------------

test("MINIMUM_TESTS vaut 1 : au moins un test doit avoir tourné", () => {
  assert.equal(MINIMUM_TESTS, 1);
});

test("MINIMUM_TESTS rend le plancher mordant sur une porte vide", () => {
  // La propriété qui compte, formulée sans citer la valeur : quel que soit le
  // seuil retenu, il doit refuser un rapport à zéro test exécuté. À 0,
  // `floorViolation` serait muette sur exactement la panne de #123.
  assert.ok(
    floorViolation(TAP_ZERO_MATCH, MINIMUM_TESTS),
    "un seuil à 0 laisserait la porte verte sur zéro test",
  );
  assert.ok(
    floorViolation(TAP_TOUT_TODO, MINIMUM_TESTS),
    "un seuil à 0 laisserait la porte verte sur une suite entièrement todo",
  );
});
