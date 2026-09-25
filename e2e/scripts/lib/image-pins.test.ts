import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  CI_PATH,
  COMPOSE_PATH,
  composeTagViolations,
  minioPinViolations,
  type SourceFile,
} from "./image-pins.ts";

// ---------------------------------------------------------------------------
// Garde-fou des images MinIO (#159, réécrit par #138).
//
// #158 avait basculé les références MinIO de `ci.yml` et
// `infra/docker-compose.yml` vers `quay.io` sur des tags `RELEASE.*`. Le
// 2026-09-25, `minio` a fermé les pulls anonymes partout où il publie
// (quay.io 401, Docker Hub 401, ghcr.io 403) et les trois jobs à MinIO sont
// morts en exit 125. `ci.yml` tire désormais le miroir
// `docker.io/bitnamilegacy/minio` + `minio-client`, épinglé par tag ET
// digest ; le compose reste sur les références amont tant que la migration
// de la pile livrée (chemin de données `/data` → `/bitnami/minio/data`) n'est
// pas arbitrée. Les deux fichiers ne bougent donc plus ensemble, et la porte
// applique une politique par fichier.
// ---------------------------------------------------------------------------

const REPO = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");

function realFiles(): SourceFile[] {
  return [CI_PATH, COMPOSE_PATH].map((path) => ({
    path,
    text: readFileSync(join(REPO, path), "utf8"),
  }));
}

// Pins synthétiques : la forme est celle des vrais pins, les valeurs non (les
// vrais ne sont lus que par les cas « vrais fichiers » ci-dessous).
const SERVER = "2025.7.23-debian-12-r5";
const CLIENT = "2025.7.21-debian-12-r3";
const SERVER_DIGEST = "@sha256:" + "1".repeat(64);
const CLIENT_DIGEST = "@sha256:" + "2".repeat(64);
const CI_SERVER = `docker.io/bitnamilegacy/minio:${SERVER}${SERVER_DIGEST}`;
const CI_CLIENT = `docker.io/bitnamilegacy/minio-client:${CLIENT}${CLIENT_DIGEST}`;
const UP_SERVER = "RELEASE.2025-09-07T16-13-09Z";
const UP_CLIENT = "RELEASE.2025-08-13T08-35-41Z";

// Deux fichiers minimaux et conformes, que chaque cas abîme d'un seul point.
function files(ci: string, compose: string): SourceFile[] {
  return [
    { path: CI_PATH, text: ci },
    { path: COMPOSE_PATH, text: compose },
  ];
}
const CI_OK =
  `        run: |\n` +
  `          docker run -d --name minio ${CI_SERVER}\n` +
  `          docker run --rm ${CI_CLIENT} -c "mc mb local/b"\n`;
const COMPOSE_OK =
  `  minio:\n    image: quay.io/minio/minio:${UP_SERVER}\n` +
  `  minio-init:\n    image: quay.io/minio/mc:${UP_CLIENT}\n`;

test("les vrais ci.yml et docker-compose.yml passent le garde-fou", () => {
  assert.deepEqual(minioPinViolations(realFiles()), []);
});

test("des fichiers conformes passent, avec des pins serveur et client distincts", () => {
  // L'erreur que le garde-fou ne doit PAS commettre : le serveur et le client
  // publient sur des horloges de release distinctes, côté miroir Bitnami
  // (2025.7.23 vs 2025.7.21) comme côté amont — exiger un pin unique pour les
  // deux rendrait la porte impossible à tenir.
  assert.notEqual(SERVER, CLIENT);
  assert.notEqual(UP_SERVER, UP_CLIENT);
  assert.deepEqual(minioPinViolations(files(CI_OK, COMPOSE_OK)), []);
});

test("refuse dans ci.yml un retour aux images minio/* que plus personne ne peut tirer", () => {
  // Le mode de panne du 2026-09-25 : ce n'est pas un pull instable, c'est une
  // autorisation refusée. Y revenir doit être rouge ici, pas en CI.
  for (const bad of [
    `quay.io/minio/minio:${UP_SERVER}`,
    `docker.io/minio/mc:${UP_CLIENT}`,
    `minio/minio:${UP_SERVER}`,
    `ghcr.io/minio/minio:${UP_SERVER}`,
  ]) {
    const ci = `${CI_OK}          docker run ${bad}\n`;
    const violations = minioPinViolations(files(ci, COMPOSE_OK));
    assert.equal(violations.length, 1, `${bad} : ${violations.join(" | ")}`);
    assert.match(violations[0], /anonym/);
    assert.match(violations[0], /ci\.yml:\d+/);
  }
});

