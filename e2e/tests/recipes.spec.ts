import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Front epic F5 — Recipes (issue #20): every user journey the epic
// introduces, happy paths plus the documented error states from
// apps/api/src/recipes/'s error tables (see docs/front-epic-5-recipes.md).
// Permission bar mirrors Stocks: any member may create/read a recipe or log a
// meal, so every member sees the log-a-meal form; the edit link and delete
// button stay behind can_modify (creator/admin/owner). The suggestion view
// renders the ranked order + derived signals (stock summary, missing
// ingredients), never the raw internal score.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-recipes-password-1";

/** Register + verify + login a fresh user on the given page. */
async function registerAndLogin(page: Page, prefix: string, displayName: string): Promise<string> {
  const email = uniqueEmail(prefix);
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
  return email;
}

async function createGroup(page: Page, name: string): Promise<void> {
  await page.goto("/groups/new");
  await page.getByLabel("Nom du groupe").fill(name);
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
}

/** Creates an invitation link for `groupName` (caller must be owner/admin). */
async function createInvitationLink(page: Page, groupName: string): Promise<string> {
  await page.goto("/groups");
  await page.locator("li", { hasText: groupName }).getByRole("link", { name: "Membres" }).click();
  await page.getByRole("button", { name: "Créer une invitation" }).click();
  const href = await page.locator(".notice.success a").getAttribute("href");
  if (!href) throw new Error("no invitation link");
  return href;
}

/** Adds a stock item so the suggestion scorer can match it by name. */
async function createStockItem(page: Page, name: string, quantity: string, unit: string): Promise<void> {
  await page.goto("/stocks/new");
  await page.getByLabel("Nom").fill(name);
  await page.getByLabel("Quantité").fill(quantity);
  await page.getByLabel("Unité").fill(unit);
  await page.getByRole("button", { name: "Ajouter l'article" }).click();
  await expect(page).toHaveURL(/\/stocks\?notice=item_created$/);
}

interface RecipeOpts {
  name: string;
  instructions?: string;
  ingredients?: string;
}

/** Fill and submit /recipes/new; asserts the redirect to the new detail. */
async function createRecipe(page: Page, opts: RecipeOpts): Promise<void> {
  await page.goto("/recipes/new");
  await page.getByLabel("Nom").fill(opts.name);
  if (opts.instructions) await page.getByLabel("Instructions").fill(opts.instructions);
  if (opts.ingredients) await page.getByLabel("Ingrédients").fill(opts.ingredients);
  await page.getByRole("button", { name: "Créer la recette" }).click();
  await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\?notice=recipe_created$/);
  await expect(page.getByText("Recette créée.")).toBeVisible();
}

/** Opens a recipe's detail by clicking its name in the full list. */
async function openRecipeDetail(page: Page, name: string): Promise<void> {
  await page.goto("/recipes");
  await page.getByRole("heading", { name: "Toutes les recettes" }).scrollIntoViewIfNeeded();
  await page
    .locator("ul")
    .last()
    .getByRole("link", { name, exact: true })
    .click();
  await expect(page.getByRole("heading", { name, level: 1 })).toBeVisible();
}

test.describe("Recipes — create & detail", () => {
  test("create a recipe with a seasonal + an optional ingredient; detail shows the markers", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-rccreate", "Recipe Creator");
    await createGroup(page, "Famille Recette");

    await createRecipe(page, {
      name: "Tarte aux pommes",
      instructions: "Étaler la pâte, garnir, enfourner.",
      ingredients: "Pâte | 1 | rouleau\nPomme | 6 | pièce | 9,10,11\nCannelle |  |  |  | optionnel",
    });

    // Redirected to the detail; markers rendered.
    await expect(page.getByRole("heading", { name: "Tarte aux pommes", level: 1 })).toBeVisible();
    await expect(page.getByText("Étaler la pâte, garnir, enfourner.")).toBeVisible();
    await expect(page.getByText("saison : 9, 10, 11")).toBeVisible();
    await expect(page.getByText("· optionnel")).toBeVisible();
    // Not yet cooked.
    await expect(page.getByText("Ce plat n'a pas encore été cuisiné.")).toBeVisible();
  });

  test("empty name is rejected before an API round-trip", async ({ page }) => {
    await registerAndLogin(page, "e2e-rcvalid", "Validation User");
    await createGroup(page, "Famille Validation Recette");
    await page.goto("/recipes/new");
    // Bypass the browser's `required` so the shared server-side validation is
    // what rejects it.
    await page.getByLabel("Nom").evaluate((el) => el.removeAttribute("required"));
    await page.getByLabel("Ingrédients").fill("Farine | 1 | kg");
    await page.getByRole("button", { name: "Créer la recette" }).click();
    await expect(page.getByText("Le nom est obligatoire.")).toBeVisible();
  });

  test("unknown recipe id shows the not-found page", async ({ page }) => {
    await registerAndLogin(page, "e2e-rc404", "Recipe 404");
    await createGroup(page, "Famille 404 Recette");
    await page.goto("/recipes/00000000-0000-4000-8000-000000000000");
    await expect(page.getByRole("heading", { name: "Recette introuvable" })).toBeVisible();
  });
});

