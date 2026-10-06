// Garde-fou des images MinIO (#159, politique par fichier depuis #138,
// compose basculé sur le miroir par #274).
//
// #158 avait sorti les références MinIO de Docker Hub (qui ne servait plus
// `minio/minio` ni `minio/mc` anonymement depuis le 2026-09-11, #157) pour
// `quay.io`, sur des tags `RELEASE.*` épinglés. Le 2026-09-25, `minio` a fermé
// les pulls anonymes partout où il publie : `quay.io` répond 401 (sur le tag
// épinglé, sur `latest` et sur la liste des tags), `docker.io/minio/minio` 401,
// `ghcr.io/minio/minio` 403. Les trois jobs de `ci.yml` qui démarrent MinIO
// sont morts en exit 125, et un `rerun` n'y change rien : c'est une
// autorisation refusée, pas un pull instable.
//
// `ci.yml` tire donc le miroir `docker.io/bitnamilegacy/minio` et
// `docker.io/bitnamilegacy/minio-client` — le dernier build librement tirable
// du vrai serveur, et c'est le vrai serveur qui compte : un faux S3 (s3mock,
// localstack) cesserait de couvrir ce que le déploiement fait tourner.
// `infra/docker-compose.yml` est resté un temps sur les références amont de
// `quay.io`, parce qu'y basculer déplace le chemin des données de la pile
// livrée (`/data` → `/bitnami/minio/data`) — et la pile livrée ne démarrait
// plus. #274 l'a basculé sur le même miroir, volume remonté sans copie (motif
// dans le compose). Le module garde **une politique par fichier** (un fichier
// sans politique n'est pas jugé), mais les deux fichiers portent aujourd'hui
// la même : celle du miroir.
//
// Ce que la porte refuse, par fichier couvert :
//   - un registre autre que celui de la politique — un miroir, un
//     sous-domaine, un port, ou pas de registre du tout (Hub implicite) ;
//   - toute référence à `minio/minio` ou `minio/mc` : ce sont les images que
//     plus personne ne peut tirer, y revenir doit être rouge ici et pas en CI
//     ni au premier `docker compose up` ;
//   - un tag qui n'est pas une version Bitnami complète
//     (`AAAA.M.J-debian-N-rN`) : `latest`, pas de tag, une série nue, une
//     variable (`${MINIO_TAG}`) ;
//   - l'absence de digest : un miroir que personne ne maintient est
//     exactement l'endroit où un tag se fait republier, donc le pin par
//     digest y est exigé ;
//   - un digest mal formé (`@sha256:zz`), au lieu de l'ignorer ;
//   - deux pins différents pour la MÊME image, dans un fichier ou d'un
//     fichier à l'autre (les jobs de `ci.yml` montent la même pile, et
//     c'est la pile que le compose fait tourner : un pin laissé derrière
//     teste autre chose) ;
//   - un fichier fourni où l'une des images attendues n'est plus trouvée :
//     sans ce plancher, passer l'image dans une variable rendrait la porte
//     verte sur zéro référence ;
//   - un fichier fourni dont le chemin n'a pas de politique : la porte dit
//     qu'elle ne couvre pas ce fichier au lieu de le déclarer conforme.
//
// Ce qu'il accepte exprès : des pins différents entre le serveur et le client.
// Les deux dépôts publient sur des horloges de release distinctes (2025.7.23
// vs 2025.7.21 sur le miroir, même constat en amont dans #159) : exiger un pin
// unique pour les deux rendrait la porte impossible à tenir.
//
// Limites, dites pour ce qu'elles sont :
//   - les lignes dont le premier caractère non blanc est `#` sont ignorées
//     (commentaires YAML et shell : ceux de `ci.yml` nomment les images
//     quittées pour dire pourquoi). Un commentaire en fin de ligne, lui, est
//     lu : s'il cite une référence interdite, la porte est rouge à tort —
//     bruyamment, sur la bonne ligne ;
//   - c'est une lecture textuelle, pas un parseur YAML ni shell. Une
//     référence assemblée à l'exécution (`"$REGISTRY/minio/minio"`) n'est pas
//     vue — mais elle n'a pas non plus de registre littéral, et le plancher
//     « image absente » ne l'attrape que si c'était la seule occurrence ;
//   - un tag épinglé rend la dérive visible dans un diff, il ne la rend pas
//     impossible : seul le digest le fait, et c'est pourquoi il est exigé
//     côté miroir.

export type SourceFile = { path: string; text: string };

