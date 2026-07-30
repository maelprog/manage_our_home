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
// ATTENTION : le job `e2e` de `.github/workflows/ci.yml` épingle
// `node-version: 20`. Câbler `npm run measure` en CI demanderait donc de
// relever cette version — c'est signalé dans e2e/README.md et volontairement
// pas fait ici (modifier la CI sort du périmètre de ce changement).

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
      "  L'image mcr.microsoft.com/playwright:*-noble utilisée par les recettes\n" +
      "  docker de e2e/README.md embarque Node 24.\n\n",
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
