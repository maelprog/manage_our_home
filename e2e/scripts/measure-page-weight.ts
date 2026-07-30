#!/usr/bin/env node
//! Pèse les réponses HTML des routes principales, authentifié, et refuse de
//! rendre un chiffre dont il ne peut pas garantir la validité.
//!
//! ## Ce que ce script existe pour ne plus jamais laisser passer
//!
//! Deux agents ont mesuré ces pages à la main. Le premier a pesé 7 routes sur
//! 8 — sa liste avait `/account` (un lien d'en-tête) à la place de
//! `/messagerie` (une entrée de la nav) — et **toutes ses mesures ont été
//! prises sur des pages vides** : son script écrivait un fichier de 0 octet
//! pour la route au chemin erroné sans que rien ne le signale. Le second a
//! reproduit ces chiffres « à 6 octets près », sa stack étant vide aussi.
//! Remesurée sur des données ordinaires, une route sortait à 15 615 octets
//! compressés contre les 8 354 annoncés comme pire cas.
//!
//! D'où quatre garde-fous, dans cet ordre :
//!
//! 1. **La liste des routes est confrontée à la nav réelle** d'apps/web
//!    (`parseNavRoutes` lit `apps/web/src/app.rs`). Toute divergence arrête le
//!    script.
//! 2. **Aucune réponse douteuse n'est comptée** : code ≠ 200, corps vide,
//!    corps absurdement petit, ou page de connexion servie à la place de la
//!    page demandée → échec.
//! 3. **La stack doit être peuplée**, et la page doit *rendre* ce que la base
//!    contient. Un plancher d'octets ne suffit pas : une collection vide
//!    laisse la page à sa taille normale (grille dessinée, feuille inlinée).
//!    On compare donc, par route, les lignes rendues aux lignes stockées.
//! 4. **Le budget décide du code de sortie.**
//!
//! ## Ce qu'il mesure
//!
//! Brut, gzip et zstd, pour la réponse entière **et pour le document seul**
//! (réponse moins la feuille de style inlinée). C'est ce dernier chiffre qui
//! manquait : la feuille pèse le même poids sur les 8 routes et masque
//! complètement ce que chaque page coûte réellement.
//!
//! Les tailles compressées sont calculées localement (`node:zlib`), pas lues
//! sur le réseau : c'est reproductible, et ça marche que le serveur en face
//! compresse ou non. La négociation réelle est vérifiée séparément et
//! rapportée — un serveur sans `encode` produit un avertissement, pas un
//! plantage.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync, zstdCompressSync } from "node:zlib";

import { HttpSession, waitForService } from "./lib/http.ts";
import {
  budgetViolations,
  collectionFailures,
  countRendered,
  diffNavRoutes,
  emptyStackFailures,
  parseNavRoutes,
  splitInlineStylesheet,
  type CollectionCheck,
} from "./lib/measure-core.ts";

// --- configuration ---------------------------------------------------------

const args = new Set(process.argv.slice(2));
const asJson = args.has("--json");

function flagValue(name: string): string | undefined {
  const hit = process.argv.slice(2).find((a) => a.startsWith(`${name}=`));
  return hit?.slice(name.length + 1);
}

/**
 * On mesure **à travers Caddy** par défaut : c'est le chemin déployé, et c'est
 * lui qui compresse (`infra/Caddyfile` → `encode zstd gzip`). Le repli sur
 * `web:3000` en direct reste possible en pointant WEB_BASE_URL dessus ; il
 * faut alors donner API_BASE_URL séparément, apps/web ne servant pas `/api`.
 */
const BASE_URL = (flagValue("--base-url") ?? process.env.WEB_BASE_URL ?? "http://localhost").replace(
  /\/$/,
  "",
);
const API_BASE_URL = (
  flagValue("--api-base-url") ??
  process.env.API_BASE_URL ??
  `${BASE_URL}/api`
).replace(/\/$/, "");

const EMAIL = process.env.SEED_EMAIL ?? "camille.perf@example.test";
const PASSWORD = process.env.SEED_PASSWORD ?? "mesure du poids des pages";

/** Budget par défaut : la réponse entière, compressée, en octets.
 *
 *  14 336 o = 14 KiB, le budget de réponse que `apps/web/src/app.rs` retient
 *  déjà pour la feuille de style (issue #83) : l'ordre de grandeur de la
 *  première fenêtre de congestion TCP. C'est un paramètre, pas un dogme —
 *  `--budget=<octets>`. */