export const CI_PATH = ".github/workflows/ci.yml";
export const COMPOSE_PATH = "infra/docker-compose.yml";

// Un tag de release MinIO amont, à la seconde près (lu par la porte des
// tags du compose, plus bas).
const PINNED_TAG = /^RELEASE\.\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}Z$/;

// Une version Bitnami complète : `2025.7.23-debian-12-r5`.
const BITNAMI_TAG = /^\d{4}\.\d{1,2}\.\d{1,2}-debian-\d+-r\d+$/;

// Un digest d'image bien formé.
const DIGEST = /^@sha256:[0-9a-f]{64}$/;

type Policy = {
  /** Registre exact attendu. */
  registry: string;
  /** Les images que ce fichier doit référencer, toutes. */
  images: readonly string[];
  /** Les images dont la seule présence est une violation. */
  forbidden: readonly string[];
  /** Le tag attendu, et sa description pour le message. */
  tag: RegExp;
  tagExpected: string;
  /** Le pin par digest est-il exigé ? */
  digestRequired: boolean;
  /** Pourquoi ce registre, en une phrase, pour le message. */
  registryWhy: string;
};

const MIRROR: Policy = {
  registry: "docker.io",
  images: ["bitnamilegacy/minio", "bitnamilegacy/minio-client"],
  forbidden: ["minio/minio", "minio/mc"],
  tag: BITNAMI_TAG,
  tagExpected: "une version Bitnami complète, AAAA.M.J-debian-N-rN",
  digestRequired: true,
  registryWhy:
    "le miroir bitnamilegacy n'est publié que sur Docker Hub (#138) ; " +
    "minio/* n'est plus tirable anonymement nulle part.",
};

const POLICIES: ReadonlyMap<string, Policy> = new Map([
  [CI_PATH, MIRROR],
  [COMPOSE_PATH, MIRROR],
]);

/** Les images suivies par fichier couvert, pour les appelants. */
export const MINIO_IMAGES: ReadonlyMap<string, readonly string[]> = new Map(
  [...POLICIES].map(([path, policy]) => [path, policy.images]),
);

function escapeForRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&");
}

/**
 * Une référence `[registre/…/]<image>[:tag][@sha256:…]`, pour les images
 * suivies ET interdites d'une politique.
 *   - le lookbehind empêche de démarrer au milieu d'un chemin ;
 *   - les noms sont alternés du plus long au plus court, pour que
 *     `bitnamilegacy/minio-client` ne soit pas lu comme `bitnamilegacy/minio`
 *     suivi d'autre chose ;
 *   - le lookahead après le nom écarte `…/minio-extra` et `…/minio/sidecar`,
 *     et une URL comme `http://localhost:9000/minio/health/ready` ne nomme
 *     aucune image suivie ;
 *   - le tag s'arrête aux blancs, guillemets, antislash et `@` ; ce qui suit
 *     `@` est pris tel quel et validé ensuite par `DIGEST`, pour qu'un digest
 *     mal formé soit refusé plutôt qu'ignoré.
 */
function referencePattern(policy: Policy): RegExp {
  const names = [...policy.images, ...policy.forbidden]
    .slice()
    .sort((a, b) => b.length - a.length)
    .map(escapeForRegExp)
    .join("|");
  return new RegExp(
    `(?<![\\w.\\/:@$-])((?:[\\w.-]+(?::\\d+)?\\/)*)(${names})(?![\\w.\\/-])` +
      `(?::([^\\s"'\`\\\\@]*))?(@[^\\s"'\`\\\\]*)?`,
    "g",
  );
}

type Occurrence = {
  where: string;
  image: string;
  registry: string;
  tag: string | undefined;
  digest: string;
  raw: string;
  policy: Policy;
};

function occurrences(file: SourceFile, policy: Policy): Occurrence[] {
  const pattern = referencePattern(policy);
  const found: Occurrence[] = [];
  file.text.split("\n").forEach((line, index) => {
    if (line.trimStart().startsWith("#")) return;
    for (const m of line.matchAll(pattern)) {
      found.push({
        where: `${file.path}:${index + 1}`,
        registry: m[1].replace(/\/$/, ""),
        image: m[2],
        tag: m[3],
        digest: m[4] ?? "",
        raw: m[0],
        policy,
      });
    }
  });
  return found;
}

/**
 * Rend la liste des violations (vide si la porte est tenue). Chaque fichier
 * fourni est jugé sur la politique attachée à son chemin, et doit référencer
 * toutes les images que celle-ci attend. **Tous** les fichiers couverts
 * doivent être fournis : appelée sur un seul, la porte ne sait rien de
 * l'autre, et un vert vaudrait affirmation sans lecture.
 */
