import assert from "node:assert/strict";
import test from "node:test";

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
} from "./measure-core.ts";

// ---------------------------------------------------------------------------
// parseNavRoutes — reads the real nav out of apps/web/src/app.rs.
//
// This is the guard against measurement mistake #1: a hand-written route
// list that quietly drifts from the nav (the first run of this measurement
// swapped /messagerie for /account and nobody noticed).
// ---------------------------------------------------------------------------

// Since #69 the nav is a `const NAV` table: every link carries an
// `aria-current` decided per page, which eight literal `<a>`s in the
// `format!` could not.
const APP_RS_NAV = `
const NAV: [(&str, &str); 3] = [
    ("/", "Accueil"),
    ("/agenda", "Agenda"),
    ("/messagerie", "Messagerie"),
];

pub fn app_header(me: &MeResponse) -> String {
    let links: String = NAV
        .iter()
        .copied()
        .chain(me.is_superadmin.then_some(("/admin/users", "Admin")))
        .map(|(href, label)| navlink(href, label, redirect_to))
        .collect();
    format!(
        r#"<header>
<div class="page-header">
<nav class="actions">{links}</nav>
<span class="actions">
<a class="navlink" href="/account"{account_current}>Mon compte</a>
</span>
</div>
</header>"#,
    )
}
`;

test("parseNavRoutes collects the nav's hrefs in order", () => {
  const parsed = parseNavRoutes(APP_RS_NAV);
  assert.deepEqual(parsed.hrefs, ["/", "/agenda", "/messagerie"]);
});

test("parseNavRoutes ignores links outside the nav table", () => {
  // /account is a header link, not a nav entry — substituting it for a real
  // nav entry is exactly the bug this parser exists to make impossible.
  const parsed = parseNavRoutes(APP_RS_NAV);
  assert.equal(parsed.hrefs.includes("/account"), false);
});

test("parseNavRoutes reports the entries chained onto the table under a condition", () => {
  const parsed = parseNavRoutes(APP_RS_NAV);
  assert.deepEqual(parsed.conditional, ["/admin/users"]);
});

test("parseNavRoutes throws when the nav table cannot be found", () => {
  assert.throws(() => parseNavRoutes("fn app_header() {}"), /const NAV/);
});

test("parseNavRoutes throws when several nav tables match", () => {
  assert.throws(() => parseNavRoutes(APP_RS_NAV + APP_RS_NAV), /2 /);
});

// ---------------------------------------------------------------------------
// diffNavRoutes — the declared list must equal the nav, entry for entry.
// ---------------------------------------------------------------------------

const CONDITIONAL = ["/admin/users"];

test("diffNavRoutes is silent when the declared list matches the nav", () => {
  const diff = diffNavRoutes(
    ["/", "/agenda", "/messagerie"],
    parseNavRoutes(APP_RS_NAV),
    CONDITIONAL,
  );
  assert.deepEqual(diff, { missing: [], unexpected: [], unknownConditional: [] });
});

test("diffNavRoutes reports a nav entry the declared list forgot", () => {
  const diff = diffNavRoutes(["/", "/agenda"], parseNavRoutes(APP_RS_NAV), CONDITIONAL);
  assert.deepEqual(diff.missing, ["/messagerie"]);
  assert.deepEqual(diff.unexpected, []);
});

test("diffNavRoutes reports a declared route that is not in the nav", () => {
  const diff = diffNavRoutes(
    ["/", "/agenda", "/messagerie", "/account"],
    parseNavRoutes(APP_RS_NAV),
    CONDITIONAL,
  );
  assert.deepEqual(diff.unexpected, ["/account"]);
});

test("diffNavRoutes reports a conditional entry nobody accounted for", () => {
  const diff = diffNavRoutes(["/", "/agenda", "/messagerie"], parseNavRoutes(APP_RS_NAV), []);
  assert.deepEqual(diff.unknownConditional, ["/admin/users"]);
});

test("diffNavRoutes does not require the conditional route to be measured", () => {
  // /admin/users 404s for everyone but the single technical superadmin, so it
  // is deliberately declared-but-not-measured. Knowing about it is enough.
  const diff = diffNavRoutes(
    ["/", "/agenda", "/messagerie"],
    parseNavRoutes(APP_RS_NAV),
    CONDITIONAL,
  );
  assert.deepEqual(diff.unknownConditional, []);
});

// ---------------------------------------------------------------------------
// splitInlineStylesheet — the number that was missing from the first budget.
// ---------------------------------------------------------------------------

