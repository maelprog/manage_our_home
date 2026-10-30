import { Browser, expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";
import { chooseTheme } from "../lib/theme";

// Issue #538 — the family's colour. The Rust tests check the attribute, the
// rules and every contrast; only a browser says that `data-accent` wins over
// the dark blocks on <html>, and that a family's colour follows its members
// from page to page and stops at another family's.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-colour-password-1";

const OCEAN_LIGHT = "#1f5f99";
const OCEAN_DARK = "#67a6e0";
const SARCELLE_LIGHT = "#14706e";

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

async function openSettings(page: Page, family: string): Promise<void> {
  await page.goto("/groups");
  await page.locator("li", { hasText: family }).getByRole("link", { name: "Paramètres" }).click();
  await expect(page.getByRole("heading", { level: 1, name: family })).toBeVisible();
}

async function secondUser(browser: Browser, prefix: string, displayName: string): Promise<Page> {
  const context = await browser.newContext();
  const page = await context.newPage();
  await registerAndLogin(page, prefix, displayName);
  return page;
}

/** `data-accent` and the resolved `--accent` of <html>. */
async function resolvedAccent(page: Page): Promise<{ dataAccent: string | null; accent: string }> {
  return page.evaluate(() => {
    const root = document.documentElement;
    return {
      dataAccent: root.getAttribute("data-accent"),
      accent: getComputedStyle(root).getPropertyValue("--accent").trim(),
    };
  });
}

const PAGES = ["/", "/agenda", "/groups", "/account", "/messagerie"];

test.describe("The family's colour (#538)", () => {
  test("an admin picks Océan: every page of that family wears it, another family's does not", async ({
    page,
    browser,
  }) => {
    await page.emulateMedia({ colorScheme: "light" });
    await registerAndLogin(page, "e2e-colour-owner", "Couleur Owner");
    await createGroup(page, "Foyer Bleu");
    await page.goto("/groups");
    await page.locator("li", { hasText: "Foyer Bleu" }).getByRole("link", { name: "Membres" }).click();
    await page.getByRole("button", { name: "Créer une invitation" }).click();
    const invite = await page.locator(".notice.success a").getAttribute("href");
    if (!invite) throw new Error("no invitation link rendered");

    const member = await secondUser(browser, "e2e-colour-member", "Couleur Member");
    await member.emulateMedia({ colorScheme: "light" });
    await member.goto(invite);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    // Standard: the colour and who may change it, no control.
    await openSettings(member, "Foyer Bleu");
    const card = member.locator("#couleur");
    await expect(card.getByRole("heading", { name: "Couleur de la famille" })).toBeVisible();
    await expect(card.getByText("Réservé aux propriétaires et admins.")).toBeVisible();
    await expect(card.getByRole("radio")).toHaveCount(0);
    expect(await resolvedAccent(member)).toEqual({ dataAccent: "sarcelle", accent: SARCELLE_LIGHT });

    // The owner makes them admin.
    await page.goto("/groups");
    await page.locator("li", { hasText: "Foyer Bleu" }).getByRole("link", { name: "Membres" }).click();
    const row = page.locator("li", { hasText: "Couleur Member" });
    await row.locator("select[name=role]").selectOption("admin");
    await row.getByRole("button", { name: "Changer le rôle" }).click();
    await expect(page.getByText("Rôle mis à jour.")).toBeVisible();

    // The admin picks Océan among the eight.
    await openSettings(member, "Foyer Bleu");
    await expect(card.getByRole("radio")).toHaveCount(8);
    await expect(card.getByRole("radio", { name: "Sarcelle" })).toBeChecked();
    await card.getByRole("radio", { name: "Océan" }).check();
    await card.getByRole("button", { name: "Enregistrer la couleur" }).click();
    await expect(member).toHaveURL(/\/settings\?notice=accent_changed$/);
    await expect(member.getByText("Couleur de la famille enregistrée.")).toBeVisible();
    await expect(card.getByRole("radio", { name: "Océan" })).toBeChecked();

    // Every page of the family, for both its members.
    for (const who of [member, page]) {
      for (const path of PAGES) {
        await who.goto(path);
        expect(await resolvedAccent(who), path).toEqual({ dataAccent: "ocean", accent: OCEAN_LIGHT });
      }
    }

    // Dark, from the device and from the choice: the family's colour wins
    // over the dark theme's own accent.
    await member.emulateMedia({ colorScheme: "dark" });
    await member.goto("/agenda");
    expect(await resolvedAccent(member)).toEqual({ dataAccent: "ocean", accent: OCEAN_DARK });
    await member.emulateMedia({ colorScheme: "light" });
    await chooseTheme(member, "Sombre");
    await member.goto("/");
    expect(await resolvedAccent(member)).toEqual({ dataAccent: "ocean", accent: OCEAN_DARK });
    await chooseTheme(member, "Automatique");

    // Another family of the same member: not Océan.
    await createGroup(member, "Foyer Vert");
    for (const path of PAGES) {
      await member.goto(path);
      expect(await resolvedAccent(member), path).toEqual({
        dataAccent: "sarcelle",
        accent: SARCELLE_LIGHT,
      });
    }

    await member.context().close();
  });
});
