// Garde-fou des images MinIO (#159).
//
// #158 a sorti les références MinIO de Docker Hub (qui ne sert plus
// `minio/minio` ni `minio/mc` anonymement depuis le 2026-09-11, #157) pour
// `quay.io`, sur des tags `RELEASE.*` épinglés : c'est un `latest` non épinglé
// qui avait laissé les images bouger sous la CI sans rien signaler. Ce choix
// n'était porté que par des commentaires. Ce module en fait une porte, lue par
// `image-pins.test.ts` sur les vrais `.github/workflows/ci.yml` et
// `infra/docker-compose.yml` — même motif que `wiringViolation` (#123).
//
// Ce qu'il refuse, pour `minio/minio` et `minio/mc` :
//   - un registre autre que `quay.io` — `docker.io/…`, un miroir, ou pas de
//     registre du tout (Docker Hub implicite) ;
//   - un tag qui n'est pas un horodatage `RELEASE.AAAA-MM-JJTHH-MM-SSZ` :
//     `latest`, pas de tag, une série nue (`RELEASE`, `RELEASE.2025-09-07`),
//     une variable (`${MINIO_TAG}`) ;
//   - deux tags différents pour la MÊME image, entre fichiers ou entre deux
//     jobs d'un même fichier ;
//   - un fichier fourni où l'une des deux images n'est plus trouvée : sans ce
//     plancher, passer l'image dans une variable rendrait la porte verte sur
//     zéro référence.
//
// Ce qu'il accepte exprès : des tags différents ENTRE `minio/minio` et
// `minio/mc`. Les deux dépôts publient sur des horloges de release distinctes
// et leurs tags `RELEASE.*` n'ont aucune valeur commune (constat de #159) :
// exiger un tag unique pour les deux rendrait la porte impossible à tenir.
//
// Limites, dites pour ce qu'elles sont :
//   - les lignes dont le premier caractère non blanc est `#` sont ignorées
//     (commentaires YAML et shell : les commentaires de #158 nomment
//     `docker.io/minio/minio` pour dire pourquoi on l'a quitté). Un
//     commentaire en fin de ligne, lui, est lu : s'il cite une référence
//     Docker Hub, la porte est rouge à tort — bruyamment, sur la bonne ligne ;
//   - c'est une lecture textuelle, pas un parseur YAML ni shell. Une
//     référence assemblée à l'exécution (`"$REGISTRY/minio/minio"`) n'est pas
//     vue comme une référence Docker Hub — mais elle n'a pas non plus de
//     registre `quay.io` littéral, et le plancher « image absente » ne
//     l'attrape que si c'était la seule occurrence du fichier ;
//   - un tag épinglé rend la dérive visible dans un diff, il ne la rend pas
//     impossible : `quay.io` peut republier un tag. Seul un pin par digest
//     (`@sha256:`) le ferait ; un digest ajouté derrière le tag est accepté,
//     et compte dans la comparaison entre fichiers.

export type SourceFile = { path: string; text: string };

export const MINIO_IMAGES = ["minio/minio", "minio/mc"] as const;

const REGISTRY = "quay.io";

// Un tag de release MinIO complet, à la seconde près.
const PINNED_TAG = /^RELEASE\.\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}Z$/;

