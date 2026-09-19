import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  composeTagViolations,
  minioPinViolations,
  type SourceFile,
} from "./image-pins.ts";

// ---------------------------------------------------------------------------
// Garde-fou des images MinIO (#159).
//
// #158 a basculé les références MinIO de `ci.yml` et `infra/docker-compose.yml`
// vers `quay.io` sur des tags `RELEASE.*` épinglés, avec des commentaires
// disant pourquoi. Un commentaire n'est pas une porte : ces cas-ci lisent les
// vrais fichiers, sur le modèle de `wiringViolation` (#123).
// ---------------------------------------------------------------------------

const REPO = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const CI_PATH = ".github/workflows/ci.yml";
const COMPOSE_PATH = "infra/docker-compose.yml";

function realFiles(): SourceFile[] {
  return [CI_PATH, COMPOSE_PATH].map((path) => ({
    path,
    text: readFileSync(join(REPO, path), "utf8"),
  }));
}

const SERVER = "RELEASE.2025-09-07T16-13-09Z";
const CLIENT = "RELEASE.2025-08-13T08-35-41Z";

// Deux fichiers minimaux et conformes, que chaque cas abîme d'un seul point.
function files(ci: string, compose: string): SourceFile[] {
  return [
    { path: CI_PATH, text: ci },
    { path: COMPOSE_PATH, text: compose },
  ];
}
const CI_OK =
  `        run: |\n` +
  `          docker run -d --name minio quay.io/minio/minio:${SERVER} server /data\n` +
  `          docker run --rm quay.io/minio/mc:${CLIENT} -c "mc mb local/b"\n`;
const COMPOSE_OK =
  `  minio:\n    image: quay.io/minio/minio:${SERVER}\n` +
  `  minio-init:\n    image: quay.io/minio/mc:${CLIENT}\n`;

test("les vrais ci.yml et docker-compose.yml passent le garde-fou", () => {
  assert.deepEqual(minioPinViolations(realFiles()), []);
});

test("des fichiers conformes passent, avec des tags serveur et client distincts", () => {
  // L'erreur que le garde-fou ne doit PAS commettre : minio/minio et minio/mc
  // publient sur des horloges de release distinctes, leurs tags RELEASE.*
  // n'ont aucune valeur commune (constat de l'issue #159, non recompté ici).
  assert.notEqual(SERVER, CLIENT);
  assert.deepEqual(minioPinViolations(files(CI_OK, COMPOSE_OK)), []);
});

test("refuse un retour explicite à Docker Hub", () => {
  const ci = CI_OK.replace(
    `quay.io/minio/minio:${SERVER}`,
    `docker.io/minio/minio:${SERVER}`,
  );
  const violations = minioPinViolations(files(ci, COMPOSE_OK));
  assert.equal(violations.length, 1);
  assert.match(violations[0], /docker\.io\/minio\/minio/);
  assert.match(violations[0], /quay\.io/);
  assert.match(violations[0], /ci\.yml:\d+/);
});

test("refuse une référence sans registre (Docker Hub implicite)", () => {
  const compose = COMPOSE_OK.replace(
    `quay.io/minio/mc:${CLIENT}`,
    `minio/mc:${CLIENT}`,
  );
  const violations = minioPinViolations(files(CI_OK, compose));
  assert.equal(violations.length, 1);
  assert.match(violations[0], /docker-compose\.yml:4/);
});

test("refuse un autre registre, même miroir", () => {
  const ci = CI_OK.replace(
    `quay.io/minio/mc:${CLIENT}`,
    `mirror.gcr.io/minio/mc:${CLIENT}`,
  );
  assert.equal(minioPinViolations(files(ci, COMPOSE_OK)).length, 1);
});

