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
//! (Tous les chiffres de ce paragraphe viennent de cette campagne manuelle
//! antérieure — issue #83 — sur un jeu de données différent de celui que
//! `npm run seed` produit. Ils situent l'incident ; ils ne décrivent pas ce
//! que ce script mesurera chez toi, et ne doivent pas servir d'attendus.)
//!
//! D'où quatre garde-fous, dans cet ordre :
//!
//! 1. **La liste des routes est confrontée à la nav réelle** d'apps/web
//!    (`parseNavRoutes` lit `apps/web/src/app.rs`). Toute divergence arrête le
//!    script.
//! 2. **Aucune réponse douteuse n'est comptée** : code ≠ 200, corps vide,
//!    corps absurdement petit, ou page de connexion servie à la place de la
//!    page demandée → échec.
//! 3. **La fenêtre affichée doit contenir le volume attendu, et la page doit
//!    le rendre en entier.** Un plancher d'octets ne suffit pas : une
//!    collection vide laisse la page à sa taille normale (grille dessinée,
//!    feuille inlinée). Une comparaison rendu/stocké seule ne suffit pas non
//!    plus — comptée dans la fenêtre affichée, elle est cohérente avec
//!    elle-même quand la fenêtre est à moitié vide (18 rendus sur 18 stockés,
//!    /agenda 36 % plus léger). Il faut donc les deux : un volume attendu par
//!    collection, ET la complétude du rendu.
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
  expectedRendered,
  optionalPositiveInt,
  parseNavRoutes,
  requirePositiveInt,
  splitInlineStylesheet,
  type CollectionCheck,
} from "./lib/measure-core.ts";
import { monthGridWindow } from "./lib/seed-core.ts";

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

/** Résolu dans `main`, pas au chargement du module, pour que l'entrée invalide
 *  ressorte dans le bloc « MESURE REFUSÉE » et non en trace de pile brute. */
function resolveBudget(): number {
  const raw = flagValue("--budget") ?? process.env.MEASURE_BUDGET;
  return raw === undefined ? DEFAULT_BUDGET : requirePositiveInt("--budget", raw);
}

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

/**
 * Ce que la fenêtre affichée doit contenir pour qu'une mesure soit
 * représentative : les cibles de `scripts/seed-perf-data.ts`. Surchargeables
 * si tu mesures un jeu volontairement plus petit — mais alors tes octets ne
 * sont plus comparables à ceux de quelqu'un d'autre, ce que le tableau
 * rappelle en imprimant les volumes.
 */
type MinStoredKey =
  | "events"
  | "stockItems"
  | "groceryItems"
  | "budgetEntries"
  | "recipes"
  | "messages"
  | "groups";

/**
 * Résolu dans `main`, et **validé** : chacune de ces six variables pilote le
 * contrôle `understocked`, et une valeur invalide ne doit pas l'éteindre.
 *
 * Elle le pouvait : ces entrées passaient par un `Number()` nu, donc
 * `SEED_EVENTS=oops` donnait NaN, `=0` et `=` donnaient 0, et `stored <
 * minStored` devenait faux dans les trois cas. Le garde-fou disparaissait
 * sans un mot et la mesure retombait sur la tautologie « 18 rendus sur 18
 * stockés » — /agenda à −36 % avec un tableau d'apparence normale. C'est le
 * mode de panne pour lequel `requirePositiveInt` avait été écrit, recâblé
 * dans le contrôle censé le remplacer.
 *
 * Verrouillé ici plutôt qu'au seed à dessein : un désaccord de cibles doit
 * faire échouer bruyamment côté mesure (`stored < minStored` → exit 1), pas
 * passer inaperçu.
 */
function resolveMinStored(): Record<MinStoredKey, number> {
  return {
    events: optionalPositiveInt("SEED_EVENTS", process.env.SEED_EVENTS, 40),
    stockItems: optionalPositiveInt("SEED_STOCK_ITEMS", process.env.SEED_STOCK_ITEMS, 40),
    groceryItems: optionalPositiveInt("SEED_GROCERY_ITEMS", process.env.SEED_GROCERY_ITEMS, 30),
    budgetEntries: optionalPositiveInt("SEED_BUDGET_ENTRIES", process.env.SEED_BUDGET_ENTRIES, 30),
    recipes: optionalPositiveInt("SEED_RECIPES", process.env.SEED_RECIPES, 25),
    messages: optionalPositiveInt("SEED_MESSAGES", process.env.SEED_MESSAGES, 50),
    groups: 1,
  };
}

