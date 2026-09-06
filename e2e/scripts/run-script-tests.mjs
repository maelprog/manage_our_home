// Lanceur de `npm run test:scripts`, avec un plancher (#123).
//
// Avant, la porte était `node --test "scripts/lib/*.test.ts" "lib/*.test.ts"`.
// `node --test` sort en 0 quand aucun fichier ne matche — vérifié sur Node
// 24.20.0 : `tests 0 / pass 0 / fail 0`, exit 0. Un renommage de répertoire,
// un déplacement de `lib/`, un changement d'extension, et la CI reste verte
// alors que plus rien ne tourne. C'est le mode de panne que #91 corrigeait,
// sauf qu'ici la CI l'affirmerait vert.
//
// Ce fichier lance exactement le même `node --test`, avec deux rapporteurs :
//   - `spec` sur stdout — la sortie lisible, inchangée par rapport à avant ;
//   - `tap` dans un fichier temporaire — la source machine du compte de tests.
// Puis il applique le plancher de `lib/test-floor.ts`.
//
// Ordre des verdicts, et c'est le point délicat : **le code de sortie de
// `node --test` prime**. Si un test échoue vraiment, on sort avec son code,
// sans consulter le plancher. Le plancher ne peut donc jamais masquer un
// échec réel, il ne peut que rendre rouge un vert vide.
//
// En `.mjs` et non en `.ts`, par cohérence avec `run.mjs`. Mais sans le
// bénéfice que `run.mjs` en tire, et autant le dire : l'import statique de
// `./lib/test-floor.ts` ci-dessous fait mourir ce fichier-ci à la RÉSOLUTION
// de cet import sur un Node trop ancien — `ERR_UNKNOWN_FILE_EXTENSION` sur
// `test-floor.ts`, exit 1, avant qu'aucun `node --test` ne soit lancé
// (vérifié en `node:20-bookworm`). L'effet pratique est nul : rouge dans les
// deux cas, même classe d'erreur, et la CI est en Node 24 depuis #91. Faire
// mieux demanderait un import dynamique après contrôle de version, comme dans
// `run.mjs` — ce n'est pas l'objet de #123.

import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { floorViolation } from "./lib/test-floor.ts";

// « Au moins un test a tourné. » #123 dit explicitement que ce seuil suffit :
// il attrape le glob qui ne matche plus, sans rien à maintenir à chaque test
// ajouté. Le relever transformerait ce fichier en compteur à tenir à jour.
const MINIMUM_TESTS = 1;

const globs = process.argv.slice(2);
if (globs.length === 0) {
  process.stderr.write(
    "usage: node scripts/run-script-tests.mjs <glob> [glob…]\n",
  );
  process.exit(1);
}

// Hors du dépôt : rien à ajouter à .gitignore, et deux exécutions parallèles
// ne se marchent pas dessus.
const tmp = mkdtempSync(join(tmpdir(), "test-floor-"));
const tapPath = join(tmp, "report.tap");

const child = spawn(
  process.execPath,
  [
    "--test",
    "--test-reporter=spec",
    "--test-reporter-destination=stdout",
    "--test-reporter=tap",
    `--test-reporter-destination=${tapPath}`,
    ...globs,
  ],
  { stdio: "inherit" },
);

child.on("error", (err) => {
  rmSync(tmp, { recursive: true, force: true });
  process.stderr.write(`impossible de lancer node --test : ${err.message}\n`);
  process.exit(1);
});

child.on("close", (code, signal) => {
  let report = "";
  try {
    report = readFileSync(tapPath, "utf8");
  } catch {
    // Rapport absent : `floorViolation("")` s'en chargera, en rouge.
  }
  rmSync(tmp, { recursive: true, force: true });

  // 1. Le verdict de node --test d'abord, toujours. Un test qui échoue rend la
  //    porte rouge pour sa propre raison, avec son propre code.
  if (signal) {
    process.stderr.write(`node --test tué par le signal ${signal}\n`);
    process.exit(1);
  }
  if (code !== 0) process.exit(code ?? 1);

  // 2. Puis, et seulement sur un vert, le plancher.
  const violation = floorViolation(report, MINIMUM_TESTS);
  if (violation !== null) {
    process.stderr.write(`\n${violation}\n\n`);
    process.exit(1);
  }
});
