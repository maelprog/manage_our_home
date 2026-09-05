// Lanceur des deux scripts de `scripts/`. En `.mjs` et non en `.ts` pour une
// raison précise : sur un Node trop ancien, un point d'entrée `.ts` échoue sur
// `ERR_UNKNOWN_FILE_EXTENSION: Unknown file extension ".ts"` — un message qui
// ne nomme ni la version requise, ni ce qu'il faut faire. Le contrôle doit
// donc vivre dans un fichier que TOUTES les versions savent charger.
//
// Deux exigences, toutes deux dans `node:` :
//   - ≥ 22.15 : `zlib.zstdCompressSync` (undefined en 22.14) ;
//   - ≥ 22.18 : exécution directe des `.ts` sans drapeau.
// La seconde domine, c'est elle qu'on exige.
//
// Le job `e2e` de `.github/workflows/ci.yml` tourne en Node 24 depuis #91, et
// y câble `npm run test:scripts`. `npm run measure` reste non câblé : la
// version qui l'en empêchait est levée, mais faire d'un rapport de budget une
// porte de merge est une autre décision.
//
// Portée exacte des garde-fous de version, parce qu'ils ne couvrent pas la
// même chose et qu'on s'y trompe :
//   - `e2e/.npmrc` (`engine-strict=true`, #91) n'agit qu'à l'INSTALLATION :
//     `npm ci` / `npm install` échouent en EBADENGINE sur un Node trop ancien.
//     Un `npm run …` n'est pas filtré, npm n'y relit pas `engines` ;
//   - ce fichier-ci ne couvre que `npm run seed` et `npm run measure`, les
//     seuls scripts qui passent par lui ;
//   - `npm run test:scripts` appelle `node --test` directement, donc ni l'un
//     ni l'autre. Sur un Node trop ancien il meurt sur le message obscur
//     d'origine — `ERR_UNKNOWN_FILE_EXTENSION`, ou « Could not find » si la
//     version ne sait pas encore développer les globs de `--test` (Node 20).
//     En CI le trou est bouché en amont par `node-version: 24`.

// Aucun import statique ici, et surtout pas `{ zstdCompressSync }` depuis
// node:zlib : sur Node 20 cet import échoue à l'instanciation du module, AVANT
// la moindre ligne de ce fichier, sur un « does not provide an export named »
// aussi peu parlant que l'erreur qu'on cherche à remplacer. Le contrôle de
// version doit pouvoir s'exécuter sur la version qu'il rejette.

const REQUIRED = [22, 18, 0];

function parseVersion(v) {
  return v.replace(/^v/, "").split(".").map((n) => Number.parseInt(n, 10));
}

function tooOld(actual, required) {
  for (let i = 0; i < required.length; i += 1) {
    if ((actual[i] ?? 0) !== required[i]) return (actual[i] ?? 0) < required[i];
  }
  return false;
}

const current = parseVersion(process.version);
if (tooOld(current, REQUIRED)) {
  process.stderr.write(
    `\nNode ${process.version} est trop ancien pour les scripts de e2e/scripts/.\n\n` +
      `  Requis : Node >= ${REQUIRED.join(".")}\n` +
      "    - >= 22.18 pour exécuter les fichiers .ts sans étape de build ;\n" +
      "    - >= 22.15 pour zlib.zstdCompressSync, qui sert à peser les pages.\n\n" +
      "  Sans ce contrôle, Node échouerait sur « ERR_UNKNOWN_FILE_EXTENSION:\n" +
      '  Unknown file extension ".ts" », qui ne dit pas quelle version installer.\n\n' +
      "  N'importe quel Node >= 22.18 convient : les images officielles\n" +
      "  node:22 / node:24, ou une image mcr.microsoft.com/playwright:*-noble\n" +
      "  dont le tag correspond au @playwright/test épinglé par\n" +
      "  package-lock.json (v1.61.1-noble porte Node 24.17). Ne pas supposer\n" +
      "  que tous les tags -noble portent Node 24 : v1.55.0-noble est en\n" +
      "  22.18.\n\n",
  );
  process.exit(1);
}

// Ceinture et bretelles : la version peut convenir et le binaire être compilé
// sans zstd. Mieux vaut le dire ici qu'au milieu d'un tableau de mesures.
// Import dynamique, donc après le contrôle de version ci-dessus.
const { zstdCompressSync } = await import("node:zlib");
if (typeof zstdCompressSync !== "function") {
  process.stderr.write(
    `\nCe Node (${process.version}) n'expose pas zlib.zstdCompressSync : ` +
      "impossible de mesurer la colonne zstd.\n\n",
  );
  process.exit(1);
}

const target = process.argv[2];
if (!target) {
  process.stderr.write("usage: node scripts/run.mjs <script.ts> [args…]\n");
  process.exit(1);
}

// Les arguments du script cible sont déjà dans process.argv à partir de
// l'index 3 ; les scripts lisent `process.argv.slice(2)`, donc on retire notre
// propre argument pour qu'ils voient exactement ce que l'utilisateur a tapé.
process.argv.splice(2, 1);

await import(new URL(target, import.meta.url).href);