type RouteSpec = {
  path: string;
  /**
   * Plancher d'octets du *document* (feuille exclue).
   *
   * Franchement : depuis que la complétude du rendu est vérifiée (voir
   * `items` et `collectionFailures`), ce plancher n'attrape plus rien qu'elle
   * n'attrape déjà, et il est calibré très bas — il ne se déclenche que sur un
   * shell totalement vide, sur une route qui n'a pas de collection à
   * confronter. Il reste pour `/`, qui n'en a pas ; ailleurs c'est une
   * ceinture par-dessus des bretelles, pas une protection.
   */
  floor: number;
  items?: {
    label: string;
    /** Motif d'une ligne rendue. */
    pattern: RegExp;
    /**
     * Endpoint API donnant ce que la base contient **dans la fenêtre que la
     * page affiche**, et clé de la liste dans la réponse.
     */
    count: (gid: string) => { path: string; key: string };
    /**
     * Taille de page, si la route pagine — la seule tolérance admise à un
     * rendu partiel, déclarée route par route et jamais globalement.
     */
    pageSize?: number;
    /**
     * Volume que la fenêtre affichée doit contenir pour que la mesure ait un
     * sens : les cibles de `npm run seed`. Sans ce nombre, une fenêtre à
     * moitié vide est cohérente avec elle-même (18 rendus sur 18 stockés) et
     * passe, alors que la page pèse un tiers de moins. Une clé, pas un
     * nombre : la valeur est résolue et validée dans `main`.
     */
    minStoredKey: MinStoredKey;
    /**
     * Endpoint facultatif comptant la même collection **hors** de la fenêtre
     * affichée, uniquement pour dire à l'utilisateur « il y en a, mais pas
     * ici » au lieu de « il n'y en a pas ».
     */
    countAnywhere?: (gid: string) => { path: string; key: string };
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
      minStoredKey: "events",
      pattern: new RegExp(`href="/agenda/${UUID}\\?occ=`),
      // La MÊME fenêtre que la page rend et que le seed remplit : les 42 jours
      // de `month_grid`. Compter plus large laisserait passer un rendu partiel
      // (semer en août, mesurer en juillet : 18 sur 40) ; compter plus étroit
      // ferait échouer une mesure valide.
      count: (gid) => ({ path: `/groups/${gid}/events${gridWindowQuery()}`, key: "occurrences" }),
      countAnywhere: (gid) => ({ path: `/groups/${gid}/events${wideWindowQuery()}`, key: "occurrences" }),
    },
  },
  {
    path: "/stocks",
    floor: 1_500,
    items: {
      label: "articles de stock",
      minStoredKey: "stockItems",
      pattern: new RegExp(`href="/stocks/${UUID}"`),
      count: (gid) => ({ path: `/groups/${gid}/stock-items`, key: "items" }),
    },
  },
  {
    path: "/recipes",
    floor: 1_200,
    items: {
      label: "recettes",
      minStoredKey: "recipes",
      pattern: new RegExp(`href="/recipes/${UUID}"`),
      count: (gid) => ({ path: `/groups/${gid}/recipes`, key: "recipes" }),
    },
  },
  {
    path: "/grocery-list",
    floor: 1_500,
    items: {
      label: "articles de courses",
      minStoredKey: "groceryItems",
      pattern: new RegExp(`href="/grocery-list/${UUID}"`),
      count: (gid) => ({ path: `/groups/${gid}/grocery-items`, key: "items" }),
    },
  },
  {
    path: "/budget",
    floor: 1_400,
    items: {
      label: "dépenses",
      minStoredKey: "budgetEntries",
      pattern: new RegExp(`href="/budget/${UUID}"`),
      count: (gid) => ({ path: `/groups/${gid}/budget-entries`, key: "entries" }),
    },
  },
  {
    path: "/messagerie",
    floor: 3_000,
    items: {
      label: "messages",
      minStoredKey: "messages",
      pattern: new RegExp(`action="/messagerie/${UUID}/delete"`),
      count: (gid) => ({ path: `/groups/${gid}/messages?limit=100`, key: "messages" }),
      // La seule route paginée : DEFAULT_PAGE_LIMIT côté API
      // (apps/shared/src/validation/messagerie.rs).
      pageSize: 50,
    },
  },
  {
    path: "/groups",
    floor: 500,
    items: {
      label: "familles",
      minStoredKey: "groups",
      pattern: new RegExp(`href="/groups/${UUID}/`),
      count: () => ({ path: "/groups", key: "" }),
    },
  },
];