export function minioPinViolations(files: ReadonlyArray<SourceFile>): string[] {
  const violations: string[] = [];
  const sound: Occurrence[] = [];

  for (const path of POLICIES.keys()) {
    if (!files.some((f) => f.path === path)) {
      violations.push(
        `${path} : fichier couvert non fourni à la porte. Les deux fichiers ` +
          "se lisent ensemble : sur un seul, rien n'est prouvé de l'autre.",
      );
    }
  }

  for (const file of files) {
    const policy = POLICIES.get(file.path);
    if (policy === undefined) {
      violations.push(
        `${file.path} : aucune politique d'image connue pour ce fichier. ` +
          `Les fichiers couverts sont ${[...POLICIES.keys()].join(", ")} ; ` +
          "un fichier hors de cette liste n'est pas jugé conforme, il n'est " +
          "pas jugé du tout.",
      );
      continue;
    }

    const found = occurrences(file, policy);
    for (const image of policy.images) {
      if (!found.some((o) => o.image === image)) {
        violations.push(
          `${file.path} : aucune référence à ${policy.registry}/${image} ` +
            "trouvée. Le garde-fou ne peut rien prouver sur une image qu'il " +
            "ne voit pas (déplacée, passée dans une variable ?).",
        );
      }
    }

    for (const o of found) {
      if (policy.forbidden.includes(o.image)) {
        violations.push(
          `${o.where} : \`${o.raw}\` — ${o.image} n'est plus tirable ` +
            "anonymement (quay.io 401, Docker Hub 401, ghcr.io 403 au " +
            "2026-09-25) : ce fichier passe par le miroir " +
            `${policy.registry}/${policy.images[0]}. Un rerun n'y change ` +
            "rien, c'est une autorisation refusée.",
        );
        continue;
      }

      let unsound = false;
      if (o.registry !== policy.registry) {
        violations.push(
          `${o.where} : \`${o.raw}\` — ${o.image} doit venir de ` +
            `${policy.registry}, pas de ` +
            `${o.registry || "Docker Hub (registre implicite)"}. ` +
            policy.registryWhy,
        );
        unsound = true;
      }

      if (o.tag === undefined || !policy.tag.test(o.tag)) {
        violations.push(
          `${o.where} : \`${o.raw}\` — tag non épinglé ` +
            `(${o.tag === undefined ? "aucun tag" : `« ${o.tag} »`}). ` +
            `Attendu : ${policy.tagExpected} ; un tag flottant laisse ` +
            "l'image bouger sous la CI sans rien signaler.",
        );
        unsound = true;
      }

      if (o.digest === "") {
        if (policy.digestRequired) {
          violations.push(
            `${o.where} : \`${o.raw}\` — pin par digest manquant. Attendu : ` +
              "@sha256: suivi de 64 caractères hexadécimaux, derrière le " +
              "tag. Un miroir que personne ne maintient est exactement " +
              "l'endroit où un tag se fait republier.",
          );
          unsound = true;
        }
      } else if (!DIGEST.test(o.digest)) {
        violations.push(
          `${o.where} : \`${o.raw}\` — digest mal formé « ${o.digest} ». ` +
            "Attendu : @sha256: suivi de 64 caractères hexadécimaux.",
        );
        unsound = true;
      }

      if (!unsound) sound.push(o);
    }
  }

  // Divergence : par image, sur les seules références saines (une référence
  // `latest` ou un digest mal formé a déjà sa propre violation, ne pas la
  // compter deux fois), tous fichiers confondus. Le serveur et le client
  // sont comparés chacun de leur côté.
  const images = new Set(sound.map((o) => o.image));
  for (const image of images) {
    const byPin = new Map<string, string[]>();
    for (const o of sound) {
      if (o.image !== image) continue;
      const pin = (o.tag ?? "") + o.digest;
      byPin.set(pin, [...(byPin.get(pin) ?? []), o.where]);
    }
    if (byPin.size > 1) {
      const detail = [...byPin]
        .map(([pin, wheres]) => `  ${pin} : ${wheres.join(", ")}`)
        .join("\n");
      violations.push(
        `${image} : pins divergents — une même image doit porter le même ` +
          `tag et le même digest partout.\n${detail}`,
      );
    }
  }

  return violations;
}

