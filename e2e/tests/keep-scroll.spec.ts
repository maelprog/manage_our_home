import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Issue #456: a full-page navigation must not lose the reader's place.
// - The phone tab bar scrolls sideways; after following a tab, the tab of
//   the page now shown is brought into the bar (`enhance.js`, `.tabs`).
// - Ticking a grocery item posts its form and redirects back to the list;
//   `data-keep-scroll` on that form puts the page back where it was.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-keep-scroll-password-1";

async function registerAndLogin(page: Page, prefix: string, displayName: string): Promise<void> {
  const email = uniqueEmail(prefix);
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

async function createGroup(page: Page, name: string): Promise<void> {
  await page.goto("/groups/new");
  await page.getByLabel("Nom du groupe").fill(name);
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
}

test.describe("keep scroll — phone tab bar", () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test("following the Messagerie tab leaves that tab entirely visible in the bar", async ({ page }) => {
    await registerAndLogin(page, "e2e-tabscroll", "Tab Scroll");
    await createGroup(page, "Famille Défilement");

    await page.goto("/");
    await page.locator(".tabs").getByRole("link", { name: "Messagerie" }).click();
    await expect(page).toHaveURL(/\/messagerie$/);

    const bar = page.locator("nav.tabs");
    // The bar has to overflow for the test to mean anything.
    const overflow = await bar.evaluate((nav) => nav.scrollWidth - nav.clientWidth);
    expect(overflow).toBeGreaterThan(0);

    const tab = bar.locator('[aria-current="page"]');
    await expect(tab).toHaveText("Messagerie");
    const barBox = await bar.boundingBox();
    const tabBox = await tab.boundingBox();
    if (!barBox || !tabBox) throw new Error("tab bar or active tab not rendered");
    expect(tabBox.x).toBeGreaterThanOrEqual(barBox.x);
    expect(tabBox.x + tabBox.width).toBeLessThanOrEqual(barBox.x + barBox.width);
  });
});

test.describe("keep scroll — grocery list check-off", () => {
  test("ticking the 25th of 30 items leaves the page where it was, within one row", async ({ page }) => {
    await registerAndLogin(page, "e2e-checkscroll", "Check Scroll");
    await createGroup(page, "Famille Coche");

    // Zero-padded so the list's `checked, name` order is the numeric one.
    for (let n = 1; n <= 30; n++) {
      const name = `Article ${String(n).padStart(2, "0")}`;
      const response = await page.request.post("/grocery-list/add", {
        form: { name, quantity: "", unit: "" },
      });
      expect(response.ok()).toBe(true);
    }

    await page.goto("/grocery-list");
    const box = page.getByRole("checkbox", { name: "Article 25", exact: true });
    await box.scrollIntoViewIfNeeded();
    const row = page.locator("li.list-row", { has: box });
    const rowHeight = (await row.boundingBox())?.height ?? 0;
    expect(rowHeight).toBeGreaterThan(0);
    const before = await page.evaluate(() => window.scrollY);
    // Scrolled at all, or the comparison below proves nothing.
    expect(before).toBeGreaterThan(rowHeight);

    await box.click();
    await page.waitForURL(/\/grocery-list\?notice=item_checked$/);
    await expect(page.getByRole("checkbox", { name: "Article 25", exact: true })).toBeChecked();

    const after = await page.evaluate(() => window.scrollY);
    expect(Math.abs(after - before)).toBeLessThanOrEqual(rowHeight);
  });
});