/**
 * Les entrées de nav conditionnelles, et pourquoi elles ne sont pas mesurées.
 * Une nouvelle entrée conditionnelle qui n'apparaîtrait pas ici fait échouer
 * le script : impossible d'en ajouter une sans décider si on la pèse.
 *
 * `/admin/users` répond 404 à quiconque n'est pas LE superadmin technique
 * (apps/web/src/routes/admin/). Le peser demanderait de promouvoir le compte
 * de mesure, ce qui changerait toutes les autres pages (la nav gagne un lien).
 */
const CONDITIONAL_NAV_ROUTES: string[] = ["/admin/users"];

function rangeQuery(from: Date, to: Date): string {
  return `?from=${encodeURIComponent(from.toISOString())}&to=${encodeURIComponent(to.toISOString())}`;
}

/** Les 42 jours que `/agenda` rend aujourd'hui — la fenêtre de référence. */
function gridWindowQuery(): string {
  const win = monthGridWindow(new Date());
  return rangeQuery(win.from, win.to);
}

/** ±1 an : sert uniquement au diagnostic « il y en a, mais pas ici ». */
function wideWindowQuery(): string {
  const now = new Date();
  const from = new Date(now);
  from.setUTCFullYear(from.getUTCFullYear() - 1);
  const to = new Date(now);
  to.setUTCFullYear(to.getUTCFullYear() + 1);
  return rangeQuery(from, to);
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
    CONDITIONAL_NAV_ROUTES,
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
  if (diff.unknownConditional.length > 0) {
    problems.push(
      `entrées de nav conditionnelles inconnues : ${diff.unknownConditional.join(", ")} ` +
        "(ajoute-les à CONDITIONAL_NAV_ROUTES en disant si elles sont mesurées)",
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
  //
  // Le témoin est le formulaire de déconnexion et non la classe du `<nav>` :
  // il n'existe que pour une session ouverte, et c'est une *route*, pas un
  // crochet de style. La version précédente cherchait `<nav class="actions">`
  // et a cessé de reconnaître une page authentifiée dès que #70 a renommé
  // cette classe en `tabs` — un renommage de CSS ne doit pas pouvoir arrêter
  // le mesureur, ni (pire) le laisser peser huit fois l'écran de connexion.
  if (!text.includes('action="/logout"')) {
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
  where: { path: string; key: string },
): Promise<number> {
  const { path, key } = where;
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
  const BUDGET = resolveBudget();
  const mins = resolveMinStored();
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
  /** route → lignes existant hors de la fenêtre affichée (diagnostic). */
  const elsewhere = new Map<string, number>();
  for (const spec of ROUTES) {
    const html = await fetchPage(session, spec.path);
    const weighed = weigh(spec.path, html);
    let rendered: number | null = null;
    let stored: number | null = null;
    if (spec.items) {
      rendered = countRendered(html, spec.items.pattern);
      stored = await storedCount(api, spec.items.count(gid));
      const minStored = mins[spec.items.minStoredKey];
      if (stored < minStored && spec.items.countAnywhere) {
        elsewhere.set(spec.path, await storedCount(api, spec.items.countAnywhere(gid)));
      }
      checks.push({
        route: spec.path,
        collection: spec.items.label,
        stored,
        rendered,
        expected: expectedRendered(stored, spec.items.pageSize),
        minStored,
      });
    }
    rows.push({ ...weighed, rendered, stored });
  }

  // --- garde-fous, avant tout affichage de chiffres ------------------------

  const hollow = collectionFailures(checks);
  if (hollow.length > 0) {
    const unseeded = hollow.filter((f) => f.kind === "unseeded");
    const invisible = hollow.filter((f) => f.kind === "not-rendered");
    const partial = hollow.filter((f) => f.kind === "incomplete");
    const thinWindow = hollow.filter((f) => f.kind === "understocked");
    const parts: string[] = [];
    if (thinWindow.length > 0) {
      parts.push(
        "La fenêtre affichée contient MOINS que ce que le seed y pose :\n" +
          thinWindow
            .map((f) => {
              const away = elsewhere.get(f.route);
              const extra =
                away !== undefined && away > f.stored
                  ? ` — ${away} au total en base, donc ${away - f.stored} hors de la fenêtre`
                  : "";
              return (
                `  - ${f.route} : ${f.stored} ${f.collection} sur ${f.minStored} attendus${extra}`
              );
            })
            .join("\n") +
          "\n\n  Une fenêtre à moitié pleine se rend intégralement : les lignes rendues\n" +
          "  égalent les lignes stockées, tout paraît cohérent, et la page pèse\n" +
          "  pourtant un tiers de moins. Seul un volume attendu distingue ce cas.\n" +
          "  /agenda ne rend que les 42 jours de la grille du mois courant\n" +
          "  (month_grid : du lundi précédant le 1er au 42ᵉ jour), donc un jeu semé\n" +
          "  sur un autre mois n'y apparaît qu'en partie.\n" +
          "  Relance le seeding sans SEED_REFERENCE_DATE : `npm run seed`.",
      );
    }
    if (unseeded.length > 0) {
      parts.push(
        "La fenêtre affichée ne contient AUCUNE ligne pour :\n" +
          unseeded
            .map((f) => {
              const away = elsewhere.get(f.route);
              return away
                ? `  - ${f.route} (${f.collection}) : 0 dans la fenêtre affichée, ` +
                    `mais ${away} existent ailleurs en base`
                : `  - ${f.route} (${f.collection}) : rien en base`;
            })
            .join("\n") +
          "\n\n  Lance le script de seeding : `npm run seed`. Il compte dans cette même\n" +
          "  fenêtre, donc il recréera bien ce qui manque ici (et il ne supprime rien).",
      );
    }
    if (invisible.length > 0) {
      parts.push(
        "Des routes ne rendent AUCUNE ligne alors que la fenêtre en contient :\n" +
          invisible
            .map((f) => `  - ${f.route} : ${f.stored} ${f.collection} attendus, 0 rendus`)
            .join("\n"),
      );
    }
    if (partial.length > 0) {
      parts.push(
        "Des routes ne rendent qu'une PARTIE de ce que la fenêtre contient :\n" +
          partial
            .map(
              (f) =>
                `  - ${f.route} : ${f.rendered} ${f.collection} rendus sur ${f.expected} attendus` +
                ` (${Math.round((100 * (f.expected - f.rendered)) / f.expected)} % manquants)`,
            )
            .join("\n") +
          "\n\n  Une page à moitié remplie pèse un poids intermédiaire parfaitement\n" +
          "  plausible : c'est exactement la panne d'origine. /agenda rend les 42 jours\n" +
          "  de la grille du mois courant (month_grid : du lundi précédant le 1er au\n" +
          "  42ᵉ jour), donc un jeu semé sur un autre mois n'y apparaît qu'en partie.\n" +
          "  Relance le seeding sans SEED_REFERENCE_DATE : `npm run seed`.",
      );
    }
    throw new Error(parts.join("\n\n"));
  }

  // Plancher de taille : contrôle SECONDAIRE, et il faut être honnête sur ce
  // qu'il vaut. Depuis que la complétude du rendu est vérifiée juste au-dessus,
  // il n'attrape plus rien qu'elle n'attrape déjà — sauf sur `/`, qui n'a pas
  // de collection à confronter. Il ne se déclenche que sur un shell vide.
  const thin = emptyStackFailures(rows, Object.fromEntries(ROUTES.map((r) => [r.path, r.floor])));
  if (thin.length > 0) {
    throw new Error(
      "Des documents sont sous leur plancher de taille — la page est un shell vide :\n" +
        thin
          .map((f) => `  - ${f.route} : ${f.documentRaw} octets, plancher ${f.floor}`)
          .join("\n") +
        "\n\n  Lance d'abord `npm run seed`.",
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