test("refuse dans ci.yml un autre registre que docker.io, même miroir", () => {
  for (const registry of [
    "quay.io",
    "mirror.gcr.io",
    "mirror.docker.io",
    "docker.io.evil.example",
    "docker.io:5000",
    "evildocker.io",
  ]) {
    const ci = CI_OK.replace(
      "docker.io/bitnamilegacy/minio-client",
      `${registry}/bitnamilegacy/minio-client`,
    );
    const violations = minioPinViolations(files(ci, COMPOSE_OK));
    assert.equal(violations.length, 1, `${registry} : ${violations.join(" | ")}`);
    assert.ok(
      violations[0].includes(`pas de ${registry}.`),
      `${registry} doit être nommé comme registre refusé : ${violations[0]}`,
    );
  }
});

test("refuse une référence sans registre (Docker Hub implicite)", () => {
  // `bitnamilegacy/minio` sans registre tire bien du Hub aujourd'hui, mais
  // rien ne le dit dans le diff : le registre s'écrit.
  const ci = CI_OK.replace("docker.io/bitnamilegacy/minio:", "bitnamilegacy/minio:");
  const violations = minioPinViolations(files(ci, COMPOSE_OK));
  assert.equal(violations.length, 1, violations.join(" | "));
  assert.match(violations[0], /registre implicite/);

  const compose = COMPOSE_OK.replace(`quay.io/minio/mc:${UP_CLIENT}`, `minio/mc:${UP_CLIENT}`);
  const v2 = minioPinViolations(files(CI_OK, compose));
  assert.equal(v2.length, 1, v2.join(" | "));
  assert.match(v2[0], /docker-compose\.yml:4/);
});

test("refuse dans ci.yml un tag qui n'est pas une version Bitnami complète", () => {
  for (const bad of [
    "latest",
    "2025.7.23",
    "2025.7.23-debian-12",
    "debian-12-r5",
    "${MINIO_TAG}",
    "2025.7.23-debian-12-r5-hotfix",
    "v2025.7.23-debian-12-r5",
  ]) {
    const ci = CI_OK.replace(`minio:${SERVER}`, `minio:${bad}`);
    const violations = minioPinViolations(files(ci, COMPOSE_OK));
    assert.ok(
      violations.some((v) => /épingl/.test(v)),
      `${bad} doit être refusé comme non épinglé, obtenu : ${violations.join(" | ")}`,
    );
  }
});

test("refuse dans ci.yml une image sans tag", () => {
  const ci = CI_OK.replace(`docker.io/bitnamilegacy/minio:${SERVER}`, "docker.io/bitnamilegacy/minio");
  const violations = minioPinViolations(files(ci, COMPOSE_OK));
  assert.ok(violations.some((v) => /épingl/.test(v)), violations.join(" | "));
});

test("exige dans ci.yml le pin par digest, que le tag seul ne remplace pas", () => {
  // Un miroir que personne ne maintient est exactement l'endroit où un tag
  // peut être republié : côté CI, le digest n'est pas optionnel.
  for (const drop of [SERVER_DIGEST, CLIENT_DIGEST]) {
    const ci = CI_OK.replace(drop, "");
    const violations = minioPinViolations(files(ci, COMPOSE_OK));
    assert.equal(violations.length, 1, `${drop} : ${violations.join(" | ")}`);
    assert.match(violations[0], /digest/);
    assert.doesNotMatch(violations[0], /mal formé/);
  }
});

test("le compose accepte un tag amont épinglé sans digest", () => {
  // Politique par fichier : le digest est exigé sur le miroir, pas sur les
  // références amont du compose, qui n'ont pas changé.
  assert.deepEqual(minioPinViolations(files(CI_OK, COMPOSE_OK)), []);
});

test("refuse dans le compose un horodatage suivi d'une variante (.fips, -cpuv1, .hotfix.*)", () => {
  for (const suffix of [".fips", "-cpuv1", ".hotfix.7b3a2e1f"]) {
    const bad = `quay.io/minio/minio:${UP_SERVER}${suffix}`;
    const compose = COMPOSE_OK.replace(`quay.io/minio/minio:${UP_SERVER}`, bad);
    const violations = minioPinViolations(files(CI_OK, compose));
    assert.equal(violations.length, 1, `${bad} : ${violations.join(" | ")}`);
    // Le tag est épinglé : le dire « flottant » serait faux.
    assert.doesNotMatch(violations[0], /flottant/);
    assert.ok(
      violations[0].includes(`« ${suffix} »`),
      `${bad} doit nommer son suffixe : ${violations[0]}`,
    );
  }
});

