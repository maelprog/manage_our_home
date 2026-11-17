import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";
import { fillIngredients } from "../lib/recipe-form";

// Issue #820 — the first questions about meals: on a member's first visit
// to the recipe list, three skippable steps (people at the table, allergies
// and their sharing per family, the kitchen's equipment), shown once per
// member. The Rust tests check each step's markup and the pure logic; this
// suite walks them end to end: everything skipped, everything answered,
// and put off for later.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-meal-start-password-1";

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
  await expect(page).toHaveURL(/\/groups(\/[0-9a-f-]+\/allergies\?from=created|\?notice=group_created)$/);
}

const step = (page: Page, n: number) => page.locator(".eyebrow", { hasText: `Étape ${n} sur 3` });

test.describe("First questions about meals (#820)", () => {
  test("skipping every step ends on the list, which does not ask again", async ({ page }) => {
    await registerAndLogin(page, "e2e-start-skip", "Camille Passe");
    await createGroup(page, "Foyer Passe");

    await page.goto("/recipes");
    await expect(page).toHaveURL("/recipes/depart");
    await expect(step(page, 1)).toBeVisible();
    await expect(page.getByRole("heading", { level: 1 })).toHaveText("Combien de personnes mangent d'habitude ?");
    // One member: one person proposed, and said so.
    await expect(page.getByLabel("Personnes à table")).toHaveValue("1");
    await expect(page.getByText("Vous êtes seul(e) dans « Foyer Passe »")).toBeVisible();

    await page.getByRole("link", { name: "Passer cette étape" }).click();
    await expect(step(page, 2)).toBeVisible();
    await expect(page.getByText("Vous n'avez déclaré aucune allergie ni aversion.")).toBeVisible();
    await expect(page.getByRole("radio")).toHaveCount(0);
    await page.getByRole("link", { name: "Passer cette étape" }).click();
    await expect(step(page, 3)).toBeVisible();
    await page.getByRole("link", { name: "Passer cette étape" }).click();

    await expect(page.getByRole("heading", { level: 1, name: "C'est prêt" })).toBeVisible();
    await expect(page.getByText("partiront du nombre de personnes de chacune")).toBeVisible();
    await page.getByRole("link", { name: "Voir mes recettes" }).click();
    await expect(page).toHaveURL("/recipes");
    await expect(page.getByRole("heading", { level: 1, name: "Recettes" })).toBeVisible();

    await page.reload();
    await expect(page.getByRole("heading", { level: 1, name: "Recettes" })).toBeVisible();
    await expect(page).toHaveURL("/recipes");
  });

  test("answering every step saves the people, the sharing and the equipment", async ({ page }) => {
    await registerAndLogin(page, "e2e-start-fill", "Camille Remplit");
    await createGroup(page, "Foyer Rempli");

    // An allergy to share, recorded where its consent is given.
    await page.goto("/account/allergies");
    await page.getByRole("checkbox", { name: /Je consens/ }).check();
    await page.getByRole("button", { name: "Continuer" }).click();
    await page.locator('select[name="tag"]').selectOption({ label: "Arachides" });
    await page.getByRole("button", { name: "Ajouter à ma liste" }).click();
    await expect(page.getByRole("status").first()).toContainText("Allergie ajoutée.");

    await page.goto("/recipes");
    await expect(page).toHaveURL("/recipes/depart");
    const people = page.getByLabel("Personnes à table");
    // Refused, and kept as typed.
    await people.fill("0");
    await people.evaluate((el) => el.removeAttribute("min"));
    await page.getByRole("button", { name: "Continuer" }).click();
    await expect(page.getByRole("alert")).toContainText("compris entre 1 et 50");
    await expect(people).toHaveValue("0");
    await people.fill("2");
    await page.getByRole("button", { name: "Continuer" }).click();

    await expect(step(page, 2)).toBeVisible();
    await expect(page.locator(".list-row", { hasText: "Arachides" })).toContainText("Allergie");
    const family = page.getByRole("group", { name: "Foyer Rempli", exact: true });
    await expect(family.getByRole("radio", { name: /^Privé/ })).toBeChecked();
    await family.getByRole("radio", { name: /^Public/ }).check();
    await page.getByRole("button", { name: "Continuer" }).click();

    await expect(step(page, 3)).toBeVisible();
    await page.locator("#equipements").getByRole("checkbox", { name: "Four" }).check();
    await page.getByRole("button", { name: "Terminer" }).click();

    await expect(page.getByRole("heading", { level: 1, name: "C'est prêt" })).toBeVisible();
    await expect(page.getByText("partiront de 2 personnes")).toBeVisible();

    // The level is the one in force on the account page…
    await page.goto("/account/allergies");
    await expect(page.locator("details", { hasText: "Arachides" }).locator("summary .badge")).toHaveText([
      "Foyer Rempli : public",
    ]);
    // …the oven the household's…
    await page.goto("/recipes/new");
    await page.getByRole("textbox", { name: "Nom" }).fill("Gratin pour deux");
    await page.getByLabel("Personnes").fill("6");
    await fillIngredients(page, "Pommes de terre | 1 | kg");
    await expect(
      page.getByRole("group", { name: "Équipements requis" }).getByRole("checkbox", { name: "Four" }),
    ).toBeVisible();
    await page.getByRole("button", { name: "Créer la recette" }).click();
    await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\?notice=recipe_created$/);
    // …and « À réaliser » starts from the two people, not the recipe's six.
    await expect(page.locator('form[action$="/select"] input[name="servings"]')).toHaveValue("2");
    await page.goto("/recipes");
    await expect(page).toHaveURL("/recipes");
    await expect(
      page.locator(".list-row", { hasText: "Gratin pour deux" }).locator('input[name="servings"]'),
    ).toHaveValue("2");
  });

  test("put off for later, the questions are not asked again; left half-way, they are", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-start-later", "Camille Plus Tard");
    await createGroup(page, "Foyer Tard");

    // Left after the first step: the list asks again, from the start.
    await page.goto("/recipes");
    await expect(page).toHaveURL("/recipes/depart");
    await page.getByRole("button", { name: "Continuer" }).click();
    await expect(step(page, 2)).toBeVisible();
    await page.goto("/");
    await page.goto("/recipes");
    await expect(page).toHaveURL("/recipes/depart");
    await expect(step(page, 1)).toBeVisible();

    await page.getByRole("link", { name: "Plus tard, passer toutes les questions" }).click();
    await expect(page).toHaveURL("/recipes");
    await expect(page.getByRole("heading", { level: 1, name: "Recettes" })).toBeVisible();
    await page.goto("/");
    await page.goto("/recipes");
    await expect(page.getByRole("heading", { level: 1, name: "Recettes" })).toBeVisible();
    await expect(page).toHaveURL("/recipes");

    // Where localStorage refuses, « Plus tard » holds for the session.
    const context = await browser.newContext({ storageState: await page.context().storageState() });
    await context.addInitScript(() => {
      Object.defineProperty(window, "localStorage", {
        get() {
          throw new DOMException("blocked", "SecurityError");
        },
      });
    });
    const blocked = await context.newPage();
    await blocked.goto("/recipes");
    await expect(blocked).toHaveURL("/recipes/depart");
    await blocked.getByRole("link", { name: "Plus tard, passer toutes les questions" }).click();
    await expect(blocked).toHaveURL("/recipes");
    await blocked.goto("/recipes");
    await expect(blocked.getByRole("heading", { level: 1, name: "Recettes" })).toBeVisible();
    await expect(blocked).toHaveURL("/recipes");
    await context.close();
  });
});
