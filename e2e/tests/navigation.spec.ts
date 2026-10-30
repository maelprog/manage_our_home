import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Issue #523: one navigation markup, laid out as six bottom tabs on a phone
// and as a sidebar on a PC. Every destination has to be reachable from it on
// both, by the name it shows there.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-navigation-password-1";

async function registerAndLogin(page: Page, prefix: string, displayName: string): Promise<void> {
  const email = uniqueEmail(prefix);
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

async function createGroup(page: Page, name: string): Promise<void> {
  await page.goto("/groups/new");
  await page.getByLabel("Nom du groupe").fill(name);
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
}

const nav = (page: Page) => page.getByRole("navigation", { name: "Navigation principale" });

/** Name shown → where it leads, in the order the navigation lists them. */
type Destinations = ReadonlyArray<readonly [string | RegExp, string]>;

const PHONE: Destinations = [
  ["Accueil", "/"],
  ["Agenda", "/agenda"],
  ["Courses", "/grocery-list"],
  ["Cuisine", "/stocks"],
  ["Messages", "/messagerie"],
  ["Profil", "/account"],
];

// The profile block is named by a visually hidden "Profil : " then the
// caller's name and the active family.
const PC: Destinations = [
  ["Accueil", "/"],
  ["Agenda", "/agenda"],
  ["Courses", "/grocery-list"],
  ["Stocks", "/stocks"],
  ["Recettes", "/recipes"],
  ["Budget", "/budget"],
  ["Messagerie", "/messagerie"],
  ["Notifications", "/notifications"],
  [/^Profil : Nav User Famille Navigation$/, "/account"],
];

for (const [layout, viewport, destinations] of [
  ["phone", { width: 390, height: 844 }, PHONE],
  ["PC", { width: 1440, height: 900 }, PC],
] as const) {
  test.describe(`navigation — ${layout}`, () => {
    test.use({ viewport });

    test("every destination is reachable from the navigation, and lit once there", async ({
      page,
    }) => {
      await registerAndLogin(page, "e2e-nav", "Nav User");
      await createGroup(page, "Famille Navigation");
      await page.goto("/");

      // Exactly these links are drawn: a hidden one is out of the
      // accessibility tree, which `getByRole` reads.
      await expect(nav(page).getByRole("link")).toHaveCount(destinations.length);
      for (const [name, href] of destinations) {
        const link = nav(page).getByRole("link", { name, exact: true });
        await link.click();
        await expect(page, `${name}`).toHaveURL(href);
        // Counted among the drawn links: on the stocks and the recipes, the
        // phone's Cuisine tab and the PC's Stocks or Recettes link are both
        // current, and each layout hides the other's.
        const current = page.locator('header [aria-current="page"]').filter({ visible: true });
        await expect(current, `${name}`).toHaveCount(1);
        await expect(current, `${name}`).toHaveAttribute("href", href);
      }
    });
  });
}

test.describe("navigation — phone only", () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test("the six tabs fit across the phone, none past its edge", async ({ page }) => {
    await registerAndLogin(page, "e2e-nav-fit", "Nav Fit");
    await createGroup(page, "Famille Largeur");
    await page.goto("/");

    const bar = nav(page);
    const overflow = await bar.evaluate((node) => node.scrollWidth - node.clientWidth);
    expect(overflow).toBeLessThanOrEqual(0);
    for (const link of await bar.getByRole("link").all()) {
      await expect(link).toBeInViewport({ ratio: 1 });
    }
  });

  test("Cuisine opens the stocks, and its segment the recipes, with Cuisine still lit", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-nav-cuisine", "Nav Cuisine");
    await createGroup(page, "Famille Cuisine");
    await page.goto("/");

    await nav(page).getByRole("link", { name: "Cuisine" }).click();
    await expect(page).toHaveURL("/stocks");
    const segment = page.getByRole("navigation", { name: "Cuisine" });
    await expect(segment.getByRole("link", { name: "Stocks" })).toHaveAttribute("aria-current", "page");

    await segment.getByRole("link", { name: "Recettes" }).click();
    await expect(page).toHaveURL("/recipes");
    await expect(segment.getByRole("link", { name: "Recettes" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    // Recettes has no tab of its own here: the Cuisine tab is the current
    // one on the recipes too, and wears the current tab's ground and weight.
    const cuisine = nav(page).getByRole("link", { name: "Cuisine" });
    await expect(cuisine).toHaveAttribute("aria-current", "page");
    await expect(page.locator('header [aria-current="page"]').filter({ visible: true })).toHaveCount(1);
    const lit = await cuisine.evaluate((node) => {
      const style = getComputedStyle(node);
      return { weight: style.fontWeight, background: style.backgroundColor };
    });
    const other = await nav(page)
      .getByRole("link", { name: "Agenda" })
      .evaluate((node) => getComputedStyle(node).backgroundColor);
    expect(lit.weight).toBe("600");
    expect(lit.background).not.toBe(other);
  });
});

test.describe("navigation — PC only", () => {
  test.use({ viewport: { width: 1440, height: 900 } });

  test("the sidebar has no kitchen segment, and its foot is the profile", async ({ page }) => {
    await registerAndLogin(page, "e2e-nav-pc", "Nav Pc");
    await createGroup(page, "Famille Bureau");

    await page.goto("/stocks");
    await expect(page.getByRole("navigation", { name: "Cuisine" })).toHaveCount(0);

    // The profile block sits at the bottom of the viewport-high sidebar.
    const profile = nav(page).getByRole("link", { name: /^Profil/ });
    await expect(profile).toContainText("Famille Bureau");
    const box = await profile.boundingBox();
    if (!box) throw new Error("profile block not rendered");
    expect(box.y + box.height).toBeGreaterThan(900 - 80);
  });
});