test("refuse latest, l'absence de tag et les tags non horodatés", () => {
  for (const bad of [
    "quay.io/minio/minio:latest",
    "quay.io/minio/minio",
    "quay.io/minio/minio:RELEASE.2025-09-07",
    "quay.io/minio/minio:RELEASE",
    // Horodatage tronqué, puis préfixé : le motif doit être ancré des deux
    // côtés, pas seulement contenir un RELEASE.* quelque part.
    "quay.io/minio/minio:RELEASE.2025-09-07T16-13-09",
    "quay.io/minio/minio:latest-RELEASE.2025-09-07T16-13-09Z",
    "quay.io/minio/minio:2025",
    "quay.io/minio/minio:${MINIO_TAG}",
    '"quay.io/minio/minio:latest"',
  ]) {
    const compose = COMPOSE_OK.replace(`quay.io/minio/minio:${SERVER}`, bad);
    const violations = minioPinViolations(files(CI_OK, compose));
    assert.ok(
      violations.some((v) => /épingl/.test(v)),
      `${bad} doit être refusé comme non épinglé, obtenu : ${violations.join(" | ")}`,
    );
  }
});

test("refuse une divergence de tag entre ci.yml et docker-compose.yml", () => {
  const other = "RELEASE.2025-10-15T17-29-55Z";
  const compose = COMPOSE_OK.replace(
    `quay.io/minio/minio:${SERVER}`,
    `quay.io/minio/minio:${other}`,
  );
  const violations = minioPinViolations(files(CI_OK, compose));
  assert.equal(violations.length, 1);
  assert.match(violations[0], /minio\/minio/);
  assert.ok(violations[0].includes(SERVER) && violations[0].includes(other));
});

test("refuse une divergence entre deux jobs du même fichier", () => {
  const other = "RELEASE.2025-10-15T17-29-55Z";
  const ci = CI_OK + CI_OK.replace(`minio/mc:${CLIENT}`, `minio/mc:${other}`);
  const violations = minioPinViolations(files(ci, COMPOSE_OK));
  assert.equal(violations.length, 1);
  assert.match(violations[0], /minio\/mc/);
});

test("refuse un fichier où une image MinIO n'est plus trouvée", () => {
  // Sans ce plancher, remplacer l'image par `${MINIO_IMAGE}` ou déplacer le
  // service rendrait le garde-fou vert sur zéro référence.
  const compose = `  minio:\n    image: quay.io/minio/minio:${SERVER}\n`;
  const violations = minioPinViolations(files(CI_OK, compose));
  assert.equal(violations.length, 1);
  assert.match(violations[0], /minio\/mc/);
  assert.match(violations[0], /docker-compose\.yml/);
});

test("ignore les lignes de commentaire, qui nomment Docker Hub pour dire pourquoi", () => {
  const ci =
    "      # docker.io/minio/minio and docker.io/minio/mc stopped answering\n" +
    CI_OK;
  assert.deepEqual(minioPinViolations(files(ci, COMPOSE_OK)), []);
});

test("ne confond pas une URL MinIO ni une image voisine avec minio/minio", () => {
  const ci =
    CI_OK +
    "          curl -sf http://localhost:9000/minio/health/ready\n" +
    "          docker run example.org/minio/minio-extra:latest\n";
  assert.deepEqual(minioPinViolations(files(ci, COMPOSE_OK)), []);
});

test("refuse le vrai ci.yml ramené à Docker Hub sur un seul job", () => {
  // Contrôle de mutation sur le vrai fichier : une seule occurrence modifiée,
  // les autres restent conformes.
  const [ci, compose] = realFiles();
  const mutated = ci.text.replace("quay.io/minio/minio:", "minio/minio:");
  assert.notEqual(mutated, ci.text);
  const violations = minioPinViolations([{ ...ci, text: mutated }, compose]);
  assert.equal(violations.length, 1);
});

test("refuse le vrai docker-compose.yml repassé en latest", () => {
  const [ci, compose] = realFiles();
  const mutated = compose.text.replace(
    /quay\.io\/minio\/mc:\S+/,
    "quay.io/minio/mc:latest",
  );
  assert.notEqual(mutated, compose.text);
  const violations = minioPinViolations([ci, { ...compose, text: mutated }]);
  assert.ok(violations.length >= 1);
});

