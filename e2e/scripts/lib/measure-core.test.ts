import assert from "node:assert/strict";
import test from "node:test";

import {
  budgetViolations,
  collectionFailures,
  countRendered,
  diffNavRoutes,
  emptyStackFailures,
  parseNavRoutes,
  splitInlineStylesheet,
} from "./measure-core.ts";

// ---------------------------------------------------------------------------
// parseNavRoutes — reads the real nav out of apps/web/src/app.rs.
//
// This is the guard against measurement mistake #1: a hand-written route
// list that quietly drifts from the nav (the first run of this measurement
// swapped /messagerie for /account and nobody noticed).
// ---------------------------------------------------------------------------

const APP_RS_NAV = `
pub fn app_header(me: &MeResponse) -> String {
    let admin_link = if me.is_superadmin {
        r#"<a href="/admin/users">Admin</a>"#
    } else {
        ""
    };
    format!(
        r#"<header>
<div class="muted page-header">
<nav class="actions">
<a href="/">Accueil</a>
<a href="/agenda">Agenda</a>
<a href="/messagerie">Messagerie</a>
{admin_link}
</nav>
<span class="actions">
<a href="/account">Mon compte</a>
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

test("parseNavRoutes ignores links outside the nav element", () => {
  // /account is a header link, not a nav entry — substituting it for a real
  // nav entry is exactly the bug this parser exists to make impossible.
  const parsed = parseNavRoutes(APP_RS_NAV);
  assert.equal(parsed.hrefs.includes("/account"), false);
});

test("parseNavRoutes reports the conditional slots interpolated into the nav", () => {
  const parsed = parseNavRoutes(APP_RS_NAV);
  assert.deepEqual(parsed.placeholders, ["admin_link"]);
});

test("parseNavRoutes throws when the nav element cannot be found", () => {
  assert.throws(() => parseNavRoutes("fn app_header() {}"), /nav class="actions"/);
});

test("parseNavRoutes throws when several nav elements match", () => {
  assert.throws(() => parseNavRoutes(APP_RS_NAV + APP_RS_NAV), /2 /);
});

// ---------------------------------------------------------------------------
// diffNavRoutes — the declared list must equal the nav, slot for slot.
// ---------------------------------------------------------------------------

const SLOTS = { admin_link: "/admin/users" };

test("diffNavRoutes is silent when the declared list matches the nav", () => {
  const diff = diffNavRoutes(["/", "/agenda", "/messagerie"], parseNavRoutes(APP_RS_NAV), SLOTS);
  assert.deepEqual(diff, { missing: [], unexpected: [], unknownPlaceholders: [] });
});

test("diffNavRoutes reports a nav entry the declared list forgot", () => {
  const diff = diffNavRoutes(["/", "/agenda"], parseNavRoutes(APP_RS_NAV), SLOTS);
  assert.deepEqual(diff.missing, ["/messagerie"]);
  assert.deepEqual(diff.unexpected, []);
});

test("diffNavRoutes reports a declared route that is not in the nav", () => {
  const diff = diffNavRoutes(
    ["/", "/agenda", "/messagerie", "/account"],
    parseNavRoutes(APP_RS_NAV),
    SLOTS,
  );
  assert.deepEqual(diff.unexpected, ["/account"]);
});

test("diffNavRoutes reports a conditional slot nobody accounted for", () => {
  const diff = diffNavRoutes(["/", "/agenda", "/messagerie"], parseNavRoutes(APP_RS_NAV), {});
  assert.deepEqual(diff.unknownPlaceholders, ["admin_link"]);
});

test("diffNavRoutes does not require the slot's route to be measured", () => {
  // /admin/users 404s for everyone but the single technical superadmin, so it
  // is deliberately declared-but-not-measured. Knowing the slot is enough.
  const diff = diffNavRoutes(["/", "/agenda", "/messagerie"], parseNavRoutes(APP_RS_NAV), SLOTS);
  assert.deepEqual(diff.unknownPlaceholders, []);
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
// no byte floor would have caught. So: compare what the page *rendered*
// against what the API says is *stored*.
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

test("collectionFailures accepts a page that renders only its first page of rows", () => {
  // /messagerie stores 55 but pages at 50 — partial rendering is normal, zero
  // rendering is not.
  assert.deepEqual(
    collectionFailures([{ route: "/messagerie", collection: "messages", stored: 55, rendered: 50 }]),
    [],
  );
});