const DEFAULT_BUDGET = 14_336;
const BUDGET = Number(flagValue("--budget") ?? process.env.MEASURE_BUDGET ?? DEFAULT_BUDGET);

/** En deçà, une réponse ne peut pas être une page de cette application. */
const ABSURDLY_SMALL = 2_048;

// --- la liste des routes, et sa confrontation à la nav ----------------------

/**
 * Les routes mesurées. Déclarées à la main parce qu'elles portent des
 * métadonnées que la nav ne peut pas transporter (quelle collection la page
 * rend, à quoi ressemble une ligne rendue), **et confrontées à la nav réelle**
 * juste après : c'est la première des deux erreurs que cette PR corrige.
 *
 * `items` décrit comment reconnaître une ligne de collection dans le markup :
 * un lien vers l'élément, dont l'UUID distingue une vraie ligne des liens
 * statiques (`/stocks/new`, `/agenda/imports`).
 */
const UUID = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";

type RouteSpec = {
  path: string;
  /** Plancher d'octets du *document* (feuille exclue). Secondaire. */
  floor: number;
  items?: {
    label: string;
    /** Motif d'une ligne rendue. */
    pattern: RegExp;
    /** Endpoint API donnant ce que la base contient, et clé de la liste. */
    count: (gid: string) => { path: string; key: string };
  };
};

const ROUTES: RouteSpec[] = [
  // L'accueil ne rend aucune collection : rien à confronter, juste un plancher.
  { path: "/", floor: 400 },
  {
    path: "/agenda",
    floor: 1_500,
    items: {
      label: "événements",
      pattern: new RegExp(`href="/agenda/${UUID}\\?occ=`),
      count: (gid) => ({ path: `/groups/${gid}/events${eventWindow()}`, key: "occurrences" }),
    },
  },
  {
    path: "/stocks",
    floor: 1_500,
    items: {
      label: "articles de stock",
      pattern: new RegExp(`href="/stocks/${UUID}"`),
      count: (gid) => ({ path: `/groups/${gid}/stock-items`, key: "items" }),
    },
  },
  {
    path: "/recipes",
    floor: 1_200,
    items: {
      label: "recettes",
      pattern: new RegExp(`href="/recipes/${UUID}"`),
      count: (gid) => ({ path: `/groups/${gid}/recipes`, key: "recipes" }),
    },
  },
  {
    path: "/grocery-list",
    floor: 1_500,
    items: {
      label: "articles de courses",
      pattern: new RegExp(`href="/grocery-list/${UUID}"`),
      count: (gid) => ({ path: `/groups/${gid}/grocery-items`, key: "items" }),
    },
  },
  {
    path: "/budget",
    floor: 1_400,
    items: {
      label: "dépenses",
      pattern: new RegExp(`href="/budget/${UUID}"`),
      count: (gid) => ({ path: `/groups/${gid}/budget-entries`, key: "entries" }),
    },
  },
  {
    path: "/messagerie",
    floor: 3_000,
    items: {
      label: "messages",
      pattern: new RegExp(`action="/messagerie/${UUID}/delete"`),
      count: (gid) => ({ path: `/groups/${gid}/messages?limit=100`, key: "messages" }),
    },
  },
  {
    path: "/groups",
    floor: 500,
    items: {
      label: "familles",
      pattern: new RegExp(`href="/groups/${UUID}/`),
      count: () => ({ path: "/groups", key: "" }),
    },
  },
];

/**
 * Les entrées de nav conditionnelles, et pourquoi elles ne sont pas mesurées.
 * Un nouveau slot dans la nav qui n'apparaîtrait pas ici fait échouer le
 * script : impossible d'ajouter une entrée sans décider si on la pèse.
 */
const CONDITIONAL_NAV_SLOTS: Record<string, string> = {
  // `/admin/users` répond 404 à quiconque n'est pas LE superadmin technique
  // (apps/web/src/routes/admin/). Le peser demanderait de promouvoir le compte
  // de mesure, ce qui changerait toutes les autres pages (la nav gagne un lien).
  admin_link: "/admin/users",
};

function eventWindow(): string {
  const now = new Date();
  const from = new Date(now);
  from.setUTCFullYear(from.getUTCFullYear() - 1);
  const to = new Date(now);
  to.setUTCFullYear(to.getUTCFullYear() + 1);
  return `?from=${encodeURIComponent(from.toISOString())}&to=${encodeURIComponent(to.toISOString())}`;
}

const HERE = dirname(fileURLToPath(import.meta.url));
const APP_RS = join(HERE, "..", "..", "apps", "web", "src", "app.rs");

