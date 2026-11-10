import { expect, Locator, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Issue #752 — « mettre les textes cliquables en gras plutôt que de les
// souligner, meilleure lecture ». A text link (a link without a class, and
// `.more`, `.replies`) is --accent and 600 with no underline at rest; the
// underline comes back under the pointer and the keyboard. A link with a box
// of its own keeps its look. The Rust guards in apps/web/src/app.rs read the
// rules; only a browser says which one wins on the element.

const ACCENT = {
  light: "rgb(20, 112, 110)", // --accent #14706e
  dark: "rgb(79, 179, 175)", // --accent #4fb3af
} as const;

const PASSWORD = "e2e-text-links-password-1";

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

async function registerAndLogin(page: Page): Promise<void> {
  const email = uniqueEmail("text-links");
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill("Liens Texte");
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

async function createGroup(page: Page, name: string): Promise<void> {
  await page.goto("/groups/new");
  await page.getByLabel("Nom du groupe").fill(name);
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
}

/** WCAG contrast of the link's text on the first opaque ground behind it. */
async function contrastOnItsGround(link: Locator): Promise<number> {
  return link.evaluate((node) => {
    const rgb = (value: string) => (value.match(/[\d.]+/g) ?? []).map(Number);
    const luminance = ([r, g, b]: number[]) => {
      const lin = (c: number) => {
        const s = c / 255;
        return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
      };
      return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
    };
    let ground: number[] | null = null;
    for (let el: Element | null = node; el && !ground; el = el.parentElement) {
      const bg = rgb(getComputedStyle(el).backgroundColor);
      if (bg.length === 3 || (bg.length === 4 && bg[3] === 1)) ground = bg.slice(0, 3);
    }
    const text = luminance(rgb(getComputedStyle(node).color));
    const back = luminance(ground ?? [255, 255, 255]);
    return (Math.max(text, back) + 0.05) / (Math.min(text, back) + 0.05);
  });
}

/** Tabs until `link` holds the focus — a keyboard focus, so `:focus-visible`. */
async function tabTo(page: Page, link: Locator): Promise<void> {
  for (let i = 0; i < 60; i += 1) {
    await page.keyboard.press("Tab");
    if (await link.evaluate((node) => node === document.activeElement)) return;
  }
  throw new Error("the keyboard never reached the link");
}

for (const theme of ["light", "dark"] as const) {
  test.describe(`text links — ${theme}`, () => {
    test.use({ colorScheme: theme });

    for (const [where, name] of [
      ["an account link under the login form", "Mot de passe oublié ?"],
      ["a legal link of the footer", "Mentions légales"],
    ] as const) {
      test(`${where} is bold in the accent, underlined only when pointed or focused`, async ({
        page,
      }) => {
        await page.goto("/login");
        const link = page.getByRole("link", { name, exact: true });

        await expect(link).toHaveCSS("font-weight", "600");
        await expect(link).toHaveCSS("color", ACCENT[theme]);
        await expect(link).toHaveCSS("text-decoration-line", "none");
        expect(await contrastOnItsGround(link)).toBeGreaterThanOrEqual(4.5);

        await link.hover();
        await expect(link).toHaveCSS("text-decoration-line", "underline");

        await page.mouse.move(0, 0);
        await expect(link).toHaveCSS("text-decoration-line", "none");
        await tabTo(page, link);
        await expect(link).toHaveCSS("text-decoration-line", "underline");
      });
    }

    test("a button-shaped link keeps its look, at rest and under the pointer", async ({ page }) => {
      await page.goto("/login");
      const google = page.getByRole("link", { name: "Continuer avec Google" });
      await expect(google).toHaveCSS("text-decoration-line", "none");
      await expect(google).toHaveCSS("font-weight", "600");
      await google.hover();
      await expect(google).toHaveCSS("text-decoration-line", "none");
    });
  });
}

test.describe("links with a box of their own — phone", () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test("the card title, the kitchen segment and the filters keep their look", async ({ page }) => {
    await registerAndLogin(page);
    await createGroup(page, "Famille Liens");

    // The card title: the heading's colour and its underline (#584).
    await page.goto("/");
    const title = page.locator(".dash h2 > a").first();
    const heading = page.locator(".dash h2").first();
    await expect(title).toHaveCSS("text-decoration-line", "underline");
    await expect(title).toHaveCSS(
      "color",
      await heading.evaluate((node) => getComputedStyle(node).color),
    );

    // The kitchen segment: only the current half is 600, none underlined.
    await page.goto("/stocks");
    const segment = page.getByRole("navigation", { name: "Cuisine" });
    const other = segment.getByRole("link", { name: "Recettes" });
    await expect(segment.getByRole("link", { name: "Stocks" })).toHaveCSS("font-weight", "600");
    await expect(other).toHaveCSS("font-weight", "400");
    await expect(other).toHaveCSS("text-decoration-line", "none");
    await other.hover();
    await expect(other).toHaveCSS("text-decoration-line", "none");

    // A filter pill: no underline under the pointer either. « Stock bas »,
    // a toggle, is never inert (`pointer-events: none`) as the shown filter is.
    const filter = page
      .getByRole("navigation", { name: "Filtres" })
      .getByRole("link", { name: "Stock bas" });
    await filter.hover();
    await expect(filter).toHaveCSS("text-decoration-line", "none");
  });
});
