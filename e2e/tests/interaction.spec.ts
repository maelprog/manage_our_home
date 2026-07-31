import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Issue #69 — interaction states. Its "Vérification" is a manual one:
// "parcourir l'application entièrement au clavier (Tab / Entrée) : le focus
// doit être visible à chaque étape, sur les deux thèmes". A promise in a PR
// description is not a gate, so it is written down here instead.
//
// The Rust guards in apps/web/src/app.rs assert what the stylesheet *says*
// (an `outline` rule exists, transitions name only the three allowed
// properties, `--dur` drops to 0s under reduced motion). Only a browser can
// say what a visitor actually gets: which rule wins on the focused element,
// whether the ring is painted in the dark theme too, and whether the nav
// tells you where you are.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-interaction-password-1";

/** Register + verify + login a fresh user on the given page. */
async function registerAndLogin(page: Page, displayName: string): Promise<void> {
  const email = uniqueEmail("interaction");
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill(displayName);
  await page.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
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

async function createGroup(page: Page, name: string): Promise<void> {
  await page.goto("/groups/new");
  await page.getByLabel("Nom du groupe").fill(name);
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
}

interface Indicator {
  tag: string;
  label: string;
  outlineStyle: string;
  outlineWidth: string;
  outlineColor: string;
  boxShadow: string;
}

/** The focus indicator the browser is actually painting on `document.activeElement`. */
async function focusIndicator(page: Page): Promise<Indicator | null> {
  return page.evaluate(() => {
    const node = document.activeElement;
    if (!node || node === document.body) return null;
    const style = getComputedStyle(node);
    return {
      tag: node.tagName.toLowerCase(),
      label: (node.textContent ?? "").trim().slice(0, 40) || node.getAttribute("name") || "",
      outlineStyle: style.outlineStyle,
      outlineWidth: style.outlineWidth,
      outlineColor: style.outlineColor,
      boxShadow: style.boxShadow,
    };
  });
}

/** `rgba(…, 0)` — a colour that paints nothing at all. */
const TRANSPARENT = /rgba\([^)]*,\s*0\)/;

/**
 * A focus indicator counts if the element carries a *painted* outline or a
 * *painted* box-shadow ring. The fields deliberately hold a transparent
 * outline (so forced-colors mode still has something to repaint) and answer
 * with the shadow, which is why either one is enough — but a transparent one
 * of either is not one.
 *
 * The transparency check is not pedantry: read on the frame the element takes
 * focus, a field reports `rgba(0, 0, 0, 0) 0px 0px 0px 0px`, the start of the
 * 150 ms box-shadow transition. A predicate that only asked `boxShadow !==
 * "none"` passed on that and would have passed just as happily on a stylesheet
 * with no ring in it.
 */
function isVisible(indicator: Indicator): boolean {
  const outline =
    indicator.outlineStyle !== "none" &&
    parseFloat(indicator.outlineWidth) > 0 &&
    !TRANSPARENT.test(indicator.outlineColor);
  const ring = indicator.boxShadow !== "none" && !TRANSPARENT.test(indicator.boxShadow);
  return outline || ring;
}

/** Tabs `steps` times, asserting every stop settles on a focus indicator. */
async function walkWithTheKeyboard(page: Page, steps: number): Promise<number> {
  let stops = 0;
  for (let i = 0; i < steps; i += 1) {
    await page.keyboard.press("Tab");
    const landed = await focusIndicator(page);
    if (!landed) continue;
    stops += 1;
    // Polled rather than read once: the ring fades in over `--dur`, so the
    // first frame after focus is legitimately still transparent.
    await expect
      .poll(async () => isVisible((await focusIndicator(page)) ?? landed), {
        message:
          `no focus indicator on <${landed.tag}> "${landed.label}" ` +
          `(outline: ${landed.outlineWidth} ${landed.outlineStyle} ` +
          `${landed.outlineColor}, box-shadow: ${landed.boxShadow})`,
        timeout: 2000,
      })
      .toBe(true);
  }
  return stops;
}

const NAV: ReadonlyArray<readonly [string, string]> = [
  ["/", "Accueil"],
  ["/agenda", "Agenda"],
  ["/stocks", "Stocks"],
  ["/recipes", "Recettes"],
  ["/grocery-list", "Liste de courses"],
  ["/budget", "Budget"],
  ["/messagerie", "Messagerie"],
  ["/groups", "Groupes"],
];

