// Plancher de la porte `npm run test:scripts` (#123).
//
// `node --test` sort en 0 quand aucun fichier ne matche ses globs : la porte
// est alors verte sur zéro test, et la couverture peut disparaître en
// silence (répertoire renommé, `lib/` déplacé, extension changée). Ce module
// porte la logique pure qui relit le compte de tests d'un rapport TAP et le
// compare à un seuil ; `scripts/run-script-tests.mjs` l'utilise pour décider
// du code de sortie.
//
// Le plancher compte les tests dont le résultat compte (`pass + fail`) et pas
// les fichiers matchés. Ce qu'il attrape, à seuil 1 :
//   - le glob qui ne matche plus rien (`tests 0`) — la panne de #123 ;
//   - la suite dont tous les cas sont `skip` ou `todo` : des tests existent,
//     aucun n'est compté en pass ni en fail — ce qui ne veut pas dire qu'aucun
//     corps n'a tourné (#186, voir `countedTests`) ;
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
//     cette famille-là, compter les tests passés ou en échec ne vaut pas
//     mieux que compter les fichiers ; la couvrir demanderait un compte des `test(...)`
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
// passé ni en échec : tout sauté/todo, ou un fichier réduit à une coquille
// `describe`/`suite`. C'est un avantage réel mais étroit, et il vaut mieux
// l'écrire que le laisser croire plus large.
//
// ---------------------------------------------------------------------------
// Le VERDICT et le DIAGNOSTIC sont deux choses (#128).
//
// Tout ce qui précède parle du verdict, et le verdict était déjà juste sur les
// trois familles. #128 porte sur ce qui s'affiche ensuite : le message
// envoyait chercher au mauvais endroit sur deux d'entre elles.
//
//   - il invoquait « sautés, annulés ou todo » tout en n'affichant que
//     `skipped` : sur une suite entièrement `test.todo`, le lecteur voyait
//     « tests 2, pass 0, fail 0, skipped 0 » et ne pouvait pas retrouver la
//     cause depuis les chiffres (mesuré en Node 24.20.0). Le résumé lu porte
//     donc désormais les sept compteurs entiers du bloc TAP, `cancelled` et
//     `todo` compris ;
//   - il accusait le glob dès `tests 0`, alors qu'un fichier réduit à une
//     coquille `describe(...)` rend `tests 0 / suites 1` : un fichier A
//     matché, et les chemins sont bons. `suites` discrimine les deux cas.
//
// Rien de tout ça ne change un exit code : les trois familles sortaient en 1
// avant, elles sortent en 1 après.
// ---------------------------------------------------------------------------

/**
 * Seuil du plancher : « au moins un test a tourné ».
 *
 * #123 dit explicitement que ce seuil suffit — il attrape le glob qui ne
 * matche plus, sans rien à maintenir à chaque test ajouté ; le relever
 * transformerait le lanceur en compteur à tenir à jour.
 *
 * Elle vit ici, et non dans `scripts/run-script-tests.mjs`, pour une raison
 * de couverture (#128) : là-bas, la passer de 1 à 0 tuait le plancher sans
 * qu'aucun test ne bouge, parce que le lanceur n'est importable par personne
 * (il lance `node --test` au chargement). Ici, `test-floor.test.ts` l'épingle.
 */
export const MINIMUM_TESTS = 1;

export interface TapSummary {
  tests: number;
  suites: number;
  pass: number;
  fail: number;
  skipped: number;
  cancelled: number;
  todo: number;
}

// Les sept compteurs entiers du bloc de résumé TAP, en colonne 0. L'ancrage
// sans espace de tête est délibéré : un sous-test imbriqué indente tout ce
// qu'il émet, et seul le résumé racine nous intéresse. `duration_ms` est
// laissé de côté : il n'est pas entier et ne sert à aucun verdict.
const COUNTER = /^# (tests|suites|pass|fail|skipped|cancelled|todo) (\d+)$/;

/**
 * Lit le bloc de résumé d'un rapport `node --test --test-reporter=tap`.
 *
 * Rend `null` si l'un des sept compteurs manque — rapport tronqué, coupé, ou
 * produit par un format qu'on ne sait pas lire. L'appelant traite ce `null`
 * comme un échec : on ne déclare pas une porte verte sur un rapport qu'on n'a
 * pas su relire. Exiger les sept plutôt que les quatre qui décident du verdict
 * est délibéré et joue dans le sens sûr : `suites`, `cancelled` et `todo` sont
 * ce qui rend le diagnostic juste (#128), et un rapport qui ne les porte pas
 * n'est pas celui qu'on croit lire.
 */