function assertRouteListMatchesNav(): void {
  let source: string;
  try {
    source = readFileSync(APP_RS, "utf8");
  } catch (cause) {
    throw new Error(
      `impossible de lire ${APP_RS} pour confronter la liste des routes à la nav réelle ` +
        `(${String(cause)}). Ce contrôle n'est pas optionnel : sans lui, la liste dérive.`,
    );
  }

  const parsed = parseNavRoutes(source);
  const diff = diffNavRoutes(
    ROUTES.map((r) => r.path),
    parsed,
    CONDITIONAL_NAV_SLOTS,
  );

  const problems: string[] = [];
  if (diff.missing.length > 0) {
    problems.push(
      `routes présentes dans la nav mais absentes de la liste mesurée : ${diff.missing.join(", ")}`,
    );
  }
  if (diff.unexpected.length > 0) {
    problems.push(
      `routes mesurées qui ne sont pas dans la nav : ${diff.unexpected.join(", ")}`,
    );
  }
  if (diff.unknownPlaceholders.length > 0) {
    problems.push(
      `entrées de nav conditionnelles inconnues : ${diff.unknownPlaceholders.join(", ")} ` +
        "(ajoute-les à CONDITIONAL_NAV_SLOTS en disant si elles sont mesurées)",
    );
  }
  if (problems.length > 0) {
    throw new Error(
      `La liste des routes mesurées a divergé de la nav d'apps/web (${APP_RS}) :\n` +
        problems.map((p) => `  - ${p}`).join("\n") +
        "\n\nC'est exactement l'erreur qui a fait mesurer 7 routes sur 8 la première fois.",
    );
  }
}

// --- mesure ----------------------------------------------------------------

type Row = {
  route: string;
  raw: number;
  gzip: number;
  zstd: number;
  documentRaw: number;
  documentGzip: number;
  documentZstd: number;
  stylesheetRaw: number;
  rendered: number | null;
  stored: number | null;
};

function weigh(route: string, html: string): Omit<Row, "rendered" | "stored"> {
  const split = splitInlineStylesheet(html);
  if (!split.hasInlineStylesheet) {
    throw new Error(
      `${route} : aucune feuille de style inlinée trouvée dans la réponse. ` +
        "apps/web en inline une sur chaque page (app::shell) — soit la réponse n'est pas " +
        "une page de l'application, soit la stratégie d'inlining a changé.",
    );
  }
  // Le document = la réponse moins le texte de la feuille. Les balises
  // <style></style> restent du côté document : elles appartiennent au shell.
  const withoutSheet = html.replace(/(<style[^>]*>)[\s\S]*?(<\/style>)/g, "$1$2");
  const full = Buffer.from(html, "utf8");
  const doc = Buffer.from(withoutSheet, "utf8");
  return {
    route,
    raw: split.totalBytes,
    gzip: gzipSync(full).length,
    zstd: zstdCompressSync(full).length,
    documentRaw: doc.length,
    documentGzip: gzipSync(doc).length,
    documentZstd: zstdCompressSync(doc).length,
    stylesheetRaw: split.styleBytes,
  };
}

async function login(): Promise<HttpSession> {
  const session = new HttpSession(BASE_URL);
  const form = new URLSearchParams({ email: EMAIL, password: PASSWORD }).toString();
  const res = await fetch(`${BASE_URL}/login`, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: form,
    redirect: "manual",
  });
  const body = await res.text();
  if (res.status !== 303 && res.status !== 302) {
    throw new Error(
      `Connexion refusée pour ${EMAIL} : POST ${BASE_URL}/login → HTTP ${res.status}.\n` +
        "La stack n'a probablement pas été peuplée : lance d'abord `npm run seed`.\n" +
        `${body.slice(0, 400)}`,
    );
  }
  for (const raw of res.headers.getSetCookie()) {
    const [pair] = raw.split(";");
    const eq = pair.indexOf("=");
    if (eq > 0) session.setCookie(pair.slice(0, eq).trim(), pair.slice(eq + 1).trim());
  }
  if (!session.cookieHeader()) {
    throw new Error(`POST ${BASE_URL}/login a répondu ${res.status} sans poser de cookie de session`);
  }
  return session;
}