// ---------------------------------------------------------------------------
// Tags des lignes `image:` (#160, durci par #310).
//
// Le garde-fou ci-dessus ne voit que MinIO. `infra/docker-compose.yml`
// laissait `axllent/mailpit` et `ollama/ollama` sur `latest` : la même
// fragilité, sur les postes de développement plutôt qu'en CI (le compose ne
// tourne pas en CI). Chaque ligne `image:` d'un fichier (le compose, et les
// `services:` des jobs de `ci.yml`) doit donc porter une version complète
// (`1.2.3`, `v1.2.3`, suffixe toléré) ou un horodatage `RELEASE.*` MinIO,
// avec ou sans digest, ou un digest seul (`image@sha256:…`), le seul pin
// qu'un registre ne peut pas republier (#262).
//
// Version complète de PostgreSQL : deux composantes (`16.4`, suffixe toléré,
// `16.4-bookworm`), car depuis PostgreSQL 10 la deuxième est déjà le
// correctif. Pour les autres images, `x.y` reste une série et est refusé.
// Seul le nom exact `postgres` est reconnu : `library/postgres:16.4` ou un
// registre explicite sont refusés, bruyamment.
//
// Un digest mal formé est refusé ; un digest bien formé ne rachète pas un tag
// flottant écrit devant lui (`latest@sha256:…`) : le tag est ce qu'on lit
// dans un diff.
//
// `postgres` et `caddy` (#310) : jusque-là laissés en exception sur leur
// série majeure (`postgres:16`, `caddy:2`), dont le tag bouge sans commit —
// build non reproductible, régression amont sans trace. Ils exigent
// désormais **tag complet ET digest** (`postgres:16.15@sha256:…`,
// `caddy:2.11.4@sha256:…`), ni l'un sans l'autre : le digest seul ne dit
// plus dans un diff quelle version tourne, et c'est le tag que le robot de
// mise à jour (`renovate.json`) compare aux nouvelles versions ; le tag seul
// peut être republié. Les correctifs qu'apportait la série flottante
// arrivent maintenant par les PR du robot, une par version ou republication.
// La règle suit le dernier segment du nom : un registre explicite
// (`docker.io/library/caddy`) ne fait pas sortir l'image de la règle.
//
// Trois écritures d'une image sont lues (#374), dans tout fichier fourni :
//   - les lignes `image:` (services du compose, `services:` des jobs de
//     `ci.yml`, forme bloc de `container:`) ;
//   - la forme ligne de `container:` d'un job (`container: postgres:16`) ;
//   - `docker run` : la commande est suivie sur ses lignes continuées par
//     `\`, ses options sont sautées (avec leur valeur, sauf les drapeaux
//     booléens connus), et le premier argument restant est l'image. Une
//     option inconnue est supposée prendre une valeur : si elle n'en prend
//     pas, c'est la commande qui est lue comme image, et refusée — l'erreur
//     est bruyante, jamais un vert.
//
// Limites : lecture textuelle (pas un parseur YAML ni shell) ; les lignes dont
// le premier caractère non blanc est `#` sont ignorées ; une image passée dans
// une variable (`image: ${X}`, `docker run $IMG`) est refusée comme non
// épinglée, ce qui est le comportement voulu. `docker create`, `docker pull`
// et `uses: docker://…` ne sont pas lus.

const DIGEST_PINNED: ReadonlyArray<string> = ["postgres", "caddy"];

const FULL_VERSION = /^v?\d+\.\d+\.\d+(?:[-+.][\w.-]+)?$/;

// Images dont la version complète n'a que deux composantes.
const TWO_COMPONENT_IMAGES: ReadonlyArray<string> = ["postgres"];
const TWO_COMPONENT_VERSION = /^\d+\.\d+(?:-[\w.-]+)?$/;

