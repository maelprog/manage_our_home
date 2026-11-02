import { expect, Locator, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Issue #674 — the filter shown (`aria-current="true"`) links to the page
// already open: a click on it only reloaded the page. It is now inert to the
// pointer, keeps its look and stays in the accessibility tree. A filter not
// shown, a lit toggle (« Stock bas ») and a nav tab (`aria-current="page"`)
// still navigate. The Rust test reads the rule off the sheet; only a browser
// says what a click on the real page does.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-current-filter-password-1";

/** Register + verify + login a fresh user, then give them a family. */
async function registerWithAFamily(page: Page): Promise<void> {
  const email = uniqueEmail("current");
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill("Courant");
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
  await page.goto("/groups/new");
  await page.getByLabel("Nom du groupe").fill("Foyer courant");
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
}

/** How many times the page's main frame navigates while `act` runs, plus a
 *  settling delay: a navigation a click starts begins well within it. */
async function navigationsDuring(page: Page, act: () => Promise<void>): Promise<number> {
  let count = 0;
  const onNavigated = (frame: unknown) => {
    if (frame === page.mainFrame()) count++;
  };
  page.on("framenavigated", onNavigated);
  await act();
  await page.waitForTimeout(750);
  page.off("framenavigated", onNavigated);
  return count;
}

/** A real pointer click at the centre of `link`, as a finger or a mouse
 *  lands: Playwright's own `click()` refuses an element that does not
 *  receive pointer events, which is the very point here. */
async function clickCentre(page: Page, link: Locator): Promise<void> {
  const box = await link.boundingBox();
  if (!box) throw new Error("the link is not laid out");
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
}

/** The current link is still a link, still current, and a click on it
 *  goes nowhere. */
async function expectInert(page: Page, link: Locator): Promise<void> {
  await expect(link).toHaveAttribute("aria-current", "true");
  await expect(link).toBeVisible();
  await expect(link).not.toHaveAttribute("aria-disabled");
  const before = page.url();
  expect(await navigationsDuring(page, () => clickCentre(page, link))).toBe(0);
  expect(page.url()).toBe(before);
}

test.describe("The current filter goes nowhere (#674)", () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test("recipes, stocks, notifications, the agenda's view and the kitchen segment", async ({
    page,
  }) => {
    await registerWithAFamily(page);

    // Recipes: « Toutes » shown is inert, « Prévues » still navigates.
    await page.goto("/recipes");
    const tabs = page.getByRole("navigation", { name: "Groupes de recettes" });
    await expectInert(page, tabs.getByRole("link", { name: "Toutes" }));
    expect(
      await navigationsDuring(page, () => clickCentre(page, tabs.getByRole("link", { name: /^Prévues/ }))),
    ).toBeGreaterThan(0);
    await expect(page).toHaveURL(/\/recipes\?vue=prevues$/);

    // The kitchen segment's current half, on the stocks.
    await page.goto("/stocks");
    const segment = page.getByRole("navigation", { name: "Cuisine" });
    await expect(segment.getByRole("link", { name: "Stocks" })).toHaveAttribute("aria-current", "page");
    expect(await navigationsDuring(page, () => clickCentre(page, segment.getByRole("link", { name: "Stocks" })))).toBe(0);

    // Stocks: « Tout » shown is inert; « Stock bas » lit is a toggle, and
    // a click turns it off.
    const filters = page.getByRole("navigation", { name: "Filtres" });
    await expectInert(page, filters.getByRole("link", { name: "Tout", exact: true }));
    await clickCentre(page, filters.getByRole("link", { name: "Stock bas" }));
    await expect(page).toHaveURL(/\/stocks\?low_stock=1$/);
    await expect(filters.getByRole("link", { name: "Stock bas" })).toHaveAttribute("aria-current", "true");
    await clickCentre(page, filters.getByRole("link", { name: "Stock bas" }));
    await expect(page).toHaveURL(/\/stocks$/);

    // Notifications: « Toutes » shown is inert.
    await page.goto("/notifications");
    await expectInert(
      page,
      page.getByRole("navigation", { name: "Filtres" }).getByRole("link", { name: "Toutes" }),
    );

    // The agenda: the view shown is inert, the other still switches.
    await page.goto("/agenda");
    const views = page.getByRole("navigation", { name: "Affichage" });
    await expectInert(page, views.getByRole("link", { name: "Mois", exact: true }));
    await clickCentre(page, views.getByRole("link", { name: "Semaine", exact: true }));
    await expect(page).toHaveURL(/\/agenda\?view=week&date=/);
  });
});

test.describe("A nav tab still leads back (#674)", () => {
  test.use({ viewport: { width: 1280, height: 800 } });

  test("« Recettes » from a recipe returns to the list", async ({ page }) => {
    await registerWithAFamily(page);
    await page.goto("/recipes/new");
    await page.getByLabel("Nom").fill("Gratin");
    await page.getByRole("button", { name: "Créer la recette" }).click();
    await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\?notice=recipe_created$/);

    const tab = page.locator('.navlink[href="/recipes"]').filter({ visible: true });
    await expect(tab).toHaveAttribute("aria-current", "page");
    await tab.click();
    await expect(page).toHaveURL("/recipes");
  });
});