async function fetchPage(session: HttpSession, route: string): Promise<string> {
  // `identity` : on veut les octets bruts pour compresser nous-mêmes, de façon
  // reproductible et indépendante de la configuration du serveur en face.
  const { status, text } = await session.raw("GET", route, {
    expect: [200],
    headers: { "Accept-Encoding": "identity" },
  });
  if (text.length === 0) {
    throw new Error(`${route} : réponse ${status} avec un corps VIDE — mesure refusée.`);
  }
  if (Buffer.byteLength(text) < ABSURDLY_SMALL) {
    throw new Error(
      `${route} : ${Buffer.byteLength(text)} octets, sous le seuil d'absurdité (${ABSURDLY_SMALL}). ` +
        "Ce n'est pas une page de cette application.",
    );
  }
  // Une session expirée ou absente ne donne pas une erreur : apps/web renvoie
  // 200 avec l'écran de connexion. Sans ce contrôle on pèserait /login huit fois.
  if (!text.includes('<nav class="actions">')) {
    throw new Error(
      `${route} : la réponse ne contient pas la nav authentifiée — c'est probablement ` +
        "l'écran de connexion servi en 200. Mesure refusée.",
    );
  }
  return text;
}

/** Ce que la base contient, lu par l'API avec la même session. */
async function storedCount(
  api: HttpSession,
  spec: NonNullable<RouteSpec["items"]>,
  gid: string,
): Promise<number> {
  const { path, key } = spec.count(gid);
  const payload = await api.json<Record<string, unknown[]> | unknown[]>("GET", path);
  if (Array.isArray(payload)) return payload.length;
  const list = payload[key];
  if (!Array.isArray(list)) {
    throw new Error(`${path} : pas de liste « ${key} » dans la réponse de l'API`);
  }
  // Les occurrences d'un événement récurrent partagent son id.
  if (key === "occurrences") {
    return new Set((list as { id: string }[]).map((o) => o.id)).size;
  }
  return list.length;
}

/** Ce que le serveur négocie vraiment — informatif, jamais bloquant. */
async function negotiatedEncoding(session: HttpSession): Promise<string | null> {
  const res = await fetch(`${BASE_URL}/`, {
    headers: { "Accept-Encoding": "zstd, gzip", Cookie: session.cookieHeader() ?? "" },
    redirect: "manual",
  });
  await res.arrayBuffer();
  return res.headers.get("content-encoding");
}

// --- rendu -----------------------------------------------------------------

function table(rows: Row[]): string {
  const head = [
    "route",
    "brut",
    "gzip",
    "zstd",
    "doc brut",
    "doc gzip",
    "doc zstd",
    "lignes",
  ];
  const body = rows.map((r) => [
    r.route,
    String(r.raw),
    String(r.gzip),
    String(r.zstd),
    String(r.documentRaw),
    String(r.documentGzip),
    String(r.documentZstd),
    r.rendered === null ? "—" : `${r.rendered}/${r.stored}`,
  ]);
  const widths = head.map((h, i) => Math.max(h.length, ...body.map((b) => b[i].length)));
  const line = (cells: string[]) =>
    "  " + cells.map((c, i) => (i === 0 ? c.padEnd(widths[i]) : c.padStart(widths[i]))).join("  ");
  return [line(head), line(widths.map((w) => "-".repeat(w))), ...body.map(line)].join("\n");
}

// --- programme -------------------------------------------------------------

