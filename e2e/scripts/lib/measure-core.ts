//! Pure logic behind `scripts/measure-page-weight.ts`.
//!
//! Everything here is deliberately I/O-free so it can be unit-tested
//! (`measure-core.test.ts`, run by `npm run test:scripts`). The three
//! functions that matter are the three guardrails: the nav/route-list
//! divergence check, the "response minus the inlined stylesheet" split, and
//! the floor that catches a stack nobody seeded.

/** What `parseNavRoutes` found inside apps/web's `<nav class="actions">`. */
export type NavParse = {
  /** The literal `href`s, in nav order. */
  hrefs: string[];
  /** `format!` slots interpolated into the nav, e.g. `admin_link`. */
  placeholders: string[];
};

const NAV_RE = /<nav class="actions">([\s\S]*?)<\/nav>/g;

/**
 * Extracts the app nav out of `apps/web/src/app.rs`'s `app_header`.
 *
 * The measured route list is declared by hand in the script (it needs per
 * route metadata the nav cannot carry), so it can drift from the real nav —
 * and it did: the first time these pages were weighed, the list carried
 * `/account` (a header link, not a nav entry) instead of `/messagerie`, and
 * one of the eight pages was simply never measured. Parsing the nav back out
 * of the source turns that drift into a failed run.
 */
export function parseNavRoutes(source: string): NavParse {
  const blocks = [...source.matchAll(NAV_RE)];
  if (blocks.length === 0) {
    throw new Error(
      'aucun `<nav class="actions">` trouvé dans apps/web/src/app.rs — ' +
        "le marquage de la nav a changé, mets à jour parseNavRoutes()",
    );
  }
  if (blocks.length > 1) {
    throw new Error(
      `${blocks.length} éléments \`<nav class="actions">\` trouvés dans la source fournie ; ` +
        "parseNavRoutes() en attend exactement un (celui de app_header)",
    );
  }
  const nav = blocks[0][1];
  return {
    hrefs: [...nav.matchAll(/href="([^"]*)"/g)].map((m) => m[1]),
    placeholders: [...nav.matchAll(/\{(\w+)\}/g)].map((m) => m[1]),
  };
}

export type NavDivergence = {
  /** In the nav, absent from the declared list. */
  missing: string[];
  /** Declared, absent from the nav. */
  unexpected: string[];
  /** Conditional nav slots the script knows nothing about. */
  unknownPlaceholders: string[];
};

/**
 * Compares the declared route list against the parsed nav. `knownSlots` maps
 * a `format!` placeholder to the route it can render (e.g. `admin_link` ->
 * `/admin/users`); those routes are *not* required to be measured — the admin
 * screens 404 for everyone but the single technical superadmin — but the slot
 * has to be acknowledged, so that a new conditional entry can't slip in
 * unmeasured and unnoticed.
 */
export function diffNavRoutes(
  declared: string[],
  parsed: NavParse,
  knownSlots: Record<string, string>,
): NavDivergence {
  const declaredSet = new Set(declared);
  const navSet = new Set(parsed.hrefs);
  return {
    missing: parsed.hrefs.filter((h) => !declaredSet.has(h)),
    unexpected: declared.filter((r) => !navSet.has(r)),
    unknownPlaceholders: parsed.placeholders.filter((p) => !(p in knownSlots)),
  };
}

export type StylesheetSplit = {
  totalBytes: number;
  styleBytes: number;
  /** `totalBytes - styleBytes`: what the route itself contributes. */
  documentBytes: number;
  hasInlineStylesheet: boolean;
};

/**
 * Splits a response into "the inlined stylesheet" and "everything else".
 *
 * apps/web inlines the whole of `style.css` into every response (see
 * `app::shell`), so the stylesheet dominates the byte count and hides what a
 * route actually costs. That missing number is what made the first budget
 * wrong. The `<style>`/`</style>` tags themselves stay on the document side —
 * they belong to the shell, not to the stylesheet.
 */
export function splitInlineStylesheet(html: string): StylesheetSplit {
  const matches = [...html.matchAll(/<style[^>]*>([\s\S]*?)<\/style>/g)];
  const styleBytes = matches.reduce((sum, m) => sum + Buffer.byteLength(m[1]), 0);
  const totalBytes = Buffer.byteLength(html);
  return {
    totalBytes,
    styleBytes,
    documentBytes: totalBytes - styleBytes,
    hasInlineStylesheet: matches.length > 0,
  };
}

/** The subset of a measurement row the pure checks need. */
export type Weighed = {
  route: string;
  /** Full response, gzip-compressed. */
  gzip: number;
  /** Response minus the inlined stylesheet, uncompressed. */
  documentRaw: number;
};

export type BudgetViolation = { route: string; gzip: number; over: number };

/** Routes whose gzipped response exceeds `budgetBytes`, worst first. */
export function budgetViolations(rows: Weighed[], budgetBytes: number): BudgetViolation[] {
  return rows
    .filter((r) => r.gzip > budgetBytes)
    .map((r) => ({ route: r.route, gzip: r.gzip, over: r.gzip - budgetBytes }))
    .sort((a, b) => b.over - a.over);
}

export type FloorFailure = { route: string; documentRaw: number; floor: number };

/**
 * Routes whose *document* (stylesheet excluded) is too thin to have come from
 * a stack with data in it.
 *
 * This is the guard the incident is named after: the first measurements were
 * all taken on empty pages, a second run reproduced them "to within 6 bytes"
 * because that stack was empty too, and the resulting budget was off by
 * nearly 2x. The floor is on the document rather than the whole response
 * precisely because the inlined stylesheet keeps an empty page looking big.
 */