// ---------------------------------------------------------------------------
// Tags des autres images du compose (#160).
//
// Le garde-fou ci-dessus ne voit que minio/minio et minio/mc. Les autres
// images de `infra/docker-compose.yml` portaient la même fragilité : `mailpit`
// et `ollama` sur `latest`. Ces cas-ci exigent de chaque `image:` du compose
// une version complète, sauf `postgres:16` et `caddy:2`, laissés sur leur
// série majeure par choix (motif dans `image-pins.ts`).
// ---------------------------------------------------------------------------

const COMPOSE_TAGS_OK =
  "services:\n" +
  "  postgres:\n    image: postgres:16\n" +
  `  minio:\n    image: quay.io/minio/minio:${SERVER}\n` +
  "  mailpit:\n    image: axllent/mailpit:v1.31.2\n" +
  "  ollama:\n    image: ollama/ollama:0.34.2\n" +
  "  api:\n    build:\n      context: ..\n" +
  "  caddy:\n    image: caddy:2\n";

function composeFile(text: string): SourceFile {
  return { path: COMPOSE_PATH, text };
}

test("le vrai docker-compose.yml n'a aucune image sur un tag flottant", () => {
  assert.deepEqual(composeTagViolations(realFiles()[1]), []);
});

test("un compose conforme passe : versions complètes, RELEASE MinIO, postgres:16 et caddy:2", () => {
  assert.deepEqual(composeTagViolations(composeFile(COMPOSE_TAGS_OK)), []);
});

test("refuse une image sur latest, avec le fichier et la ligne", () => {
  const text = COMPOSE_TAGS_OK.replace(
    "axllent/mailpit:v1.31.2",
    "axllent/mailpit:latest",
  );
  const violations = composeTagViolations(composeFile(text));
  assert.equal(violations.length, 1);
  assert.match(violations[0], /docker-compose\.yml:7 /);
  assert.match(violations[0], /axllent\/mailpit:latest/);
});

test("refuse une image sans tag (latest implicite)", () => {
  const text = COMPOSE_TAGS_OK.replace("ollama/ollama:0.34.2", "ollama/ollama");
  const violations = composeTagViolations(composeFile(text));
  assert.equal(violations.length, 1);
  assert.match(violations[0], /ollama\/ollama/);
});

test("refuse une série majeure hors de postgres:16 et caddy:2", () => {
  const text = COMPOSE_TAGS_OK.replace(
    "axllent/mailpit:v1.31.2",
    "axllent/mailpit:v1",
  );
  assert.equal(composeTagViolations(composeFile(text)).length, 1);
});

test("refuse pour postgres ou caddy une autre série que celle retenue", () => {
  // postgres:16 est aussi la série des jobs de la CI : changer de série se
  // fait ici, en connaissance de cause, pas au détour d'un diff du compose.
  const text = COMPOSE_TAGS_OK.replace("postgres:16", "postgres:17").replace(
    "caddy:2",
    "caddy:latest",
  );
  assert.equal(composeTagViolations(composeFile(text)).length, 2);
});

test("accepte un digest derrière une version complète et ignore les commentaires", () => {
  const digest = "@sha256:" + "a".repeat(64);
  const text =
    "# image: ollama/ollama:latest\n" +
    COMPOSE_TAGS_OK.replace(
      "ollama/ollama:0.34.2",
      `ollama/ollama:0.34.2${digest}`,
    );
  assert.deepEqual(composeTagViolations(composeFile(text)), []);
});

test("refuse un compose où aucune image n'est trouvée", () => {
  // Plancher : sans lui, sortir les images des lignes `image:` rendrait la
  // porte verte sur zéro référence.
  const text = "services:\n  api:\n    build: ..\n";
  assert.equal(composeTagViolations(composeFile(text)).length, 1);
});

test("refuse le vrai docker-compose.yml avec mailpit repassé en latest", () => {
  const real = realFiles()[1];
  const mutated = real.text.replace(
    /axllent\/mailpit:\S+/,
    "axllent/mailpit:latest",
  );
  assert.notEqual(mutated, real.text);
  assert.equal(composeTagViolations({ ...real, text: mutated }).length, 1);
});
