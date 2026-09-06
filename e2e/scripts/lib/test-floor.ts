// Plancher de la porte `npm run test:scripts` (#123).
//
// `node --test` sort en 0 quand aucun fichier ne matche ses globs : la porte
// est alors verte sur zéro test exécuté, et la couverture peut disparaître en
// silence (répertoire renommé, `lib/` déplacé, extension changée). Ce module
// porte la logique pure qui relit le compte de tests d'un rapport TAP et le
// compare à un seuil ; `scripts/run-script-tests.mjs` l'utilise pour décider
// du code de sortie.
//
// Le plancher compte les tests EXÉCUTÉS (`pass + fail`) et pas les fichiers
// matchés. Ce qu'il attrape, à seuil 1 :
//   - le glob qui ne matche plus rien (`tests 0`) — la panne de #123 ;
//   - la suite dont tous les cas sont `skip` ou `todo` : des tests existent,
//     aucun n'exécute son corps ;
//   - le fichier réduit à une coquille `describe(...)` / `suite(...)` :
//     `node --test` rapporte alors `tests 0 / suites 1`.
//   Un plancher sur les fichiers matchés laisserait passer ces deux
//   dernières ; celui-ci mord.
//
// Ce qu'il NE rattrape PAS, et il faut le dire précisément parce que c'est
// l'écart entre les deux formes de plancher :
//   - **un fichier vidé jusqu'à n'enregistrer plus rien**. `node --test`
//     compte un fichier matché qui n'enregistre aucun test comme UN TEST QUI
//     PASSE — vérifié en Node 24.20.0 : deux fichiers réduits à
//     `import test from "node:test";` donnent `tests 2 / pass 2 / fail 0`,
//     exit 0, le rapporteur `spec` affichant « ✔ lib/dates.test.ts ». Sur
//     cette famille-là, compter les tests exécutés ne vaut pas mieux que
//     compter les fichiers ; la couvrir demanderait un compte des `test(...)`
//     réellement enregistrés, hors du périmètre de #123. Attention à la
//     nuance : vidé jusqu'à une coquille `describe(...)`, le fichier est
//     rouge (voir ci-dessus) ; c'est le fichier qui n'enregistre plus rien du
//     tout qui passe ;
//   - la suite qui rétrécit : trois fichiers qui tombent à un seul, ou vingt
//     tests qui tombent à un, restent verts. Attraper ça demanderait un seuil
//     à maintenir à chaque test ajouté — #123 dit que « au moins 1 » suffit.
//
// Autrement dit, à seuil 1, ce plancher équivaut à un plancher sur les
// fichiers matchés SAUF sur les familles où `node --test` ne compte aucun test
// exécuté : tout sauté/todo, ou un fichier réduit à une coquille
// `describe`/`suite`. C'est un avantage réel mais étroit, et il vaut mieux
// l'écrire que le laisser croire plus large.

export interface TapSummary {
  tests: number;
  pass: number;
  fail: number;
  skipped: number;
}

// Les compteurs du bloc de résumé TAP, en colonne 0. L'ancrage sans espace de
// tête est délibéré : un sous-test imbriqué indente tout ce qu'il émet, et
// seul le résumé racine nous intéresse.
const COUNTER = /^# (tests|pass|fail|skipped) (\d+)$/;

/**
 * Lit le bloc de résumé d'un rapport `node --test --test-reporter=tap`.
 *
 * Rend `null` si l'un des quatre compteurs manque — rapport tronqué, coupé,
 * ou produit par un format qu'on ne sait pas lire. L'appelant traite ce `null`
 * comme un échec : on ne déclare pas une porte verte sur un rapport qu'on n'a
 * pas su relire.
 */
export function parseTapSummary(report: string): TapSummary | null {
  const found = new Map<string, number>();
  for (const line of report.split("\n")) {
    const match = COUNTER.exec(line);
    // Dernière occurrence gagnante : le résumé racine est en fin de rapport.
    if (match) found.set(match[1], Number.parseInt(match[2], 10));
  }
  const tests = found.get("tests");
  const pass = found.get("pass");
  const fail = found.get("fail");
  const skipped = found.get("skipped");
  if (
    tests === undefined ||
    pass === undefined ||
    fail === undefined ||
    skipped === undefined
  ) {
    return null;
  }
  return { tests, pass, fail, skipped };
}

/**
 * Nombre de tests qui ont réellement exécuté leur corps.
 *
 * `tests` compte aussi les sautés et les `todo` ; un test sauté n'exerce rien,
 * donc il ne compte pas pour le plancher.
 */
export function executedTests(summary: TapSummary): number {
  return summary.pass + summary.fail;
}

/**
 * Rend `null` si le plancher est tenu, sinon le message d'erreur à afficher.
 */
export function floorViolation(report: string, minimum: number): string | null {
  const summary = parseTapSummary(report);
  if (summary === null) {
    return (
      "Plancher de tests : rapport TAP illisible (bloc de résumé absent ou " +
      "tronqué). Impossible de prouver qu'un test a tourné, donc la porte " +
      "est rouge. Voir e2e/scripts/lib/test-floor.ts."
    );
  }
  const executed = executedTests(summary);
  if (executed >= minimum) return null;
  // Le diagnostic dépend de ce qui manque : aucun test trouvé du tout, ou des
  // tests trouvés mais tous sautés. Accuser le glob dans le second cas
  // enverrait sur une fausse piste.
  const hint =
    summary.tests === 0
      ? "  Un glob de `test:scripts` ne matche probablement plus rien :\n" +
        "  vérifier les chemins passés à scripts/run-script-tests.mjs dans\n" +
        "  e2e/package.json."
      : `  ${summary.tests} test(s) ont été trouvés mais aucun n'a exécuté son ` +
        "corps\n  (sautés, annulés ou todo) : la couverture est nulle malgré " +
        "les fichiers.";
  return (
    `Plancher de tests : ${executed} test(s) exécuté(s), au moins ${minimum} ` +
    `exigé(s).\n` +
    `  (rapporté par node --test : tests ${summary.tests}, pass ` +
    `${summary.pass}, fail ${summary.fail}, skipped ${summary.skipped})\n` +
    hint
  );
}