async function main(): Promise<void> {
  assertRouteListMatchesNav();

  await waitForService(BASE_URL, "/login");
  const session = await login();
  const api = new HttpSession(API_BASE_URL);
  for (const pair of (session.cookieHeader() ?? "").split("; ")) {
    const eq = pair.indexOf("=");
    if (eq > 0) api.setCookie(pair.slice(0, eq), pair.slice(eq + 1));
  }

  const groups = await api.json<{ group_id: string; name: string }[]>("GET", "/groups");
  if (groups.length === 0) {
    throw new Error(
      `${EMAIL} n'appartient à aucune famille : la stack n'a pas été peuplée. ` +
        "Lance d'abord le script de seeding : `npm run seed`.",
    );
  }
  const gid = groups[0].group_id;

  const rows: Row[] = [];
  const checks: CollectionCheck[] = [];
  for (const spec of ROUTES) {
    const html = await fetchPage(session, spec.path);
    const weighed = weigh(spec.path, html);
    let rendered: number | null = null;
    let stored: number | null = null;
    if (spec.items) {
      rendered = countRendered(html, spec.items.pattern);
      stored = await storedCount(api, spec.items, gid);
      checks.push({
        route: spec.path,
        collection: spec.items.label,
        stored,
        rendered,
      });
    }
    rows.push({ ...weighed, rendered, stored });
  }

  // --- garde-fous, avant tout affichage de chiffres ------------------------

  const notRendered = collectionFailures(checks);
  if (notRendered.length > 0) {
    const unseeded = notRendered.filter((f) => f.kind === "unseeded");
    const invisible = notRendered.filter((f) => f.kind === "not-rendered");
    const parts: string[] = [];
    if (unseeded.length > 0) {
      parts.push(
        "La stack n'est pas peuplée — la base ne contient aucune ligne pour :\n" +
          unseeded.map((f) => `  - ${f.route} (${f.collection})`).join("\n") +
          "\n\n  Lance d'abord le script de seeding : `npm run seed`.",
      );
    }
    if (invisible.length > 0) {
      parts.push(
        "Des routes ne rendent AUCUNE ligne alors que la base en contient :\n" +
          invisible
            .map((f) => `  - ${f.route} : ${f.stored} ${f.collection} en base, 0 rendus`)
            .join("\n") +
          "\n\n  Les données sont hors de la fenêtre affichée par la page — /agenda rend la\n" +
          "  grille du MOIS COURANT (apps/web/src/routes/agenda/calendar.rs, repli sur\n" +
          "  today_paris). Un jeu semé sur un autre mois donne une page de taille normale\n" +
          "  mais vide, soit ~33 % de moins mesurés sans que rien n'ait l'air anormal.\n" +
          "  Relance le seeding sans SEED_REFERENCE_DATE : `npm run seed`.",
      );
    }
    throw new Error(parts.join("\n\n"));
  }

  const thin = emptyStackFailures(rows, Object.fromEntries(ROUTES.map((r) => [r.path, r.floor])));
  if (thin.length > 0) {
    throw new Error(
      "Des documents sont sous leur plancher de taille :\n" +
        thin
          .map((f) => `  - ${f.route} : ${f.documentRaw} octets, plancher ${f.floor}`)
          .join("\n") +
        "\n\n  Stack insuffisamment peuplée : lance d'abord `npm run seed`.",
    );
  }

  // --- sortie ---------------------------------------------------------------

  const encoding = await negotiatedEncoding(session);
  const violations = budgetViolations(rows, BUDGET);

  if (asJson) {
    process.stdout.write(
      `${JSON.stringify(
        {
          baseUrl: BASE_URL,
          measuredAt: new Date().toISOString(),
          budgetBytes: BUDGET,
          negotiatedContentEncoding: encoding,
          dataset: { group: groups[0].name, account: EMAIL, collections: checks },
          routes: rows,
          violations,
        },
        null,
        2,
      )}\n`,
    );
  } else {
    process.stdout.write(`Poids des réponses HTML — ${BASE_URL}\n`);
    process.stdout.write(`  jeu de données : « ${groups[0].name} » via ${EMAIL}\n`);
    process.stdout.write(
      `  volumes en base : ${checks.map((c) => `${c.stored} ${c.collection}`).join(", ")}\n`,
    );
    process.stdout.write(`  budget : ${BUDGET} octets (réponse entière, gzip)\n`);
    if (encoding) {
      process.stdout.write(`  compression négociée par le serveur : ${encoding}\n`);
    } else {
      process.stdout.write(
        "  AVERTISSEMENT : le serveur ne négocie aucune compression (pas de Content-Encoding).\n" +
          "    Les colonnes gzip/zstd restent valides — elles sont calculées ici — mais ce\n" +
          "    n'est pas ce qui part sur le réseau. Mesure-tu à travers Caddy (`encode zstd\n" +
          "    gzip` dans infra/Caddyfile) ou en direct sur web:3000 ?\n",
      );
    }
    process.stdout.write("\n");
    process.stdout.write(`${table(rows)}\n\n`);
    process.stdout.write(
      `  « lignes » = lignes rendues par la page / lignes en base. Le document\n` +
        `  est la réponse moins la feuille de style inlinée (~${rows[0].stylesheetRaw} octets,\n` +
        `  identique sur toutes les routes) : c'est la part propre à la page.\n\n`,
    );
  }

  if (violations.length > 0) {
    process.stderr.write(
      `BUDGET DÉPASSÉ (${BUDGET} octets gzip) :\n` +
        violations.map((v) => `  - ${v.route} : ${v.gzip} octets (+${v.over})\n`).join("") +
        "\n",
    );
    process.exit(1);
  }
}

main().catch((error: unknown) => {
  process.stderr.write(
    `\nMESURE REFUSÉE\n\n${error instanceof Error ? error.message : String(error)}\n\n`,
  );
  process.exit(1);
});
