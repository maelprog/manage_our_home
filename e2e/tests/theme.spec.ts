import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";
import { chooseTheme, resolvedTheme } from "../lib/theme";

// Issue #585 — the theme is chosen: Automatique, Clair or Sombre. The Rust
// tests check the cookie, the attribute and that the sheet states every dark
// token under both triggers; only a browser says which block wins once the
// device and the choice disagree.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-theme-password-1";

const LIGHT_BG = "#fbfaf7";
const DARK_BG = "#1a1917";

/** Register + verify + login a fresh user, then give them a family. */
async function registerWithAFamily(page: Page): Promise<void> {
  const email = uniqueEmail("theme");
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill("Thème");
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
  await page.getByLabel("Nom du groupe").fill("Foyer couleurs");
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
}

const PAGES = ["/", "/agenda"];

test.describe("Choosing the theme (#585)", () => {
  test.beforeEach(async ({ page }) => {
    await registerWithAFamily(page);
  });

  test("Sombre on a light device, then back to Automatique", async ({ page }) => {
    await page.emulateMedia({ colorScheme: "light" });
    await page.goto("/account/appearance");
    await expect(page.getByRole("radio", { name: "Automatique" })).toBeChecked();

    await chooseTheme(page, "Sombre");
    for (const path of PAGES) {
      await page.goto(path);
      expect(await resolvedTheme(page), path).toEqual({
        dataTheme: "dark",
        bg: DARK_BG,
        colorScheme: "dark",
      });
    }
    // The profile says the theme in force.
    await page.goto("/account");
    await expect(page.locator('a[href="/account/appearance"]')).toContainText("Sombre");

    await chooseTheme(page, "Automatique");
    for (const path of PAGES) {
      await page.goto(path);
      const theme = await resolvedTheme(page);
      expect(theme.dataTheme, path).toBeNull();
      expect(theme.bg, path).toBe(LIGHT_BG);
    }
  });

  test("Clair on a dark device, and the device's dark without a choice", async ({ page }) => {
    await page.emulateMedia({ colorScheme: "dark" });
    // No choice yet: the device decides, through the derived block.
    await page.goto("/");
    expect(await resolvedTheme(page)).toEqual({ dataTheme: null, bg: DARK_BG, colorScheme: "dark" });

    await chooseTheme(page, "Clair");
    for (const path of PAGES) {
      await page.goto(path);
      expect(await resolvedTheme(page), path).toEqual({
        dataTheme: "light",
        bg: LIGHT_BG,
        colorScheme: "light",
      });
    }

    await chooseTheme(page, "Automatique");
    await page.goto("/");
    expect(await resolvedTheme(page)).toEqual({ dataTheme: null, bg: DARK_BG, colorScheme: "dark" });
  });
});

// Issue #674 — the label rule stacks a field's content, and it put each
// theme's radio above its name. Every radio row sits on one line, the radio
// centred on its text, the whole row a ≥ 44 px target; on a phone and a PC.
for (const viewport of [
  { width: 390, height: 844 },
  { width: 1280, height: 800 },
]) {
  test.describe(`The theme's choices at ${viewport.width}px (#674)`, () => {
    test.use({ viewport });

    test("each radio sits beside its text, centred on it, in a row of 44px or more", async ({
      page,
    }) => {
      await registerWithAFamily(page);
      await page.goto("/account/appearance");
      const rows = page.locator('[role="radiogroup"] label');
      await expect(rows).toHaveCount(3);
      for (let i = 0; i < 3; i++) {
        const row = rows.nth(i);
        const radio = row.locator('input[type="radio"]');
        const text = row.locator(":scope > span");
        const r = await radio.boundingBox();
        const t = await text.boundingBox();
        const l = await row.boundingBox();
        if (!r || !t || !l) throw new Error(`row ${i} is not laid out`);
        const centre = r.y + r.height / 2;
        expect(centre, `radio ${i} is within its text`).toBeGreaterThanOrEqual(t.y);
        expect(centre, `radio ${i} is within its text`).toBeLessThanOrEqual(t.y + t.height);
        expect(Math.abs(centre - (t.y + t.height / 2)), `radio ${i} off its text's centre`).toBeLessThanOrEqual(2);
        expect(r.x + r.width, `radio ${i} is left of its text`).toBeLessThanOrEqual(t.x);
        expect(l.height, `row ${i} is a 44px target`).toBeGreaterThanOrEqual(44);
        // The radio takes the accent, as the rest of the controls do.
        expect(await radio.evaluate((el) => getComputedStyle(el).accentColor)).not.toBe("auto");
      }
      // The whole row is the target: a click on the text picks the radio.
      await rows.nth(2).locator("strong").click();
      await expect(page.getByRole("radio", { name: "Sombre" })).toBeChecked();
    });
  });
}