export function parseTapSummary(report: string): TapSummary | null {
  const found = new Map<string, number>();
  for (const line of report.split("\n")) {
    const match = COUNTER.exec(line);
    // Dernière occurrence gagnante : le résumé racine est en fin de rapport.
    if (match) found.set(match[1], Number.parseInt(match[2], 10));
  }
  const tests = found.get("tests");
  const suites = found.get("suites");
  const pass = found.get("pass");
  const fail = found.get("fail");
  const skipped = found.get("skipped");
  const cancelled = found.get("cancelled");
  const todo = found.get("todo");
  if (
    tests === undefined ||
    suites === undefined ||
    pass === undefined ||
    fail === undefined ||
    skipped === undefined ||
    cancelled === undefined ||
    todo === undefined
  ) {
    return null;
  }
  return { tests, suites, pass, fail, skipped, cancelled, todo };
}

/**
 * Nombre de tests dont le résultat compte : `pass + fail`.
 *
 * Ce n'est PAS le nombre de corps exécutés (#186). `tests` compte aussi les
 * sautés, les annulés et les `todo`, et ces trois-là ne tournent pas de la
 * même façon — mesuré en Node 24.20.0 :
 *   - un sauté déclaré `test.skip(...)` n'exécute pas son corps ; mais
 *     `t.skip()` appelé DANS le corps le marque sauté après l'avoir lancé, et
 *     les compteurs ne distinguent pas les deux ;
 *   - un `todo` l'exécute s'il en a un, mais son issue n'entre ni dans `pass`
 *     ni dans `fail` : un corps qui lève sort `not ok … # TODO`, exit 0 ;
 *   - un annulé a pu commencer (timeout) ou ne jamais démarrer.
 * Aucun des trois ne peut rendre la porte rouge, donc aucun ne tient le
 * plancher. Le diagnostic, lui, doit les distinguer (voir `diagnose`).
 */
export function countedTests(
  summary: Pick<TapSummary, "pass" | "fail">,
): number {
  return summary.pass + summary.fail;
}

/**
 * Le lanceur par lequel `npm run test:scripts` doit passer pour que le
 * plancher s'applique. Débrancher cette ligne rouvre #123 en entier.
 */
export const RUNNER = "run-script-tests.mjs";

// Le drapeau `--test` de node, en jeton isolé. `--test-reporter`,
// `--test-concurrency` et le reste de la famille ne matchent pas : c'est
// l'invocation nue de `node --test` qu'on refuse, pas les options que le
// lanceur pourrait relayer un jour.
const BARE_TEST_FLAG = /(^|\s)--test(\s|$)/;