test("splitInlineStylesheet subtracts the inlined CSS from the response", () => {
  const html = "<head><style>body{color:red}</style></head><body>x</body>";
  const split = splitInlineStylesheet(html);
  assert.equal(split.totalBytes, Buffer.byteLength(html));
  assert.equal(split.styleBytes, Buffer.byteLength("body{color:red}"));
  assert.equal(split.documentBytes, split.totalBytes - split.styleBytes);
  assert.equal(split.hasInlineStylesheet, true);
});

test("splitInlineStylesheet counts bytes, not UTF-16 code units", () => {
  const html = "<style>a{}</style><p>éé</p>";
  const split = splitInlineStylesheet(html);
  assert.equal(split.totalBytes, Buffer.byteLength(html));
  assert.notEqual(split.totalBytes, html.length);
  assert.equal(split.documentBytes, Buffer.byteLength("<style></style><p>éé</p>"));
});

test("splitInlineStylesheet sums several style elements", () => {
  const split = splitInlineStylesheet("<style>ab</style><p>x</p><style>cde</style>");
  assert.equal(split.styleBytes, 5);
});

test("splitInlineStylesheet flags a response with no inlined stylesheet", () => {
  const split = splitInlineStylesheet("<p>rien</p>");
  assert.equal(split.hasInlineStylesheet, false);
  assert.equal(split.styleBytes, 0);
  assert.equal(split.documentBytes, split.totalBytes);
});

// ---------------------------------------------------------------------------
// budgetViolations — drives the process exit code.
// ---------------------------------------------------------------------------

const ROWS = [
  { route: "/", gzip: 8_000, documentRaw: 3_000 },
  { route: "/agenda", gzip: 15_615, documentRaw: 40_000 },
];

test("budgetViolations returns nothing when every route fits", () => {
  assert.deepEqual(budgetViolations(ROWS, 20_000), []);
});

test("budgetViolations reports the routes over budget, worst first", () => {
  const over = budgetViolations([...ROWS, { route: "/budget", gzip: 30_000, documentRaw: 1 }], 10_000);
  assert.deepEqual(
    over.map((v) => v.route),
    ["/budget", "/agenda"],
  );
  assert.equal(over[0].over, 20_000);
});

test("budgetViolations treats a route exactly on budget as passing", () => {
  assert.deepEqual(budgetViolations([{ route: "/", gzip: 100, documentRaw: 1 }], 100), []);
});

// ---------------------------------------------------------------------------
// emptyStackFailures — the guard against measuring an unseeded stack, which
// is measurement mistake #2 (two agents confirmed each other's numbers taken
// on empty pages).
// ---------------------------------------------------------------------------

const FLOORS = { "/agenda": 6_000, "/messagerie": 4_000 };

test("emptyStackFailures passes a populated stack", () => {
  const failures = emptyStackFailures(
    [
      { route: "/agenda", gzip: 1, documentRaw: 9_000 },
      { route: "/messagerie", gzip: 1, documentRaw: 7_000 },
    ],
    FLOORS,
  );
  assert.deepEqual(failures, []);
});

test("emptyStackFailures catches a route whose document is suspiciously thin", () => {
  const failures = emptyStackFailures(
    [
      { route: "/agenda", gzip: 1, documentRaw: 2_100 },
      { route: "/messagerie", gzip: 1, documentRaw: 7_000 },
    ],
    FLOORS,
  );
  assert.deepEqual(failures, [{ route: "/agenda", documentRaw: 2_100, floor: 6_000 }]);
});

test("emptyStackFailures ignores routes with no declared floor", () => {
  assert.deepEqual(emptyStackFailures([{ route: "/groups", gzip: 1, documentRaw: 12 }], FLOORS), []);
});

// ---------------------------------------------------------------------------
// countRendered / collectionFailures — the guard a size floor cannot provide.
//
// A page whose collection renders empty still weighs its normal amount: the
// month grid draws all its cells, the shell and the inlined stylesheet dwarf
// the rows. Seeding events outside the displayed month therefore produced a
// perfectly plausible-looking 1 881 bytes instead of 2 807 — a 33 % shortfall
// no byte floor would have caught. (Those two figures come from the earlier
// manual campaign of issue #83, on a different dataset from this seed's; this
// repository's own run of the same mistake cost 36 %.) So: compare what the
// page *rendered* against what the API says is *stored*.
// ---------------------------------------------------------------------------

test("countRendered counts the item links a populated list renders", () => {
  const html =
    '<a href="/stocks/new">Nouvel article</a>' +
    '<a href="/stocks/11111111-1111-4111-8111-111111111111">Farine</a>' +
    '<a href="/stocks/22222222-2222-4222-8222-222222222222">Riz</a>';
  assert.equal(countRendered(html, /href="\/stocks\/[0-9a-f-]{36}"/g), 2);
});

test("countRendered does not count the static new/creation links", () => {
  assert.equal(countRendered('<a href="/stocks/new">x</a>', /href="\/stocks\/[0-9a-f-]{36}"/g), 0);
});