// Une référence `[registre/…/]minio/(minio|mc)[:tag][@sha256:…]`.
//   - le lookbehind empêche de démarrer au milieu d'un chemin
//     (`…/minio/minio` est pris depuis le début de son préfixe) ;
//   - le lookahead après le nom écarte `minio/minio-extra`, et une URL comme
//     `http://localhost:9000/minio/health/ready` ne nomme aucune des deux ;
//   - le tag s'arrête aux blancs, guillemets, antislash et `@`.
const REFERENCE =
  /(?<![\w.\/:@$-])((?:[\w.-]+(?::\d+)?\/)*)(minio\/(?:minio|mc))(?![\w.\/-])(?::([^\s"'`\\@]*))?(@sha256:[0-9a-f]{64})?/g;

type Occurrence = {
  where: string;
  image: string;
  registry: string;
  tag: string | undefined;
  digest: string;
  raw: string;
};

function occurrences(file: SourceFile): Occurrence[] {
  const found: Occurrence[] = [];
  file.text.split("\n").forEach((line, index) => {
    if (line.trimStart().startsWith("#")) return;
    for (const m of line.matchAll(REFERENCE)) {
      found.push({
        where: `${file.path}:${index + 1}`,
        registry: m[1].replace(/\/$/, ""),
        image: m[2],
        tag: m[3],
        digest: m[4] ?? "",
        raw: m[0],
      });
    }
  });
  return found;
}

/**
 * Rend la liste des violations (vide si la porte est tenue). Chaque fichier
 * fourni doit référencer les deux images MinIO.
 */
export function minioPinViolations(files: ReadonlyArray<SourceFile>): string[] {
  const violations: string[] = [];
  const all: Occurrence[] = [];

  for (const file of files) {
    const found = occurrences(file);
    all.push(...found);
    for (const image of MINIO_IMAGES) {
      if (!found.some((o) => o.image === image)) {
        violations.push(
          `${file.path} : aucune référence à ${REGISTRY}/${image} trouvée. ` +
            "Le garde-fou ne peut rien prouver sur une image qu'il ne voit " +
            "pas (déplacée, passée dans une variable ?).",
        );
      }
    }
  }

  for (const o of all) {
    if (o.registry !== REGISTRY) {
      violations.push(
        `${o.where} : \`${o.raw}\` — ${o.image} doit venir de ${REGISTRY}, ` +
          `pas de ${o.registry || "Docker Hub (registre implicite)"}. ` +
          "Docker Hub ne sert plus minio/* anonymement (#157).",
      );
    }
    if (o.tag === undefined || !PINNED_TAG.test(o.tag)) {
      violations.push(
        `${o.where} : \`${o.raw}\` — tag non épinglé ` +
          `(${o.tag === undefined ? "aucun tag" : `« ${o.tag} »`}). ` +
          "Attendu : RELEASE.AAAA-MM-JJTHH-MM-SSZ ; un tag flottant laisse " +
          "l'image bouger sous la CI sans rien signaler.",
      );
    }
  }

  // Divergence : par image, sur les seules références dont le tag est épinglé
  // (une référence `latest` a déjà sa propre violation, ne pas la compter
  // deux fois). minio/minio et minio/mc sont comparés chacun de leur côté.
  for (const image of MINIO_IMAGES) {
    const byPin = new Map<string, string[]>();
    for (const o of all) {
      if (o.image !== image || o.tag === undefined || !PINNED_TAG.test(o.tag)) {
        continue;
      }
      const pin = o.tag + o.digest;
      byPin.set(pin, [...(byPin.get(pin) ?? []), o.where]);
    }
    if (byPin.size > 1) {
      const detail = [...byPin]
        .map(([pin, wheres]) => `  ${pin} : ${wheres.join(", ")}`)
        .join("\n");
      violations.push(
        `${image} : tags divergents — une même image doit porter le même ` +
          `tag partout (ci.yml et docker-compose.yml bougent ensemble).\n${detail}`,
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
// `RELEASE.*` MinIO, avec ou sans digest.
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

// `image: <référence>`, guillemets simples ou doubles tolérés.
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
    if (line.trimStart().startsWith("#")) return;
    const m = line.match(IMAGE_LINE);
    if (!m) return;
    seen += 1;
    const raw = m[1];
    const ref = raw.split("@")[0];
    const lastSlash = ref.lastIndexOf("/");
    const colon = ref.indexOf(":", lastSlash + 1);
    const tag = colon === -1 ? undefined : ref.slice(colon + 1);
    const ok =
      tag !== undefined &&
      (FULL_VERSION.test(tag) ||
        PINNED_TAG.test(tag) ||
        MAJOR_SERIES_ALLOWED.includes(ref));
    if (!ok) {
      violations.push(
        `${file.path}:${index + 1} : \`${raw}\` — tag non épinglé ` +
          `(${tag === undefined ? "aucun tag" : `« ${tag} »`}). Attendu : une ` +
          "version complète (1.2.3) ou, par exception, " +
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