test("refuse un digest mal formé au lieu de l'ignorer, dans les deux fichiers", () => {
  for (const digest of ["@sha256:zz", "@sha256:" + "a".repeat(63), "@md5:abc"]) {
    const compose = COMPOSE_OK.replace(
      `quay.io/minio/minio:${UP_SERVER}`,
      `quay.io/minio/minio:${UP_SERVER}${digest}`,
    );
    const v1 = minioPinViolations(files(CI_OK, compose));
    assert.equal(v1.length, 1, `${digest} : ${v1.join(" | ")}`);
    assert.match(v1[0], /mal formé/);
    assert.ok(v1[0].includes(digest));

    const ci = CI_OK.replace(SERVER_DIGEST, digest);
    const v2 = minioPinViolations(files(ci, COMPOSE_OK));
    assert.equal(v2.length, 1, `${digest} : ${v2.join(" | ")}`);
    assert.match(v2[0], /mal formé/);
  }
});

test("accepte une référence épinglée entre guillemets", () => {
  // Le tag s'arrête aux guillemets : sinon `"…r5"` serait lu comme un tag
  // non conforme.
  const ci = CI_OK.replace(CI_SERVER, `"${CI_SERVER}"`);
  const compose = COMPOSE_OK.replace(
    `quay.io/minio/minio:${UP_SERVER}`,
    `'quay.io/minio/minio:${UP_SERVER}'`,
  );
  assert.deepEqual(minioPinViolations(files(ci, compose)), []);
});

test("refuse une divergence de pin entre deux jobs du même fichier", () => {
  // Les trois jobs de ci.yml montent la même pile : un job laissé derrière
  // teste autre chose que les deux autres.
  const other = "@sha256:" + "9".repeat(64);
  const ci = CI_OK + CI_OK.replace(SERVER_DIGEST, other);
  const violations = minioPinViolations(files(ci, COMPOSE_OK));
  assert.equal(violations.length, 1, violations.join(" | "));
  assert.match(violations[0], /divergent/);
  assert.ok(violations[0].includes(SERVER_DIGEST) && violations[0].includes(other));

  const tagOther = CI_OK + CI_OK.replace(`minio:${SERVER}`, "minio:2025.7.23-debian-12-r4");
  assert.match(
    minioPinViolations(files(tagOther, COMPOSE_OK)).join(" | "),
    /divergent/,
  );
});

test("refuse une divergence de pin entre deux services du compose", () => {
  const compose = COMPOSE_OK + `  minio-other:\n    image: quay.io/minio/minio:RELEASE.2025-10-15T17-29-55Z\n`;
  const violations = minioPinViolations(files(CI_OK, compose));
  assert.equal(violations.length, 1, violations.join(" | "));
  assert.match(violations[0], /divergent/);
});

test("refuse un fichier où une image attendue n'est plus trouvée", () => {
  // Sans ce plancher, passer l'image dans une variable ou déplacer le service
  // rendrait le garde-fou vert sur zéro référence.
  const ci = `        run: |\n          docker run -d ${CI_SERVER}\n`;
  const v1 = minioPinViolations(files(ci, COMPOSE_OK));
  assert.equal(v1.length, 1, v1.join(" | "));
  assert.match(v1[0], /minio-client/);
  assert.match(v1[0], /ci\.yml/);

  const compose = `  minio:\n    image: quay.io/minio/minio:${UP_SERVER}\n`;
  const v2 = minioPinViolations(files(CI_OK, compose));
  assert.equal(v2.length, 1, v2.join(" | "));
  assert.match(v2[0], /minio\/mc/);
  assert.match(v2[0], /docker-compose\.yml/);
});

test("refuse un fichier dont la politique d'image est inconnue", () => {
  // La politique est attachée au chemin : un fichier qu'on croirait couvert
  // et qui ne l'est pas doit le dire, pas passer en vert sur rien.
  const violations = minioPinViolations([
    { path: "infra/docker-compose.prod.yml", text: CI_OK },
  ]);
  assert.equal(violations.length, 1, violations.join(" | "));
  assert.match(violations[0], /politique/);
  assert.match(violations[0], /docker-compose\.prod\.yml/);
});

test("ignore les lignes de commentaire, qui nomment les images quittées pour dire pourquoi", () => {
  const ci =
    "      # quay.io/minio/minio et docker.io/minio/mc ne répondent plus (401)\n" +
    CI_OK;
  assert.deepEqual(minioPinViolations(files(ci, COMPOSE_OK)), []);
});

test("ne confond pas une URL MinIO ni une image voisine avec les images suivies", () => {
  const ci =
    CI_OK +
    "          curl -sf http://localhost:9000/minio/health/ready\n" +
    "          docker run example.org/bitnamilegacy/minio-extra:latest\n" +
    "          docker run example.org/bitnamilegacy/minio/sidecar:latest\n";
  assert.deepEqual(minioPinViolations(files(ci, COMPOSE_OK)), []);
});