test("countRendered returns 0 for an empty page rather than throwing", () => {
  assert.equal(countRendered("<p>Aucun article</p>", /href="\/stocks\/[0-9a-f-]{36}"/g), 0);
});

test("countRendered counts each item once even when the page links it twice", () => {
  // /recipes links a recipe from the list *and* from the suggestions block;
  // /groups links a family from several of its sub-pages. Counting raw
  // matches would report 45 recipes for 25 stored and look like a bug.
  const twice =
    '<a href="/recipes/11111111-1111-4111-8111-111111111111">Gratin</a>' +
    '<a href="/recipes/11111111-1111-4111-8111-111111111111">Gratin (suggestion)</a>' +
    '<a href="/recipes/22222222-2222-4222-8222-222222222222">Quiche</a>';
  assert.equal(countRendered(twice, /href="\/recipes\/[0-9a-f-]{36}"/g), 2);
});

test("collectionFailures passes when the page renders what the API stores", () => {
  assert.deepEqual(
    collectionFailures([{ route: "/agenda", collection: "événements", stored: 40, rendered: 40 }]),
    [],
  );
});

test("collectionFailures flags an unseeded stack when nothing is stored", () => {
  const [failure] = collectionFailures([
    { route: "/agenda", collection: "événements", stored: 0, rendered: 0 },
  ]);
  assert.equal(failure.kind, "unseeded");
  assert.equal(failure.route, "/agenda");
});

test("collectionFailures flags rows that exist but are not rendered", () => {
  // Trap #1: 40 events seeded in August, /agenda showing the current month.
  const [failure] = collectionFailures([
    { route: "/agenda", collection: "événements", stored: 40, rendered: 0 },
  ]);
  assert.equal(failure.kind, "not-rendered");
  assert.equal(failure.stored, 40);
  assert.equal(failure.rendered, 0);
});

test("collectionFailures accepts a page that renders its full first page of rows", () => {
  // /messagerie stores 55 but pages at 50. La tolérance est déclarée route par
  // route (`expected`), jamais globale : ailleurs, un rendu partiel est un bug.
  assert.deepEqual(
    collectionFailures([
      { route: "/messagerie", collection: "messages", stored: 55, rendered: 50, expected: 50 },
    ]),
    [],
  );
});

// --- le contrôle de complétude (et non plus « à zéro ») --------------------
//
// La première version ne se déclenchait qu'à `rendered === 0`. Or la grille
// du mois fait 42 jours à partir du lundi précédant le 1er : semer en août et
// mesurer en juillet rend 18 événements sur 40 — garde-fou passé, /agenda
// dégonflé de 36 %, exit 0. Pire, semer en juillet et mesurer en août ne rend
// que les 2 événements des 27-28 juillet, toujours sans variable
// d'environnement. C'est le mode de panne d'origine, reproduit dans l'outil
// censé l'éliminer. Le contrôle porte donc sur `rendered < expected`.

// Le trou que la seule comparaison rendu/stocké laisse ouvert : si « stocké »
// est compté DANS la fenêtre affichée, une fenêtre à moitié vide donne
// 18 rendus sur 18 stockés — cohérent, complet, et pourtant /agenda pèse 36 %
// de moins. Il faut donc aussi un volume ATTENDU par collection : une mesure
// n'a de sens que sur une stack peuplée à la hauteur de ce que `npm run seed`
// pose. C'est ce contrôle qui remplace les planchers d'octets, lesquels ne se
// déclenchaient que sur une page totalement blanche.

test("collectionFailures refuses a window holding fewer rows than the seed puts there", () => {
  const [failure] = collectionFailures([
    { route: "/agenda", collection: "événements", stored: 18, rendered: 18, expected: 18, minStored: 40 },
  ]);
  assert.equal(failure.kind, "understocked");
  assert.equal(failure.stored, 18);
  assert.equal(failure.minStored, 40);
});

test("collectionFailures accepts a window holding more than the minimum", () => {
  // Deux mois de seeding cumulés : la fenêtre courante est pleine, la mesure
  // est valide même si d'autres lignes dorment hors fenêtre.
  assert.deepEqual(
    collectionFailures([
      { route: "/agenda", collection: "événements", stored: 40, rendered: 40, expected: 40, minStored: 40 },
    ]),
    [],
  );
});

test("collectionFailures skips the volume check when no minimum is declared", () => {
  assert.deepEqual(
    collectionFailures([
      { route: "/groups", collection: "familles", stored: 1, rendered: 1, expected: 1 },
    ]),
    [],
  );
});