/**
 * Rend `null` si `scripts["test:scripts"]` du package.json donné mentionne le
 * lanceur et ne porte aucun `node --test` nu à côté, sinon le message d'erreur
 * à afficher.
 *
 * **D'abord la condition sous laquelle ce contrôle s'exécute**, parce que tout
 * le reste en dépend et qu'elle ne se voit pas depuis le code : ce contrôle est
 * asserté **depuis l'intérieur de la suite qu'il protège**. Seul
 * `test-floor.test.ts` appelle cette fonction, et il n'est exécuté que si les
 * globs de `test:scripts` le matchent. **Il ne mord donc que tant que la suite
 * tourne encore.** Un débranchement qui vide aussi les globs ne l'exécute
 * jamais : la ligne d'avant #123 pointée sur des chemins qui ne matchent plus
 * rend `tests 0` et **exit 0** (mesuré) — c'est-à-dire précisément l'état que
 * #123 décrit, garde-fou compris. Aucun test vivant dans une suite ne peut
 * garder l'invocation de cette suite ; fermer ce trou demanderait un contrôle
 * hors de la suite — un hook `"pretest:scripts"`, qui reste dans npm mais se
 * déclenche sous `npm run` quelle que soit la ligne `test:scripts` réécrite
 * (mesuré), et que `--ignore-scripts` saute en lançant quand même la ligne
 * (mesuré en npm 11.19.0 ; la CI ne passe pas ce drapeau), ou une étape `grep`
 * dans `ci.yml`. Hors périmètre de #123, et assumé.
 *
 * Tout ce qui suit ne vaut donc que **quand la suite tourne**.
 *
 * **Ce que le contrôle fait**, alors : une heuristique sur du texte, pas une
 * analyse de commande — recherche de sous-chaîne pour le nom du lanceur, plus
 * un refus du jeton `--test` isolé et non quoté. Un vrai parseur de ligne de
 * commande serait disproportionné pour une ligne de `package.json`. Sur les
 * lignes sondées, ça refuse la ligne d'avant #123, une porte coupée en deux
 * moitiés dont une seule passe par le lanceur (l'autre retrouve exactement la
 * panne de #123), et une ligne qui ne nomme le lanceur que dans un commentaire
 * shell.
 *
 * **Une heuristique se trompe dans les deux sens**, et les deux sont réels
 * ici :
 *
 *   - elle **accepte du cassé** : toute ligne qui nomme le lanceur sans
 *     l'appeler et sans écrire `--test` — `bash foo.sh # run-script-tests.mjs`,
 *     ou une seconde porte lancée par un autre wrapper sans plancher. Et une
 *     ligne qui **appelle** bien le lanceur mais en neutralise le code de
 *     sortie : `node scripts/run-script-tests.mjs "…" "…" || true` sort en 0
 *     avec un vrai test en échec, sans message de câblage (mesuré). Rien de
 *     propre à ce contrôle — `|| true` neutralise n'importe quelle porte ;
 *   - elle **refuse du correct** : un câblage juste dont le commentaire
 *     contient le jeton `--test`. Ce n'est pas un cas tiré par les cheveux —
 *     `node scripts/run-script-tests.mjs "…" # remplace l'ancien node --test
 *     direct` est refusé (mesuré), et c'est le commentaire le plus probable
 *     qu'un mainteneur écrirait ici. L'échec est bruyant et nomme la ligne
 *     fautive, ce qui vaut mieux qu'un trou silencieux, mais c'est bien un
 *     faux positif.
 *
 * Ces deux listes sont ce qui a été sondé, pas une frontière démontrée.
 *
 * Un JSON illisible, un `scripts` absent ou une ligne `test:scripts` absente
 * comptent comme une violation : on ne suppose pas le câblage bon faute de
 * savoir le lire.
 */
export function wiringViolation(packageJsonText: string): string | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(packageJsonText);
  } catch {
    return "Câblage de la porte : package.json illisible (JSON invalide).";
  }
  const scripts = (parsed as { scripts?: Record<string, unknown> })?.scripts;
  const command = scripts?.["test:scripts"];
  if (typeof command !== "string") {
    return (
      "Câblage de la porte : `scripts[\"test:scripts\"]` est absent de " +
      "package.json, ou n'est pas une chaîne."
    );
  }
  if (command.includes(RUNNER) && !BARE_TEST_FLAG.test(command)) return null;
  return (
    `Câblage de la porte : \`scripts["test:scripts"]\` ne passe pas (ou pas ` +
    `entièrement) par \`${RUNNER}\`.\n` +
    `  ligne trouvée : ${command}\n` +
    "  Un `node --test` nu n'a pas de plancher : il sort en 0 quand ses globs\n" +
    "  ne matchent rien, ce qui est #123 rouvert — pour toute la porte, ou\n" +
    "  seulement pour la moitié qui ne passe pas par le lanceur."
  );
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
  const counted = countedTests(summary);
  if (counted >= minimum) return null;
  return (
    `Plancher de tests : ${counted} test(s) passé(s) ou en échec, au moins ` +
    `${minimum} exigé(s).\n` +
    `  (rapporté par node --test : tests ${summary.tests}, suites ` +
    `${summary.suites}, pass ${summary.pass}, fail ${summary.fail},\n` +
    `   skipped ${summary.skipped}, cancelled ${summary.cancelled}, todo ` +
    `${summary.todo})\n` +
    diagnose(summary)
  );
}

