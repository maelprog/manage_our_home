import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { minioPinViolations, type SourceFile } from "./image-pins.ts";

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
