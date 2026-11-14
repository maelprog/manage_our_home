import { expect, Page, test } from "@playwright/test";
import { parisDay } from "../lib/dates";
import { fetchVerificationToken } from "../lib/db";

// Issue #74 — "Toute cible interactive ≥ 44 px" (DESIGN.md → Espacement).
// The Rust guards in apps/web/src/app.rs can read a padding or a min-height
// out of the sheet; only a browser can say how tall a control ends up once
// the font's line box, the border and the UA's own form styling are added.
// So the boxes are measured here, on the real pages, at both layouts.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-touch-targets-password-1";
const MIN = 44;

async function registerAndLogin(page: Page, displayName: string): Promise<void> {
  const email = uniqueEmail("touch");
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill(displayName);
  await page.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
  await page.getByRole("textbox", { name: "Confirmation" }).fill(PASSWORD);
  await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
  await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
  await page.getByRole("button", { name: "Créer mon compte" }).click();
  await expect(page).toHaveURL(/\/register\/check-email$/);
  const token = await fetchVerificationToken(email);
  await page.goto(`/verify-email?token=${token}`);
  await page.goto("/login");
  await page.getByLabel("Email").fill(email);
  await page.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
  await page.getByRole("button", { name: "Se connecter" }).click();
  await expect(page).toHaveURL("/");
}