test.describe("Interaction states (#69)", () => {
  test.beforeEach(async ({ page }) => {
    await registerAndLogin(page, "Interaction");
    await createGroup(page, "Foyer interaction");
  });

  test("the navigation says which page you are on, and only one", async ({ page }) => {
    for (const [path, label] of NAV) {
      await page.goto(path);
      const current = page.locator('[aria-current="page"]');
      await expect(current, `on ${path}`).toHaveCount(1);
      await expect(current, `on ${path}`).toHaveText(label);
    }

    // A page below a section keeps its section lit, so you never lose the
    // thread by opening a form.
    await page.goto("/agenda/new");
    await expect(page.locator('[aria-current="page"]')).toHaveText("Agenda");

    // "Mon compte" is a destination too.
    await page.goto("/account");
    await expect(page.locator('[aria-current="page"]')).toHaveText("Mon compte");

    // …and a page that is in no tab lights none of them rather than guessing.
    await page.goto("/privacy-policy");
    await expect(page.locator('[aria-current="page"]')).toHaveCount(0);
  });

  test("the current tab is told apart by its ground and its weight", async ({ page }) => {
    await page.goto("/agenda");
    const current = page.locator('.navlink[aria-current="page"]');
    const other = page.locator('.navlink[href="/stocks"]');
    const read = (locator: typeof current) =>
      locator.evaluate((node) => {
        const style = getComputedStyle(node);
        return {
          background: style.backgroundColor,
          weight: style.fontWeight,
          color: style.color,
          size: style.fontSize,
        };
      });

    const lit = await read(current);
    const dim = await read(other);
    expect(lit.background, "the active tab needs a ground of its own").not.toBe(dim.background);
    // WCAG 1.4.1: colour is never the only carrier.
    expect(Number(lit.weight)).toBeGreaterThan(Number(dim.weight));
    // Out of `.muted`: the navigation is not secondary content, so it is set
    // at the body size (16px), not the 0.875rem `.muted` imposed.
    expect(dim.size).toBe("16px");
  });

  test("every keyboard stop shows a focus ring, in both themes", async ({ page }) => {
    for (const colorScheme of ["light", "dark"] as const) {
      await page.emulateMedia({ colorScheme });
      for (const path of ["/", "/agenda", "/stocks/new", "/groups"]) {
        await page.goto(path);
        await page.locator("body").click({ position: { x: 1, y: 1 } });
        const stops = await walkWithTheKeyboard(page, 25);
        expect(stops, `no focusable element at all on ${path} (${colorScheme})`).toBeGreaterThan(3);
      }
    }
    await page.emulateMedia({ colorScheme: "light" });
  });

  test("the focus ring is drawn in the theme's own accent", async ({ page }) => {
    const ringOf = async (colorScheme: "light" | "dark") => {
      await page.emulateMedia({ colorScheme });
      await page.goto("/");
      await page.locator("body").click({ position: { x: 1, y: 1 } });
      await page.keyboard.press("Tab");
      const indicator = await focusIndicator(page);
      expect(indicator, `nothing focused in ${colorScheme}`).not.toBeNull();
      return indicator!.outlineColor;
    };
    // #74's lesson: a colour declared once serves the theme it was written
    // in and quietly fails the other. A ring is the one thing a keyboard
    // user cannot do without.
    const light = await ringOf("light");
    const dark = await ringOf("dark");
    expect(dark, "the focus ring keeps its light colour in the dark theme").not.toBe(light);
    await page.emulateMedia({ colorScheme: "light" });
  });

  test("the pointer gets an answer on a button, a row and a tab", async ({ page }) => {
    await page.goto("/groups");
    const background = (selector: string) =>
      page.locator(selector).first().evaluate((node) => getComputedStyle(node).backgroundColor);

    for (const selector of ['.navlink[href="/stocks"]', ".list-row", "form .btn, form button"]) {
      const before = await background(selector);
      await page.locator(selector).first().hover();
      await expect
        .poll(() => background(selector), {
          message: `${selector} does not react to the pointer`,
          timeout: 2000,
        })
        .not.toBe(before);
      // Park the pointer away again so the next element starts from rest.
      await page.mouse.move(0, 0);
    }
  });

  test("a visitor who asked for no motion gets none", async ({ page }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.goto("/groups");
    const durations = await page
      .locator("button")
      .first()
      .evaluate((node) => getComputedStyle(node).transitionDuration);
    expect(durations.split(",").map((d) => d.trim())).toEqual(["0s", "0s", "0s"]);
    await page.emulateMedia({ reducedMotion: "no-preference" });
  });
});