// `image: <référence>` et `container: <référence>`, guillemets simples ou
// doubles tolérés. L'ancre `^\s*` écarte d'elle-même les lignes de
// commentaire ; un `container:` sans valeur (forme bloc) ne correspond pas, son
// image est lue sur sa ligne `image:`.
const IMAGE_LINE = /^\s*(?:image|container):\s*["']?([^\s"'#]+)/;

// `docker run` en début de commande : début de ligne, ou après un blanc ou un
// séparateur shell (`;`, `&&`, `|`, `(`). `sudo docker run` est lu.
const DOCKER_RUN = /(?:^|[\s;&|(])docker\s+run(?=\s|$)/;

// Options de `docker run` qui ne prennent pas de valeur. Les autres sont
// supposées en prendre une (voir l'en-tête de section).
const RUN_BOOLEAN_LONG: ReadonlySet<string> = new Set([
  "--detach",
  "--rm",
  "--interactive",
  "--tty",
  "--init",
  "--privileged",
  "--publish-all",
  "--read-only",
  "--no-healthcheck",
  "--oom-kill-disable",
  "--quiet",
]);
const RUN_BOOLEAN_SHORT = "ditPq";

// Les registres qui désignent Docker Hub.
const HUB_REGISTRIES: ReadonlyArray<string> = [
  "docker.io",
  "index.docker.io",
  "registry-1.docker.io",
];

type ImageLine = {
  where: string;
  raw: string;
  name: string;
  /** Dernier segment du nom : `caddy` pour `docker.io/library/caddy`. */
  base: string;
  tag: string | undefined;
  digest: string;
};

function parseReference(where: string, raw: string): ImageLine {
  const at = raw.indexOf("@");
  const ref = at === -1 ? raw : raw.slice(0, at);
  const lastSlash = ref.lastIndexOf("/");
  const colon = ref.indexOf(":", lastSlash + 1);
  const name = colon === -1 ? ref : ref.slice(0, colon);
  return {
    where,
    raw,
    name,
    base: name.slice(name.lastIndexOf("/") + 1),
    tag: colon === -1 ? undefined : ref.slice(colon + 1),
    digest: at === -1 ? "" : raw.slice(at),
  };
}

/**
 * Le nom d'image tel que le registre le résout (#374) : Docker Hub, implicite
 * ou écrit (`docker.io`, `index.docker.io`), perd son registre, et une image
 * officielle perd `library/`. `caddy`, `library/caddy` et
 * `docker.io/library/caddy` donnent `caddy` ; `ghcr.io/x/caddy` reste tel quel.
 */
function canonicalName(name: string): string {
  const slash = name.indexOf("/");
  const first = slash === -1 ? "" : name.slice(0, slash);
  const hasRegistry = /[.:]/.test(first) || first === "localhost";
  if (hasRegistry && !HUB_REGISTRIES.includes(first)) return name;
  const path = hasRegistry ? name.slice(slash + 1) : name;
  return /^library\/[^/]+$/.test(path) ? path.slice("library/".length) : path;
}

/**
 * L'image d'un `docker run` qui commence sur la ligne `start`, suivie sur
 * ses lignes continuées par `\` : le premier argument qui n'est ni une option
 * ni sa valeur. `undefined` si la commande s'arrête avant.
 */
function dockerRunImage(
  file: SourceFile,
  lines: ReadonlyArray<string>,
  start: number,
  after: string,
): ImageLine | undefined {
  const tokens: { text: string; line: number }[] = [];
  let index = start;
  let text = after;
  for (;;) {
    for (const t of text.match(/"[^"]*"|'[^']*'|\S+/g) ?? []) {
      if (t !== "\\") tokens.push({ text: t, line: index });
    }
    if (!text.trimEnd().endsWith("\\") || index + 1 >= lines.length) break;
    index += 1;
    text = lines[index];
  }

  for (let i = 0; i < tokens.length; i += 1) {
    const t = tokens[i].text;
    if (t.startsWith("--")) {
      if (!t.includes("=") && !RUN_BOOLEAN_LONG.has(t)) i += 1;
      continue;
    }
    if (t.startsWith("-") && t.length > 1) {
      // Grappe de drapeaux courts (`-dit`) : la première lettre qui prend une
      // valeur la prend collée (`-p5432:5432`) ou dans l'argument suivant.
      const letters = t.slice(1);
      const valued = [...letters].findIndex((c) => !RUN_BOOLEAN_SHORT.includes(c));
      if (valued === letters.length - 1) i += 1;
      continue;
    }
    const raw = t.replace(/^["']|["']$/g, "").replace(/[;&|)].*$/, "");
    return parseReference(`${file.path}:${tokens[i].line + 1}`, raw);
  }
  return undefined;
}

function imageLines(file: SourceFile): ImageLine[] {
  const found: ImageLine[] = [];
  const lines = file.text.split("\n");
  lines.forEach((line, index) => {
    if (line.trimStart().startsWith("#")) return;
    const m = line.match(IMAGE_LINE);
    if (m) found.push(parseReference(`${file.path}:${index + 1}`, m[1]));
    const run = DOCKER_RUN.exec(line);
    if (run) {
      const image = dockerRunImage(
        file,
        lines,
        index,
        line.slice(run.index + run[0].length),
      );
      if (image) found.push(image);
    }
  });
  return found;
}

/**
 * Rend la liste des violations (vide si la porte est tenue) pour les images
 * d'un fichier : lignes `image:` (compose, `services:` des jobs de `ci.yml`),
 * `container:` et `docker run` (#374). Un fichier où aucune image n'est lue
 * est lui-même une violation.
 */
export function composeTagViolations(file: SourceFile): string[] {
  const violations: string[] = [];
  const lines = imageLines(file);

  for (const { where, raw, name, base, tag, digest } of lines) {
    if (digest !== "" && !DIGEST.test(digest)) {
      violations.push(
        `${where} : \`${raw}\` — digest mal formé « ${digest} ». Attendu : ` +
          "@sha256: suivi de 64 caractères hexadécimaux.",
      );
      continue;
    }

    const fullTag =
      tag !== undefined &&
      (FULL_VERSION.test(tag) ||
        PINNED_TAG.test(tag) ||
        (TWO_COMPONENT_IMAGES.includes(name) && TWO_COMPONENT_VERSION.test(tag)));

    if (DIGEST_PINNED.includes(base)) {
      if (!fullTag) {
        violations.push(
          `${where} : \`${raw}\` — tag non épinglé ` +
            `(${tag === undefined ? "aucun tag" : `« ${tag} »`}). Attendu pour ` +
            `${base} : une version complète (16.15 pour postgres, 2.11.4 ` +
            "pour caddy) suivie de son digest @sha256:… (#310). Un tag de " +
            "série bouge sans commit ; un digest nu ne dit pas quelle " +
            "version tourne.",
        );
      } else if (digest === "") {
        violations.push(
          `${where} : \`${raw}\` — pin par digest manquant. Attendu pour ` +
            `${base} : ${tag}@sha256: suivi de 64 caractères hexadécimaux ` +
            "(#310) ; un tag peut être republié sous le même nom.",
        );
      }
      continue;
    }

    const ok = tag === undefined ? digest !== "" : fullTag;
    if (!ok) {
      violations.push(
        `${where} : \`${raw}\` — tag non épinglé ` +
          `(${tag === undefined ? "aucun tag" : `« ${tag} »`}). Attendu : une ` +
          "version complète (1.2.3 ; 16.4 pour postgres) ou un digest " +
          "(@sha256:…). Un tag flottant laisse l'image bouger sous une pile " +
          "existante sans rien signaler.",
      );
    }
  }

  if (lines.length === 0) {
    violations.push(
      `${file.path} : aucune image trouvée (ligne \`image:\`, ` +
        "`container:` ou `docker run`). Le garde-fou ne peut " +
        "rien prouver sur des images qu'il ne voit pas.",
    );
  }
  return violations;
}

/**
 * Un même pin pour `postgres` et pour `caddy`, tous fichiers confondus
 * (#310) : les jobs de `ci.yml` jouent les migrations sur le Postgres que le
 * compose fait tourner, un job laissé sur l'ancien pin teste autre chose. Le
 * robot de mise à jour bouge toutes les occurrences d'une image dans la même
 * PR ; une PR qui n'en bouge qu'une est rouge ici. Les lignes sans tag ou au
 * digest mal formé ne sont pas comparées (`composeTagViolations` les refuse
 * déjà). Les images sont groupées par nom résolu (`canonicalName`, #374) :
 * `caddy` et `docker.io/library/caddy` sont comparés entre eux.
 */
export function pinnedImageDivergence(files: ReadonlyArray<SourceFile>): string[] {
  const violations: string[] = [];
  const byImage = new Map<string, Map<string, string[]>>();
  for (const file of files) {
    for (const line of imageLines(file)) {
      if (!DIGEST_PINNED.includes(line.base)) continue;
      if (line.tag === undefined || !DIGEST.test(line.digest)) continue;
      const image = canonicalName(line.name);
      const pins = byImage.get(image) ?? new Map<string, string[]>();
      const pin = `${line.tag}${line.digest}`;
      pins.set(pin, [...(pins.get(pin) ?? []), line.where]);
      byImage.set(image, pins);
    }
  }
  for (const [image, pins] of byImage) {
    if (pins.size <= 1) continue;
    const detail = [...pins]
      .map(([pin, wheres]) => `  ${pin} : ${wheres.join(", ")}`)
      .join("\n");
    violations.push(
      `${image} : pins divergents — une même image doit porter le même tag ` +
        `et le même digest partout (#310).\n${detail}`,
    );
  }
  return violations;
}