test.describe("Recipes — suggestions", () => {
  test("suggestions show the stock summary and missing ingredients", async ({ page }) => {
    await registerAndLogin(page, "e2e-rcsugg", "Suggestion User");
    await createGroup(page, "Famille Suggestions");

    // One ingredient is in stock, one recipe needs an out-of-stock one.
    await createStockItem(page, "Farine", "2", "kg");
    await createRecipe(page, { name: "Pain maison", ingredients: "Farine | 2 | kg" });
    await createRecipe(page, { name: "Ratatouille", ingredients: "Aubergine | 2 | pièce" });

    await page.goto("/recipes");
    await expect(page.getByRole("heading", { name: "Suggestions" })).toBeVisible();

    // Pain: fully stocked.
    await expect(
      page.locator("li", { hasText: "Pain maison" }).getByText("Tous les ingrédients en stock"),
    ).toBeVisible();

    // Ratatouille: missing Aubergine, listed under the grocery-list marker.
    const ratatouille = page.locator("li", { hasText: "Ratatouille" }).first();
    await expect(ratatouille.getByText("0/1 ingrédients en stock")).toBeVisible();
    await expect(ratatouille.getByText("à ajouter à la liste de courses")).toBeVisible();
    await expect(ratatouille.getByText("Aubergine")).toBeVisible();
  });
});

test.describe("Recipes — log a meal", () => {
  test("logging a meal records it and the detail reflects the last-cooked state", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-rclog", "Log User");
    await createGroup(page, "Famille Repas");
    await createRecipe(page, { name: "Curry", ingredients: "Riz | 1 | tasse" });

    // We're on the detail after create. Log a meal for today.
    await page.getByRole("button", { name: "Logger ce repas" }).click();
    await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\?notice=meal_logged$/);
    await expect(page.getByText("Repas enregistré.")).toBeVisible();
    await expect(page.getByText(/Dernier repas cuisiné : le/)).toBeVisible();
  });
});

test.describe("Recipes — edit & delete", () => {
  test("full edit round-trips through the ingredient textarea", async ({ page }) => {
    await registerAndLogin(page, "e2e-rcedit", "Edit User");
    await createGroup(page, "Famille Édition Recette");
    await createRecipe(page, { name: "Soupe", ingredients: "Carotte | 3 | pièce" });

    await openRecipeDetail(page, "Soupe");
    await page.getByRole("link", { name: "Modifier la recette" }).click();
    // The textarea is pre-filled from the stored ingredients (round-trip).
    await expect(page.getByLabel("Ingrédients")).toHaveValue("Carotte | 3 | pièce");

    await page.getByLabel("Nom").fill("Soupe de légumes");
    await page.getByLabel("Ingrédients").fill("Carotte | 3 | pièce\nPoireau | 2 | pièce");
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page.getByText("Recette mise à jour.")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Soupe de légumes", level: 1 })).toBeVisible();
    await expect(page.getByText("Poireau")).toBeVisible();
  });

  test("delete removes the recipe from the list", async ({ page }) => {
    await registerAndLogin(page, "e2e-rcdel", "Delete User");
    await createGroup(page, "Famille Suppression Recette");
    await createRecipe(page, { name: "À supprimer", ingredients: "Eau | 1 | L" });

    await openRecipeDetail(page, "À supprimer");
    await page.getByRole("button", { name: "Supprimer" }).click();
    await expect(page).toHaveURL(/\/recipes\?notice=recipe_deleted$/);
    await expect(page.getByText("Recette supprimée.")).toBeVisible();
    await page.goto("/recipes");
    await expect(page.getByRole("link", { name: "À supprimer", exact: true })).toHaveCount(0);
  });
});

test.describe("Recipes — permission bar", () => {
  test("a standard member cannot edit or delete another member's recipe but can log a meal", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-rcpermowner", "Perm Owner");
    await createGroup(page, "Famille Droits Recette");
    const href = await createInvitationLink(page, "Famille Droits Recette");

    // Owner creates the recipe.
    await createRecipe(page, { name: "Gratin du proprio", ingredients: "Pomme de terre | 4 | pièce" });

    const context = await browser.newContext();
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-rcmember", "Perm Member");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    // The member sees the log form but no edit/delete controls.
    await openRecipeDetail(member, "Gratin du proprio");
    await expect(
      member.getByText("Seul le créateur ou un administrateur peut modifier ou supprimer"),
    ).toBeVisible();
    await expect(member.getByRole("link", { name: "Modifier la recette" })).toHaveCount(0);
    await expect(member.getByRole("button", { name: "Supprimer" })).toHaveCount(0);

    // ...and can actually log a meal against the owner's recipe → 201.
    await member.getByRole("button", { name: "Logger ce repas" }).click();
    await expect(member.getByText("Repas enregistré.")).toBeVisible();
    await expect(member.getByText(/Dernier repas cuisiné : le/)).toBeVisible();
    await context.close();
  });
});