export function emptyStackFailures(rows: Weighed[], floors: Record<string, number>): FloorFailure[] {
  return rows
    .filter((r) => floors[r.route] !== undefined && r.documentRaw < floors[r.route])
    .map((r) => ({ route: r.route, documentRaw: r.documentRaw, floor: floors[r.route] }));
}

/**
 * How many **distinct** items a page rendered, per an item-link pattern.
 *
 * Distinct, because a page can link the same item more than once: `/recipes`
 * lists a recipe and may also surface it under "suggestions", `/groups` links
 * a family from several of its sub-pages. Counting raw matches would report
 * 45 recipes for 25 stored — indistinguishable from a bug at a glance.
 */
export function countRendered(html: string, pattern: RegExp): number {
  // A fresh regex each call: a `/g` literal carries `lastIndex` across calls.
  const matches = [...html.matchAll(new RegExp(pattern.source, "g"))].map((m) => m[0]);
  return new Set(matches).size;
}

export type CollectionCheck = {
  route: string;
  /** French label for the error message, e.g. "événements". */
  collection: string;
  /** Rows the API says the group holds, **within the window the page shows**. */
  stored: number;
  /** Rows the page actually rendered. */
  rendered: number;
  /** Rows this route ought to render — see `expectedRendered`. */
  expected: number;
  /**
   * Rows the displayed window must hold for a measurement to mean anything —
   * i.e. what `npm run seed` puts there. Absent = no volume expectation.
   */
  minStored?: number;
};

export type CollectionFailure = CollectionCheck & {
  kind: "unseeded" | "understocked" | "not-rendered" | "incomplete";
};

/**
 * Combien de lignes une route doit rendre pour que sa mesure compte.
 *
 * `pageSize` est la tolérance, **déclarée route par route** et non globale :
 * seule `/messagerie` pagine (50 par page côté API), partout ailleurs un rendu
 * partiel est une mesure creuse, pas une pagination.
 */
export function expectedRendered(stored: number, pageSize: number | undefined): number {
  return pageSize === undefined ? stored : Math.min(stored, pageSize);
}

/**
 * Valide un entier venu de la ligne de commande ou de l'environnement.
 *
 * `Number("oops")` vaut `NaN`, et toute comparaison avec `NaN` est fausse :
 * un `--budget=oops` faisait donc passer le budget en silence — sur le seul
 * drapeau qui pilote le code de sortie. On refuse au lieu de rendre vert.
 */
export function requirePositiveInt(name: string, raw: string): number {
  const value = Number(raw.trim());
  if (raw.trim() === "" || !Number.isInteger(value) || value <= 0) {
    throw new Error(
      `${name} attend un entier positif en octets, reçu « ${raw} ». ` +
        "Refus de mesurer : une valeur non numérique rendrait la comparaison au " +
        "budget toujours fausse, donc le contrôle vert quoi qu'il pèse.",
    );
  }
  return value;
}

/**
 * The guard a byte floor cannot give you.
 *
 * A page whose collection comes back empty is *not* a small page: the month
 * grid still draws its cells and the inlined stylesheet still dominates. When
 * 40 events were seeded into the wrong month, an earlier manual campaign
 * (issue #83, a different dataset) recorded 1 881 bytes of document instead of
 * 2 807 — a third light, and utterly unremarkable in a table of numbers. This
 * repository's own run of the same mistake cost 36 %. Comparing rendered rows
 * against stored rows catches both that and a plainly unseeded stack, and
 * tells them apart:
 *
 * - `unseeded`: the API holds nothing → the seed script was never run.
 * - `understocked`: the displayed window holds fewer rows than `npm run seed`
 *   puts there. Counting "stored" *inside* the window the page shows is what
 *   makes the seed's idempotence honest, but it also makes rendered-vs-stored
 *   self-consistent when the window itself is half empty: 18 events seeded
 *   into next month render 18 of 18 while `/agenda` weighs 36 % less. Only an
 *   expected **volume** separates that from a legitimately smaller page.
 * - `not-rendered`: the API holds rows the page shows none of.
 * - `incomplete`: the page renders **some** of them, but fewer than it owes.
 *
 * Ce dernier cas est celui qu'un contrôle « à zéro » laisse passer, et il n'a
 * rien de théorique : la grille du mois fait 42 jours à partir du lundi
 * précédant le 1er, donc semer en août et mesurer en juillet rend 18
 * événements sur 40 — `/agenda` dégonflé de 36 %, exit 0, une seule cellule
 * `18/40` à remarquer dans le tableau. Et semer en juillet pour mesurer en
 * août n'en rend que 2, sans aucune variable d'environnement.
 *
 * La tolérance vit dans `expected` (voir `expectedRendered`), déclarée route
 * par route : seule `/messagerie` a le droit de rendre moins que ce qu'elle
 * stocke, parce qu'elle pagine.
 */
export function collectionFailures(checks: CollectionCheck[]): CollectionFailure[] {
  return checks.flatMap((c) => {
    if (c.stored === 0) return [{ ...c, kind: "unseeded" as const }];
    if (c.minStored !== undefined && c.stored < c.minStored) {
      return [{ ...c, kind: "understocked" as const }];
    }
    if (c.rendered === 0) return [{ ...c, kind: "not-rendered" as const }];
    if (c.rendered < c.expected) return [{ ...c, kind: "incomplete" as const }];
    return [];
  });
}
