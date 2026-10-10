import { expect, Page, test } from "@playwright/test";
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

/** One family with something on each page, so every component renders. */
async function seed(page: Page): Promise<void> {
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
  await expect(page.locator(".list-row", { hasText: "Bonjour" })).toBeVisible();

  await page.goto("/agenda/new");
  await page.getByLabel("Titre").fill("Rendez-vous");
  await page.getByLabel("Début").fill(todayAt(10));
  await page.getByLabel("Fin").fill(todayAt(11));
  await page.getByRole("button", { name: "Créer l'événement" }).click();
  await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);
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
 * agenda's arrows and chips, a disclosure's summary, a `.links` column.
 *
 * Left out, each for a stated reason:
 * - a bare text link (`<a>` with no class): inside a sentence it is WCAG
 *   2.5.5's inline exception, and the rest — a name in a list row, "Membres"
 *   on /groups — are not in the issue's list and are not closed by it;
 * - the calendar's chips on a wide screen: three to a 6.5rem day by design,
 *   a pointer layout (below the breakpoint they are 48px rows, checked);
 * - a checkbox with no label whose form submits the same change through a
 *   button (see below);
 * - the skip link, which is off-screen until it takes focus.
 */
async function undersized(page: Page): Promise<Box[]> {
  return page.evaluate((min) => {
    const out: { what: string; width: number; height: number }[] = [];
    const nodes = document.querySelectorAll<HTMLElement>(
      'button, .btn, input:not([type="hidden"]), select, textarea, summary, .navlink, .chip, .links a',
    );
    const wide = window.matchMedia("(min-width: 861px)").matches;
    for (const node of nodes) {
      if (node.closest(".skip-link")) continue;
      if (wide && node.matches(".chip")) continue;
      const style = getComputedStyle(node);
      if (style.display === "none" || style.visibility === "hidden") continue;
      const isBox = node.matches('input[type="checkbox"], input[type="radio"]');
      // A bare box whose own form has a submit button doing the same thing —
      // the grocery row's checkbox beside its "Cocher" / "Décocher": WCAG
      // 2.5.5's equivalent-control exception. The button is measured.
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
        for (const b of await undersized(page)) {
          failures.push(`${path}: ${b.what} ${b.width}×${b.height}`);
        }
      };
      for (const path of PUBLIC_PAGES) await visit(path);
      await seed(page);
      for (const path of APP_PAGES) await visit(path);
      expect(failures, failures.join("\n")).toEqual([]);
    });
  });
}