test("refuse le vrai ci.yml ramené aux images minio/* sur un seul job", () => {
  // Contrôle de mutation sur le vrai fichier : une seule occurrence modifiée,
  // les autres restent conformes.
  const [ci, compose] = realFiles();
  const mutated = ci.text.replace(
    /docker\.io\/bitnamilegacy\/minio:\S+/,
    `quay.io/minio/minio:${UP_SERVER}`,
  );
  assert.notEqual(mutated, ci.text);
  const violations = minioPinViolations([{ ...ci, text: mutated }, compose]);
  // Deux constats : l'image interdite, et la divergence des deux jobs restants
  // avec rien — le pin du miroir ne bouge pas, donc seul le premier tombe ici.
  assert.ok(violations.some((v) => /anonym/.test(v)), violations.join(" | "));
});

test("refuse le vrai ci.yml privé de son digest", () => {
  const [ci, compose] = realFiles();
  const mutated = ci.text.replace(/@sha256:[0-9a-f]{64}/, "");
  assert.notEqual(mutated, ci.text);
  const violations = minioPinViolations([{ ...ci, text: mutated }, compose]);
  assert.ok(violations.some((v) => /digest/.test(v)), violations.join(" | "));
});

test("refuse le vrai docker-compose.yml repassé en latest", () => {
  const [ci, compose] = realFiles();
  const mutated = compose.text.replace(/quay\.io\/minio\/mc:\S+/, "quay.io/minio/mc:latest");
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
  `  minio:\n    image: quay.io/minio/minio:${UP_SERVER}\n` +
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

test("accepte la version complète de PostgreSQL, en deux composantes", () => {
  // Depuis PostgreSQL 10, `16.4` est une version complète : resserrer le pin
  // `postgres:16` ne doit pas obliger à modifier le garde-fou (#262).
  for (const pin of ["postgres:16.4", "postgres:16.4-bookworm"]) {
    const text = COMPOSE_TAGS_OK.replace("postgres:16", pin);
    assert.deepEqual(composeTagViolations(composeFile(text)), [], pin);
  }
});

test("refuse un tag en deux composantes hors PostgreSQL", () => {
  // Pour les autres images, `x.y` est une série qui reçoit encore des
  // correctifs : ce n'est pas une version complète.
  for (const [from, to] of [
    ["axllent/mailpit:v1.31.2", "axllent/mailpit:v1.31"],
    ["ollama/ollama:0.34.2", "ollama/ollama:0.34"],
    ["caddy:2", "caddy:2.8"],
  ]) {
    const text = COMPOSE_TAGS_OK.replace(from, to);
    const violations = composeTagViolations(composeFile(text));
    assert.equal(violations.length, 1, `${to} : ${violations.join(" | ")}`);
    assert.ok(violations[0].includes(to));
  }
});

test("refuse pour PostgreSQL une série majeure suffixée ou flottante", () => {
  for (const pin of ["postgres:16-bookworm", "postgres:latest", "postgres:16.4.x"]) {
    const text = COMPOSE_TAGS_OK.replace("postgres:16", pin);
    assert.equal(composeTagViolations(composeFile(text)).length, 1, pin);
  }
});

test("accepte un pin par digest seul", () => {
  const digest = "@sha256:" + "0123456789abcdef".repeat(4);
  const text = COMPOSE_TAGS_OK.replace("postgres:16", `postgres${digest}`)
    .replace("axllent/mailpit:v1.31.2", `axllent/mailpit${digest}`);
  assert.deepEqual(composeTagViolations(composeFile(text)), []);
});

test("refuse un digest mal formé, seul ou derrière une version complète", () => {
  for (const [from, to] of [
    ["postgres:16", "postgres@sha256:zz"],
    ["postgres:16", "postgres@sha256:" + "a".repeat(63)],
    ["ollama/ollama:0.34.2", "ollama/ollama:0.34.2@sha256:zz"],
  ]) {
    const text = COMPOSE_TAGS_OK.replace(from, to);
    const violations = composeTagViolations(composeFile(text));
    assert.equal(violations.length, 1, `${to} : ${violations.join(" | ")}`);
    assert.match(violations[0], /digest/);
  }
});

test("un digest ne rachète pas un tag flottant écrit devant lui", () => {
  // Le tag est ce qu'on lit dans un diff : `latest@sha256:…` affiche
  // « latest » quel que soit le digest.
  const digest = "@sha256:" + "a".repeat(64);
  const text = COMPOSE_TAGS_OK.replace(
    "axllent/mailpit:v1.31.2",
    `axllent/mailpit:latest${digest}`,
  );
  assert.equal(composeTagViolations(composeFile(text)).length, 1);
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