/**
 * La phrase qui dit OÙ CHERCHER, à partir des compteurs. Pas de verdict ici :
 * l'appelant a déjà tranché, on explique.
 *
 * Trois familles, discriminées par `tests` et `suites` — c'est l'objet de
 * #128, où le message accusait le glob sur une famille qui n'a rien à voir :
 *
 *   - `tests 0 / suites 0` : plus rien n'a matché. Le glob est le bon suspect.
 *   - `tests 0 / suites > 0` : un fichier A matché, et il a enregistré des
 *     `describe(...)` / `suite(...)`, mais aucun `test(...)` dedans. Les
 *     chemins sont bons ; c'est le contenu des fichiers qu'il faut regarder.
 *     Le mot « glob » est réservé à la branche du dessus — c'est ce qui rend
 *     `assert.doesNotMatch(message, /glob/i)` un contrôle qui veut dire
 *     quelque chose dans `test-floor.test.ts`, et pas un hasard de rédaction.
 *   - `tests > 0` : des cas existent, aucun n'est passé ni en échec. On nomme
 *     alors les compteurs non nuls qui l'expliquent plutôt que de réciter
 *     « sautés, annulés ou todo » — c'est exactement ce que #128 reproche à
 *     l'affichage précédent, qui invoquait trois familles tout en n'affichant
 *     que `skipped`. Et chacun dit ce qu'il sait du corps (#186) : le
 *     message affirmait « aucun n'a exécuté son corps » sur une suite `todo`,
 *     dont Node 24 exécute bien les corps.
 *
 * Une quatrième, qui n'existe qu'au-dessus du seuil 1 (#152) : `tests > 0`
 * avec des tests passés ou en échec, mais moins que le seuil. `diagnose`
 * affirmait « aucun n'a exécuté … la couverture est nulle » sans regarder `pass` /
 * `fail`, et contredisait la ligne qu'il suit. Inatteignable tant que
 * `MINIMUM_TESTS` vaut 1, mais `minimum` est un paramètre de `floorViolation`.
 */
function diagnose(summary: TapSummary): string {
  if (summary.tests === 0) {
    if (summary.suites === 0) {
      return (
        "  Un glob de `test:scripts` ne matche probablement plus rien :\n" +
        "  vérifier les chemins passés à scripts/run-script-tests.mjs dans\n" +
        "  e2e/package.json."
      );
    }
    return (
      `  ${summary.suites} suite(s) ont matché et se sont enregistrées, mais ` +
      "aucune\n  n'a déclaré de cas : une coquille `describe(...)` / " +
      "`suite(...)` vidée de\n  ses `test(...)`. Les chemins de " +
      "`test:scripts` matchent donc encore —\n  chercher dans le contenu des " +
      "fichiers, pas dans les chemins."
    );
  }
  // Ne citer que les compteurs non nuls, chacun avec ce qu'il dit du corps :
  // un todo a tourné s'il avait un corps, un sauté seulement si `t.skip()` a
  // été appelé dedans, un annulé peut-être (#186).
  const causes: string[] = [];
  if (summary.skipped > 0) {
    causes.push(
      `${summary.skipped} sauté(s) (corps non lancé, sauf t.skip() appelé ` +
        "dans le corps)",
    );
  }
  if (summary.cancelled > 0) {
    causes.push(
      `${summary.cancelled} annulé(s) (corps interrompu ou jamais lancé)`,
    );
  }
  if (summary.todo > 0) {
    causes.push(
      `${summary.todo} todo (corps exécuté s'il existe, issue non comptée)`,
    );
  }
  const counted = countedTests(summary);
  const outcomes =
    counted + summary.skipped + summary.cancelled + summary.todo;
  if (outcomes > summary.tests) {
    // Sur les rapports mesurés, `tests` vaut la somme des cinq issues. Plus
    // d'issues que de tests, node ne le produit pas : écrire « dont »
    // présenterait comme un sous-ensemble ce qui n'en est pas un. On dit
    // l'incohérence et on renvoie aux chiffres. (Le cas inverse, moins
    // d'issues que de tests, tombe dans la formule générique plus bas.)
    return (
      `  Compteurs incohérents : ${outcomes} issue(s) (pass, fail, skipped, ` +
      `cancelled, todo)\n  pour ${summary.tests} test(s) trouvé(s). Relire ` +
      "le rapport de node --test : la cause\n  ne se déduit pas de ces " +
      "chiffres."
    );
  }
  if (counted > 0) {
    // Seuil > 1 seulement : des résultats comptent, pas assez. Ni « aucun »,
    // ni « couverture nulle » — et pas de formule générique non plus : sans
    // compteur qui explique l'écart, on donne les deux nombres et rien d'autre.
    return (
      `  ${summary.tests} test(s) trouvé(s), dont ${counted} passé(s) ou en ` +
      "échec : la couverture existe\n  mais reste sous le seuil exigé." +
      bullets(causes)
    );
  }
  // Rien de compté. Si aucun compteur ne l'explique (tests > 0, tout le reste
  // à zéro : compteurs incohérents), on garde la formule générique plutôt que
  // d'inventer une cause.
  const detail =
    causes.length > 0 ? bullets(causes) : "\n    - sautés, annulés ou todo";
  return (
    `  ${summary.tests} test(s) trouvé(s), aucun passé ni en échec :\n` +
    "  aucun résultat ne tient le plancher." +
    detail
  );
}

function bullets(lines: string[]): string {
  return lines.map((line) => `\n    - ${line}`).join("");
}
