// Garde-fou des images MinIO (#159, politique par fichier depuis #138).
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
// `infra/docker-compose.yml`, lui, reste sur les références amont : y basculer
// change le chemin des données de la pile livrée (`/data` →
// `/bitnami/minio/data`) et se décide à part. Les deux fichiers ne bougent
// donc plus ensemble, et ce module porte **une politique par fichier**.
//
// Ce que la porte refuse, par fichier couvert :
//   - un registre autre que celui de la politique — un miroir, un
//     sous-domaine, un port, ou pas de registre du tout (Hub implicite) ;
//   - dans `ci.yml`, toute référence à `minio/minio` ou `minio/mc` : ce sont
//     les images que plus personne ne peut tirer, y revenir doit être rouge
//     ici et pas en CI ;
//   - un tag qui n'est pas celui attendu par la politique — version Bitnami
//     complète (`AAAA.M.J-debian-N-rN`) côté miroir, horodatage
//     `RELEASE.AAAA-MM-JJTHH-MM-SSZ` côté amont : `latest`, pas de tag, une
//     série nue, une variable (`${MINIO_TAG}`) ;
//   - côté amont, un horodatage suivi d'une variante (`.fips`, `-cpuv1`,
//     `.hotfix.*`) : le tag est épinglé, mais ce n'est pas la release
//     publiée ; l'accepter se décide ici, avec son propre message plutôt que
//     « tag flottant » ;
//   - côté miroir, l'absence de digest : un miroir que personne ne maintient
//     est exactement l'endroit où un tag se fait republier, donc le pin par
//     digest y est exigé (il reste optionnel sur les références amont, où il
//     ne l'était pas non plus avant) ;
//   - un digest mal formé (`@sha256:zz`), au lieu de l'ignorer ;
//   - deux pins différents pour la MÊME image (les trois jobs de `ci.yml`
//     montent la même pile ; un job laissé derrière teste autre chose) ;
//   - un fichier fourni où l'une des images attendues n'est plus trouvée :
//     sans ce plancher, passer l'image dans une variable rendrait la porte
//     verte sur zéro référence ;
//   - un fichier fourni dont le chemin n'a pas de politique : la porte dit
//     qu'elle ne couvre pas ce fichier au lieu de le déclarer conforme.
//
// Ce qu'il accepte exprès : des pins différents entre le serveur et le client.
// Les deux dépôts publient sur des horloges de release distinctes, côté
// miroir (2025.7.23 vs 2025.7.21) comme côté amont (constat de #159) : exiger
// un pin unique pour les deux rendrait la porte impossible à tenir.
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

// Un tag de release MinIO amont, à la seconde près.
const PINNED_TAG = /^RELEASE\.\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}Z$/;

// Le même horodatage, suivi d'un suffixe de variante (capturé).
const VARIANT_TAG = /^RELEASE\.\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}Z(.+)$/;

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
  /** Détection d'une variante derrière un tag épinglé (amont seulement). */
  variant?: RegExp;
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

const UPSTREAM: Policy = {
  registry: "quay.io",
  images: ["minio/minio", "minio/mc"],
  forbidden: [],
  tag: PINNED_TAG,
  tagExpected: "un horodatage RELEASE.AAAA-MM-JJTHH-MM-SSZ",
  digestRequired: false,
  variant: VARIANT_TAG,
  registryWhy: "Docker Hub ne sert plus minio/* anonymement (#157).",
};