/** `YYYY-MM-DDTHH:MM` for today at the given hour, local to the browser. */
function todayAt(hour: number): string {
  const d = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(hour)}:00`;
}

/**
 * One family with something on each page, so every component renders.
 * Returns the pages whose path carries an id the seed only learns here: the
 * member list, the seeded member's profile, and the event's detail (its
 * assignee, the creator, is a `.member` link — #466).
 */
async function seed(page: Page): Promise<string[]> {
  await registerAndLogin(page, "Touch Tester");
  await page.goto("/groups/new");
  await page.getByLabel("Nom du groupe").fill("Famille Cibles");
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);

  await page.goto("/grocery-list");
  await page.getByLabel("Article").fill("Pain");
  await page.getByRole("button", { name: "Ajouter" }).click();
  await expect(page).toHaveURL(/\/grocery-list\?notice=item_added$/);

  await page.goto("/messagerie");
  await page.getByLabel("Votre message").fill("Bonjour");
  await page.getByRole("button", { name: "Envoyer" }).click();
  await expect(page.locator("li[data-message-id]", { hasText: "Bonjour" })).toBeVisible();

  await page.goto("/agenda/new");
  await page.getByLabel("Titre").fill("Rendez-vous");
  await page.getByLabel("Début").fill(todayAt(10));
  await page.getByLabel("Fin").fill(todayAt(11));
  await page.getByRole("button", { name: "Créer l'événement" }).click();
  await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);
  const eventHref = await page.locator("a.bar", { hasText: "Rendez-vous" }).first().getAttribute("href");
  if (!eventHref) throw new Error("no link to the seeded event");
  // The visit below measures the assignee's link only if there is one.
  await page.goto(eventHref);
  await expect(page.locator("a.member")).toHaveCount(1);

  await page.goto("/groups");
  const membersHref = await page.getByRole("link", { name: "Membres" }).getAttribute("href");
  if (!membersHref) throw new Error("no link to the seeded group's members");
  await page.goto(membersHref);
  const profileHref = await page.locator("a.member").first().getAttribute("href");
  if (!profileHref) throw new Error("no link to the seeded member's profile");

  // A day of the coming week with what the day view draws under 44px (#838):
  // two lines of the « Journée » banner in a row (events of 24 hours) and a
  // block of half an hour, the slot of 14:30 right under it. Its week shows
  // the day's number. Not today, whose line of the current time would cross
  // a block wherever the clock stands.
  const day = parisDay(7);
  const next = parisDay(8);
  for (const [title, starts, ends] of [
    ["Congrès", `${day}T00:00`, `${next}T00:00`],
    ["Séminaire", `${day}T00:00`, `${next}T00:00`],
    ["Pharmacie", `${day}T14:00`, `${day}T14:30`],
  ]) {
    await page.goto("/agenda/new");
    await page.getByLabel("Titre").fill(title);
    await page.getByLabel("Début").fill(starts);
    await page.getByLabel("Fin").fill(ends);
    await page.getByRole("button", { name: "Créer l'événement" }).click();
    await expect(page).toHaveURL(/notice=event_created$/);
  }

  return [membersHref, profileHref, eventHref, `/agenda?view=day&date=${day}`, `/agenda?view=week&date=${day}`];
}

interface Box {
  what: string;
  width: number;
  height: number;
}

/**
 * Every control the issue lists, measured as a finger meets it: a checkbox
 * by the `<label>` that toggles it, anything else by its own box — buttons
 * and button-styled links, fields, nav links, the password toggle, the
 * agenda's arrows and chips, a disclosure's summary, a `.links` column, a
 * member's name as a link to their profile (`.member`, #466 — the seeded
 * message's author on /messagerie, the member list, the event's assignee),
 * every day of the phone's mini grid, a link to its day view (#675, #793),
 * every day of the month's grid, a link as tall as its week (#836), and the
 * day view's own head (#793).
 *
 * Left out, each for a stated reason:
 * - a bare text link (`<a>` with no class): inside a sentence it is WCAG
 *   2.5.5's inline exception, and the rest — a name in a list row, "Membres"
 *   on /groups — are not in the issue's list and are not closed by it;
 * - the calendar's chips on a wide screen: three to a 6.5rem day by design,
 *   a pointer layout (below the breakpoint they are 48px rows, checked);
 * - a checkbox with no label whose form submits the same change through a
 *   button (see below);
 * - the skip link, which is off-screen until it takes focus;
 * - three targets of the day view under 44px, « Créneau d'abord »
 *   (arbitrage of 2026-11-14, #838; DESIGN.md → « L'agenda sur téléphone :
 *   vue jour »): a half hour of the hours (`.slots a`, #794), 28px tall,
 *   half a row of 56px; a line of the « Journée » banner (`.ad`), 32px; a
 *   block of the hours (`.blk`) shorter than 44px — 26px for half an hour.
 *   A box reaching past the line or the block would lie over a slot (the
 *   banner's `.slot` is under its whole surface, the hours' `.slots a`
 *   under every block), and a tap there must create, not open:
 *   `slotsCovered` below holds that;
 * - the month's bars and « +N » (#836, `.bar`, `.plus`), drawn at the
 *   mock-up's size: one line of a title, 19px tall on a phone and 24px on a
 *   PC (26px outlined, and in a row holding an outlined one); « +N » 20px
 *   on a phone, 24px on a PC — the day they sit on, a link to its day view,
 *   is measured.
 */
async function undersized(page: Page): Promise<Box[]> {
  return page.evaluate((min) => {
    const out: { what: string; width: number; height: number }[] = [];
    const nodes = document.querySelectorAll<HTMLElement>(
      'button, .btn, input:not([type="hidden"]), select, textarea, summary, .navlink, .chip, .links a, .member, .mini a, .day',
    );
    const wide = window.matchMedia("(min-width: 861px)").matches;
    for (const node of nodes) {
      if (node.closest(".skip-link")) continue;
      if (wide && node.matches(".chip")) continue;
      const style = getComputedStyle(node);
      if (style.display === "none" || style.visibility === "hidden") continue;
      const isBox = node.matches('input[type="checkbox"], input[type="radio"]');
      // A bare box whose own form has a submit button doing the same thing —
      // the grocery row's checkbox beside its "Cocher" / "Décocher" until
      // #526 made the box the button: WCAG 2.5.5's equivalent-control
      // exception. The button is measured.
      if (isBox && !node.closest("label")) {
        const form = (node as HTMLInputElement).form;
        if (form?.querySelector('button[type="submit"], button:not([type])')) continue;
      }
      const target = (isBox && node.closest("label")) || node;
      const rect = target.getBoundingClientRect();
      if (rect.width === 0 && rect.height === 0) continue;
      // Half a pixel of tolerance for sub-pixel layout, not for design.
      if (rect.height + 0.5 < min || rect.width + 0.5 < min) {
        const label =
          (node.getAttribute("aria-label") ?? node.textContent ?? "").trim().slice(0, 30) ||
          node.getAttribute("name") ||
          "";
        out.push({
          what: `${node.tagName.toLowerCase()}.${node.className}${isBox ? " (label)" : ""} "${label}"`,
          width: Math.round(rect.width * 10) / 10,
          height: Math.round(rect.height * 10) / 10,
        });
      }
    }
    return out;
  }, MIN);
}

/**
 * A target drawn smaller than it is (#838): a day's number in the week
 * (`.cal-day a`, 28px square on a PC, 25px tall in the phone's list)
 * reaches 44px by a box that paints nothing (`::before`), which no bounding
 * box reports. It is measured as a finger meets it, by asking the page
 * which link lies under each pixel of a vertical and a horizontal line
 * through the link's centre, and counting the run that is this link.
 * Whatever covers it — a chip, the next day — shortens the run.
 */
async function undersizedHitAreas(page: Page): Promise<Box[]> {
  return page.evaluate((min) => {
    const out: { what: string; width: number; height: number }[] = [];
    for (const node of document.querySelectorAll<HTMLElement>(".cal-day a")) {
      if (node.getClientRects().length === 0) continue;
      node.scrollIntoView({ block: "center", inline: "nearest", behavior: "instant" });
      const r = node.getBoundingClientRect();
      const cx = Math.round(r.left + r.width / 2);
      const cy = Math.round(r.top + r.height / 2);
      const on = (x: number, y: number) => document.elementFromPoint(x, y)?.closest("a") === node;
      const run = (dx: number, dy: number) => {
        let n = 1;
        while (n < 100 && on(cx + dx * n, cy + dy * n)) n++;
        return n - 1;
      };
      if (!on(cx, cy)) {
        out.push({ what: `${node.className} "${(node.textContent ?? "").trim().slice(0, 30)}" covered`, width: 0, height: 0 });
        continue;
      }
      const height = run(0, -1) + 1 + run(0, 1);
      const width = run(-1, 0) + 1 + run(1, 0);
      // A pixel of tolerance: the run counts whole pixels of a box placed to
      // the fraction.
      if (height + 1 < min || width + 1 < min) {
        out.push({
          what: `a.${node.className} (hit area) "${(node.textContent ?? "").trim().slice(0, 30)}"`,
          width,
          height,
        });
      }
    }
    return out;
  }, MIN);
}

/**
 * The other side of a box that paints nothing: it must not take a tap from
 * another link. On every link of the agenda's day and week views, each
 * point of a 4px grid over the link's visible box is asked which link lies
 * there; a point that goes to another link, outside the box that link
 * draws, is a point an invisible target took. A block drawn over a slot
 * takes that part of it by design and is not counted: what is held is that
 * the slots of #794 (`.slots a`, the banner's `.slot`) keep every point
 * nothing is drawn on (« Créneau d'abord », #838).
 */
async function slotsCovered(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const out: string[] = [];
    const name = (a: Element) =>
      `a.${a.className} "${(a.textContent ?? "").trim().replace(/\s+/g, " ").slice(0, 40)}"`;
    for (const link of document.querySelectorAll<HTMLElement>("a")) {
      if (link.getClientRects().length === 0) continue;
      link.scrollIntoView({ block: "center", inline: "nearest", behavior: "instant" });
      const r = link.getBoundingClientRect();
      // What of the link can be seen: the window, cut by every scroller
      // around it.
      let [x0, y0, x1, y1] = [Math.max(r.left, 0), Math.max(r.top, 0), Math.min(r.right, innerWidth), Math.min(r.bottom, innerHeight)];
      for (let p = link.parentElement; p; p = p.parentElement) {
        if (getComputedStyle(p).overflow === "visible") continue;
        const c = p.getBoundingClientRect();
        [x0, y0, x1, y1] = [Math.max(x0, c.left), Math.max(y0, c.top), Math.min(x1, c.right), Math.min(y1, c.bottom)];
      }
      const takers = new Map<Element, number>();
      let points = 0;
      for (let y = Math.ceil(y0) + 1; y < y1 - 1; y += 4) {
        for (let x = Math.ceil(x0) + 1; x < x1 - 1; x += 4) {
          points++;
          const hit = document.elementFromPoint(x, y);
          const other = hit?.closest("a");
          if (!hit || !other || other === link) continue;
          const b = hit.getBoundingClientRect();
          if (x >= b.left - 1 && x <= b.right + 1 && y >= b.top - 1 && y <= b.bottom + 1) continue;
          takers.set(other, (takers.get(other) ?? 0) + 1);
        }
      }
      for (const [other, n] of takers) {
        out.push(`${name(link)}: ${n} of ${points} points taken by ${name(other)} outside what it draws`);
      }
    }
    return out;
  });
}

const PUBLIC_PAGES = [
  "/login",
  "/register",
  "/register/check-email",
  "/forgot-password",
  "/verify-email/resend",
];
const APP_PAGES = [
  "/",
  "/agenda",
  "/agenda?view=week",
  "/agenda?view=day",
  "/agenda/new",
  "/grocery-list",
  "/messagerie",
  "/stocks",
  "/stocks/new",
  "/recipes",
  "/budget",
  "/groups",
  "/account",
];

for (const [layout, viewport] of [
  ["phone", { width: 390, height: 844 }],
  ["desktop", { width: 1280, height: 800 }],
] as const) {
  test.describe(`touch targets — ${layout}`, () => {
    test.use({ viewport });

    test("every interactive element is at least 44px both ways", async ({ page }) => {
      const failures: string[] = [];
      const visit = async (path: string) => {
        await page.goto(path);
        // Today's day view is left to the boxes alone: its line of the
        // current time crosses whatever block stands at that hour.
        const hits = path === "/agenda?view=day" ? [] : await undersizedHitAreas(page);
        for (const b of [...(await undersized(page)), ...hits]) {
          failures.push(`${path}: ${b.what} ${b.width}×${b.height}`);
        }
        if (/^\/agenda\?view=(day|week)/.test(path)) {
          for (const c of await slotsCovered(page)) failures.push(`${path}: ${c}`);
        }
      };
      for (const path of PUBLIC_PAGES) await visit(path);
      const seeded = await seed(page);
      for (const path of [...APP_PAGES, ...seeded]) await visit(path);
      expect(failures, failures.join("\n")).toEqual([]);
    });
  });
}
