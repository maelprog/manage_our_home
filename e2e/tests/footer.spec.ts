import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Issue #419 — the legal documents leave the login and register forms for a
// footer every page closes on, and the login page is reframed: its account
// links centred under the form, "Continuer avec Google" as wide and as tall
// as the form's own button. The Rust tests read the markup and the sheet;
// only a browser can say where the boxes land, and whether the fixed tab bar
// of a phone covers the footer.

const LEGAL = [
  { name: "Politique de confidentialité", href: "/privacy-policy" },
  { name: "Conditions générales d'utilisation", href: "/terms-of-service" },
  { name: "Mentions légales", href: "/legal-notice" },
];

const PASSWORD = "e2e-footer-password-1";

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

async function registerAndLogin(page: Page): Promise<void> {
  const email = uniqueEmail("footer");
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill("Footer Tester");
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

/** The footer landmark carries the three documents, each once on the page. */
async function expectLegalFooter(page: Page, path: string): Promise<void> {
  const footer = page.getByRole("contentinfo");
  await expect(footer, path).toHaveCount(1);
  for (const doc of LEGAL) {
    // Exactly once on the whole page, which is what keeps
    // `getByRole("link", { name })` unambiguous in strict mode.
    await expect(page.getByRole("link", { name: doc.name }), `${path}: ${doc.name}`).toHaveCount(1);
    await expect(footer.getByRole("link", { name: doc.name }), path).toHaveAttribute(
      "href",
      doc.href,
    );
  }
  // Not a navigation (DESIGN.md → Layout).
  await expect(footer.getByRole("navigation"), path).toHaveCount(0);
}

/**
 * Scrolls to the very end of the page and returns how much of the footer the
 * fixed tab bar covers (0 when there is no tab bar, or when it is a sidebar),
 * and whether the footer sits entirely inside the viewport.
 */
async function footerAtTheEnd(page: Page) {
  await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
  return page.evaluate(() => {
    const footer = document.querySelector("footer")!.getBoundingClientRect();
    const tabs = document.querySelector(".tabs");
    let covered = 0;
    if (tabs && getComputedStyle(tabs).position === "fixed") {
      const bar = tabs.getBoundingClientRect();
      covered = Math.max(0, footer.bottom - bar.top);
    }
    return {
      covered,
      inside: footer.top >= 0 && footer.bottom <= window.innerHeight + 0.5,
      height: footer.height,
    };
  });
}

for (const [layout, viewport] of [
  ["phone", { width: 390, height: 844 }],
  ["desktop", { width: 1280, height: 800 }],
] as const) {
  test.describe(`footer and login frame — ${layout}`, () => {
    test.use({ viewport });

    test("the login and register pages link the legal documents from the footer only", async ({
      page,
    }) => {
      for (const path of ["/login", "/register"]) {
        await page.goto(path);
        await expectLegalFooter(page, path);
      }
    });

    test("the Google button is the size of the form's own button", async ({ page }) => {
      for (const [path, submit] of [
        ["/login", "Se connecter"],
        ["/register", "Créer mon compte"],
      ] as const) {
        await page.goto(path);
        const read = (name: string, role: "link" | "button") =>
          page.getByRole(role, { name }).evaluate((node) => {
            const r = node.getBoundingClientRect();
            return {
              width: r.width,
              height: r.height,
              size: getComputedStyle(node).fontSize,
              secondary: node.classList.contains("secondary"),
            };
          });
        const google = await read("Continuer avec Google", "link");
        const primary = await read(submit, "button");
        expect(google.width, `${path} width`).toBeCloseTo(primary.width, 0);
        expect(google.height, `${path} height`).toBeCloseTo(primary.height, 0);
        expect(google.size, `${path} font size`).toBe(primary.size);
        // Still the secondary variant: the form's button stays the primary.
        expect(google.secondary, path).toBe(true);
      }
    });

    test("the account links are centred under the login form", async ({ page }) => {
      await page.goto("/login");
      const form = await page.locator('form[action="/login"]').boundingBox();
      expect(form).not.toBeNull();
      const formCentre = form!.x + form!.width / 2;
      for (const name of ["Créer un compte", "Mot de passe oublié ?"]) {
        const box = await page.getByRole("link", { name }).boundingBox();
        expect(box, name).not.toBeNull();
        // Either one centred line, or each link centred on its own: what
        // the rule says is that the row is centred, so the links as a group
        // are, and each lies wholly inside the form's width.
        expect(box!.x, name).toBeGreaterThan(form!.x + 1);
        expect(box!.x + box!.width, name).toBeLessThan(form!.x + form!.width - 1);
      }
      const group = await page.locator(".links.centered").first().evaluate((node) => {
        const links = [...node.querySelectorAll("a")].map((a) => a.getBoundingClientRect());
        const left = Math.min(...links.map((r) => r.left));
        const right = Math.max(...links.map((r) => r.right));
        return (left + right) / 2;
      });
      expect(group).toBeCloseTo(formCentre, 0);
    });

    test("a signed-in page closes on the footer, never under the tab bar", async ({ page }) => {
      await registerAndLogin(page);
      // A long page and a short one: the footer has to clear the tab bar
      // whether the page scrolls or not.
      for (const path of ["/account", "/groups/new"]) {
        await page.goto(path);
        await expectLegalFooter(page, path);
        const end = await footerAtTheEnd(page);
        expect(end.height, path).toBeGreaterThan(0);
        expect(end.covered, `${path}: px of footer under the tab bar`).toBe(0);
        expect(end.inside, `${path}: footer entirely in view at the end`).toBe(true);
      }
    });
  });
}
