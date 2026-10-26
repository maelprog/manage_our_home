// Image Playwright du job `e2e` alignée sur `@playwright/test` (#610).
//
// Jusqu'à #610, le job `e2e` installait Chromium par
// `npx playwright install --with-deps chromium` : `--with-deps` passe par apt
// et le miroir Ubuntu du runner, dont le débit a fait durer l'étape de 18 s à
// plus de 10 min selon les runs. La suite tourne désormais dans
// `mcr.microsoft.com/playwright:v<version>-<distribution>`, qui embarque Node,
// les navigateurs et leurs dépendances système. Mais ces navigateurs sont ceux
// d'UNE version de Playwright : avec un `@playwright/test` (ou un
// `playwright-core`, qui choisit le build du navigateur) d'une autre version,
// la suite cherche un exécutable que l'image n'a pas.
//
// Aucun robot ne monte l'image et le paquet ensemble (`renovate.json` ne lit
// ni npm ni les `docker run`). Cette porte, lancée par `npm run test:scripts`
// dans le job `e2e`, rend donc rouge toute PR qui n'en bouge qu'un.
//
// Ce qu'elle refuse :
//   - une référence `mcr.microsoft.com/playwright` dont le tag n'est pas
//     `v<x.y.z>-<distribution>`, ou dont la version diffère de celle de
//     `@playwright/test`, `playwright` ou `playwright-core` dans le lockfile ;
//   - une référence sans digest, ou au digest mal formé (comme les autres
//     images de `ci.yml`) ;
//   - deux références de `ci.yml` sur des pins différents ;
//   - un `ci.yml` où l'image n'est plus trouvée, et un lockfile illisible ou
//     sans `@playwright/test` : une porte sans rien à comparer serait verte
//     sur zéro vérification.
//
// Limites : lecture textuelle de `ci.yml`, lignes de commentaire (premier
// caractère non blanc `#`) ignorées ; une image passée par une variable n'est
// pas vue, et le plancher « image absente » ne l'attrape que si c'était la
// seule occurrence. `mcr.microsoft.com/playwright/mcp` est une autre image.

export type SourceFile = { path: string; text: string };

export const CI_PATH = ".github/workflows/ci.yml";
export const LOCK_PATH = "e2e/package-lock.json";

// Les paquets du lockfile dont la version doit être celle de l'image.
const PACKAGES = ["@playwright/test", "playwright", "playwright-core"] as const;

// `mcr.microsoft.com/playwright` suivi d'un tag, d'un digest, ou de rien ; pas
// `mcr.microsoft.com/playwright/mcp`.
const IMAGE = /mcr\.microsoft\.com\/playwright(?![\w/.-])(?::([^\s@"'`]+))?(@[^\s"'`]*)?/g;
const TAG = /^v(\d+\.\d+\.\d+)-[a-z]+$/;
const DIGEST = /^@sha256:[0-9a-f]{64}$/;

type Reference = { where: string; raw: string; tag: string; digest: string };

function references(ci: SourceFile): Reference[] {
  const refs: Reference[] = [];
  ci.text.split("\n").forEach((line, index) => {
    if (line.trimStart().startsWith("#")) return;
    for (const m of line.matchAll(IMAGE)) {
      refs.push({
        where: `${ci.path}:${index + 1}`,
        raw: m[0],
        tag: m[1] ?? "",
        digest: m[2] ?? "",
      });
    }
  });
  return refs;
}

function lockVersions(lock: SourceFile): Map<string, string> | string {
  let parsed: unknown;
  try {
    parsed = JSON.parse(lock.text);
  } catch (e) {
    return `${lock.path} : package-lock illisible (${(e as Error).message}).`;
  }
  const packages = (parsed as { packages?: Record<string, { version?: unknown }> })
    ?.packages;
  const versions = new Map<string, string>();
  for (const name of PACKAGES) {
    const version = packages?.[`node_modules/${name}`]?.version;
    if (typeof version === "string") versions.set(name, version);
  }
  if (!versions.has("@playwright/test")) {
    return (
      `${lock.path} : aucune version de @playwright/test dans le lockfile ; ` +
      "rien à comparer à l'image."
    );
  }
  return versions;
}

/**
 * Rend la liste des violations (vide si la porte est tenue) entre les
 * références à l'image Playwright de `ci.yml` et le lockfile de `e2e/`.
 */
export function playwrightPinViolations(ci: SourceFile, lock: SourceFile): string[] {
  const versions = lockVersions(lock);
  if (typeof versions === "string") return [versions];

  const refs = references(ci);
  if (refs.length === 0) {
    return [
      `${ci.path} : aucune référence à mcr.microsoft.com/playwright. La porte ` +
        "ne peut rien prouver sur une image qu'elle ne voit pas.",
    ];
  }

  const violations: string[] = [];
  const pins = new Map<string, string[]>();
  for (const { where, raw, tag, digest } of refs) {
    const version = TAG.exec(tag)?.[1];
    if (version === undefined) {
      violations.push(
        `${where} : \`${raw}\` — tag « ${tag || "aucun tag"} ». Attendu : ` +
          "v<x.y.z>-<distribution> (v1.61.1-noble), la version de Playwright " +
          "dont l'image porte les navigateurs.",
      );
      continue;
    }
    if (!DIGEST.test(digest)) {
      violations.push(
        `${where} : \`${raw}\` — ` +
          (digest === "" ? "pin par digest manquant" : `digest mal formé « ${digest} »`) +
          ". Attendu : @sha256: suivi de 64 caractères hexadécimaux, comme " +
          "les autres images de ci.yml ; un tag peut être republié.",
      );
      continue;
    }
    for (const [name, locked] of versions) {
      if (locked !== version) {
        violations.push(
          `${where} : \`${tag}\` porte les navigateurs de Playwright ${version}, ` +
            `mais ${lock.path} fige ${name} en ${locked}. Monter l'image et le ` +
            "paquet ensemble.",
        );
      }
    }
    const pin = `${tag}${digest}`;
    pins.set(pin, [...(pins.get(pin) ?? []), where]);
  }

  if (pins.size > 1) {
    const detail = [...pins]
      .map(([pin, wheres]) => `  ${pin} : ${wheres.join(", ")}`)
      .join("\n");
    violations.push(
      `mcr.microsoft.com/playwright : pins divergents — une même image doit ` +
        `porter le même tag et le même digest partout.\n${detail}`,
    );
  }
  return violations;
}