test("collectionFailures catches a partially rendered collection", () => {
  const [failure] = collectionFailures([
    { route: "/agenda", collection: "événements", stored: 40, rendered: 18, expected: 40 },
  ]);
  assert.equal(failure.kind, "incomplete");
  assert.equal(failure.rendered, 18);
  assert.equal(failure.expected, 40);
});

test("collectionFailures catches the two-of-forty case a zero-check misses", () => {
  const [failure] = collectionFailures([
    { route: "/agenda", collection: "événements", stored: 40, rendered: 2, expected: 40 },
  ]);
  assert.equal(failure.kind, "incomplete");
});

test("collectionFailures still separates unseeded from rendered-nothing", () => {
  const none = collectionFailures([
    { route: "/agenda", collection: "événements", stored: 0, rendered: 0, expected: 0 },
  ]);
  assert.equal(none[0].kind, "unseeded");
  const invisible = collectionFailures([
    { route: "/agenda", collection: "événements", stored: 40, rendered: 0, expected: 40 },
  ]);
  assert.equal(invisible[0].kind, "not-rendered");
});

test("collectionFailures does not complain when a page renders MORE than expected", () => {
  // Ne devrait pas arriver, mais un sur-rendu n'est pas une mesure creuse.
  assert.deepEqual(
    collectionFailures([
      { route: "/groups", collection: "familles", stored: 1, rendered: 2, expected: 1 },
    ]),
    [],
  );
});

// --- expectedRendered — la tolérance, déclarée par route -------------------

test("expectedRendered demands every row on an unpaginated route", () => {
  assert.equal(expectedRendered(40, undefined), 40);
});

test("expectedRendered caps at the page size on a paginated route", () => {
  assert.equal(expectedRendered(55, 50), 50);
  assert.equal(expectedRendered(30, 50), 30);
});

// --- requirePositiveInt — le drapeau qui pilote le code de sortie ----------
//
// `Number("oops")` vaut NaN, et `gzip > NaN` est toujours faux : un
// `--budget=oops` rendait le gate vert alors qu'une route dépassait.

test("requirePositiveInt accepts a plain integer", () => {
  assert.equal(requirePositiveInt("--budget", "14336"), 14336);
});

test("requirePositiveInt rejects a non-numeric value instead of yielding NaN", () => {
  assert.throws(() => requirePositiveInt("--budget", "oops"), /--budget/);
});

test("requirePositiveInt rejects zero, negatives and fractions", () => {
  assert.throws(() => requirePositiveInt("--budget", "0"), /--budget/);
  assert.throws(() => requirePositiveInt("--budget", "-5"), /--budget/);
  assert.throws(() => requirePositiveInt("--budget", "1.5"), /--budget/);
});

test("requirePositiveInt rejects the empty string and whitespace", () => {
  assert.throws(() => requirePositiveInt("--budget", ""), /--budget/);
  assert.throws(() => requirePositiveInt("--budget", "  "), /--budget/);
});

// --- optionalPositiveInt — les six entrées qui pilotent le garde-fou -------
//
// Les volumes attendus (SEED_EVENTS & co.) passaient par un `Number()` nu.
// `Number("oops")` vaut NaN et `stored < NaN` est faux, donc une variable
// invalide ÉTEIGNAIT le contrôle `understocked` en silence — et la mesure
// retombait sur la tautologie « 18 rendus sur 18 stockés » que ce contrôle
// existe pour fermer. Même mode de panne que celui qui a motivé
// `requirePositiveInt`, sur le bouton documenté juste à côté : le bouton
// annoncé était aussi l'interrupteur.

test("optionalPositiveInt falls back when the variable is unset", () => {
  assert.equal(optionalPositiveInt("SEED_EVENTS", undefined, 40), 40);
});

test("optionalPositiveInt uses a valid override", () => {
  assert.equal(optionalPositiveInt("SEED_EVENTS", "12", 40), 12);
});

test("optionalPositiveInt refuses a non-numeric override rather than disabling the guard", () => {
  assert.throws(() => optionalPositiveInt("SEED_EVENTS", "oops", 40), /SEED_EVENTS/);
});

test("optionalPositiveInt refuses an empty override, which `??` does not catch", () => {
  // `process.env.X ?? 40` laisse passer la chaîne vide : elle n'est pas
  // nullish, donc `Number("")` valait 0 et éteignait le contrôle.
  assert.throws(() => optionalPositiveInt("SEED_EVENTS", "", 40), /SEED_EVENTS/);
});

test("optionalPositiveInt refuses zero, which would disable the guard too", () => {
  assert.throws(() => optionalPositiveInt("SEED_EVENTS", "0", 40), /SEED_EVENTS/);
});

test("optionalPositiveInt refuses a negative override", () => {
  assert.throws(() => optionalPositiveInt("SEED_EVENTS", "-1", 40), /SEED_EVENTS/);
});