const POLICIES: ReadonlyMap<string, Policy> = new Map([
  [CI_PATH, MIRROR],
  [COMPOSE_PATH, UPSTREAM],
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
 * toutes les images que celle-ci attend.
 */
export function minioPinViolations(files: ReadonlyArray<SourceFile>): string[] {
  const violations: string[] = [];
  const sound: Occurrence[] = [];

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

      const variant = policy.variant && o.tag?.match(policy.variant);
      if (variant) {
        violations.push(
          `${o.where} : \`${o.raw}\` — horodatage suivi du suffixe ` +
            `« ${variant[1]} » (variante ou correctif de la release). ` +
            `Attendu : ${policy.tagExpected}, nu ; une variante s'ajoute au ` +
            "garde-fou en connaissance de cause.",
        );
        unsound = true;
      } else if (o.tag === undefined || !policy.tag.test(o.tag)) {
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
  // compter deux fois). Le serveur et le client sont comparés chacun de leur
  // côté, et les images du miroir ne croisent pas celles de l'amont.
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
// Tags des autres images du compose (#160).
//
// Le garde-fou ci-dessus ne voit que MinIO. `infra/docker-compose.yml`
// laissait `axllent/mailpit` et `ollama/ollama` sur `latest` : la même
// fragilité, sur les postes de développement plutôt qu'en CI (le compose ne
// tourne pas en CI). Chaque ligne `image:` du compose doit donc porter une
// version complète (`1.2.3`, `v1.2.3`, suffixe toléré) ou un horodatage
// `RELEASE.*` MinIO, avec ou sans digest, ou un digest seul
// (`image@sha256:…`), le seul pin qu'un registre ne peut pas republier (#262).
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
// Deux exceptions, exactes et voulues, sur leur série majeure :
//   - `postgres:16` : une série majeure est le contrat de compatibilité sur
//     lequel les migrations sont jouées, c'est aussi la série des jobs de
//     `ci.yml`, et les versions mineures de PostgreSQL sont des correctifs,
//     souvent de sécurité, qu'un pin au patch près obligerait à suivre à la
//     main ;
//   - `caddy:2` : c'est le frontal exposé à Internet (TLS, en-têtes) ; y
//     recevoir les correctifs sans bump manuel pèse plus que la
//     reproductibilité au patch près.
// Changer de série (`postgres:17`) passe par cette table, pas par un simple
// diff du compose.
//
// Limites : lecture textuelle des lignes `image:` (pas un parseur YAML) ; une
// image passée dans une variable (`image: ${X}`) est refusée comme non
// épinglée, ce qui est le comportement voulu. Le fichier `ci.yml` n'est pas
// couvert ici : hors MinIO, il n'y référence que `postgres:16`.

const MAJOR_SERIES_ALLOWED: ReadonlyArray<string> = ["postgres:16", "caddy:2"];

const FULL_VERSION = /^v?\d+\.\d+\.\d+(?:[-+.][\w.-]+)?$/;

// Images dont la version complète n'a que deux composantes.
const TWO_COMPONENT_IMAGES: ReadonlyArray<string> = ["postgres"];
const TWO_COMPONENT_VERSION = /^\d+\.\d+(?:-[\w.-]+)?$/;

// `image: <référence>`, guillemets simples ou doubles tolérés. L'ancre
// `^\s*image:` écarte d'elle-même les lignes de commentaire.
const IMAGE_LINE = /^\s*image:\s*["']?([^\s"'#]+)/;

/**
 * Rend la liste des violations (vide si la porte est tenue) pour les lignes
 * `image:` d'un fichier compose. Un fichier sans aucune ligne `image:` est
 * lui-même une violation.
 */
export function composeTagViolations(file: SourceFile): string[] {
  const violations: string[] = [];
  let seen = 0;

  file.text.split("\n").forEach((line, index) => {
    const m = line.match(IMAGE_LINE);
    if (!m) return;
    seen += 1;
    const raw = m[1];
    const at = raw.indexOf("@");
    const ref = at === -1 ? raw : raw.slice(0, at);
    const digest = at === -1 ? "" : raw.slice(at);
    const lastSlash = ref.lastIndexOf("/");
    const colon = ref.indexOf(":", lastSlash + 1);
    const name = colon === -1 ? ref : ref.slice(0, colon);
    const tag = colon === -1 ? undefined : ref.slice(colon + 1);

    if (digest !== "" && !DIGEST.test(digest)) {
      violations.push(
        `${file.path}:${index + 1} : \`${raw}\` — digest mal formé ` +
          `« ${digest} ». Attendu : @sha256: suivi de 64 caractères ` +
          "hexadécimaux.",
      );
      return;
    }

    const ok =
      tag === undefined
        ? digest !== ""
        : FULL_VERSION.test(tag) ||
          PINNED_TAG.test(tag) ||
          (TWO_COMPONENT_IMAGES.includes(name) &&
            TWO_COMPONENT_VERSION.test(tag)) ||
          MAJOR_SERIES_ALLOWED.includes(ref);
    if (!ok) {
      violations.push(
        `${file.path}:${index + 1} : \`${raw}\` — tag non épinglé ` +
          `(${tag === undefined ? "aucun tag" : `« ${tag} »`}). Attendu : une ` +
          "version complète (1.2.3 ; 16.4 pour postgres), un digest " +
          "(@sha256:…) ou, par exception, " +
          `${MAJOR_SERIES_ALLOWED.join(" / ")}. Un tag flottant laisse ` +
          "l'image bouger sous une pile existante sans rien signaler.",
      );
    }
  });

  if (seen === 0) {
    violations.push(
      `${file.path} : aucune ligne \`image:\` trouvée. Le garde-fou ne peut ` +
        "rien prouver sur des images qu'il ne voit pas.",
    );
  }
  return violations;
}
