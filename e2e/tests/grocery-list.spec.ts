import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Front epic F6 — Grocery list (issue #21): every user journey the epic
// introduces, happy paths plus the documented error states from
// apps/api/src/grocery_list/'s error tables (see
// docs/front-epic-6-grocery-list.md). One shared list per family; any member
// may add/read/check off, so every member sees the check control; the per-row
// "Modifier" link and the edit/delete screen stay behind can_modify
// (creator/admin/owner). Generate is idempotent — a re-run adds nothing.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-grocery-password-1";

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

/** Adds a stock item; a non-empty threshold makes it low-stock at/below it. */
async function createStockItem(
  page: Page,
  name: string,
  quantity: string,
  unit: string,
  threshold?: string,
): Promise<void> {
  await page.goto("/stocks/new");
  await page.getByLabel("Nom").fill(name);
  await page.getByLabel("Quantité").fill(quantity);
  await page.getByLabel("Unité").fill(unit);
  if (threshold !== undefined) await page.getByLabel("Seuil de réappro").fill(threshold);
  await page.getByRole("button", { name: "Ajouter l'article" }).click();
  await expect(page).toHaveURL(/\/stocks\?notice=item_created$/);
}

/** Creates a recipe with a single required ingredient line. */
async function createRecipe(page: Page, name: string, ingredients: string): Promise<void> {
  await page.goto("/recipes/new");
  await page.getByLabel("Nom").fill(name);
  await page.getByLabel("Ingrédients").fill(ingredients);
  await page.getByRole("button", { name: "Créer la recette" }).click();
  await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\?notice=recipe_created$/);
}

/** Adds a manual item via the inline form on /grocery-list. */
async function addItem(page: Page, name: string, quantity?: string, unit?: string): Promise<void> {
  await page.goto("/grocery-list");
  await page.getByLabel("Article").fill(name);
  if (quantity !== undefined) await page.getByLabel("Quantité").fill(quantity);
  if (unit !== undefined) await page.getByLabel("Unité").fill(unit);
  await page.getByRole("button", { name: "Ajouter" }).click();
  await expect(page).toHaveURL(/\/grocery-list\?notice=item_added$/);
}

test.describe("Grocery list — manual add", () => {
  test("adds an item inline and it appears on the shared list", async ({ page }) => {
    await registerAndLogin(page, "e2e-glnew", "Grocery Adder");
    await createGroup(page, "Famille Courses");

    await addItem(page, "Lait", "2", "L");
    await expect(page.getByText("Article ajouté.")).toBeVisible();

    const row = page.locator("li", { hasText: "Lait" });
    await expect(row.getByText("Lait")).toBeVisible();
    await expect(row.getByText("2 L")).toBeVisible();
  });

  test("server-side empty-name rejection shows the error banner", async ({ page }) => {
    await registerAndLogin(page, "e2e-glvalid", "Grocery Validation");
    await createGroup(page, "Famille Courses Validation");
    await page.goto("/grocery-list");
    // Bypass the browser's `required` so the shared server-side validation is
    // what rejects it.
    await page.getByLabel("Article").evaluate((el) => el.removeAttribute("required"));
    await page.getByRole("button", { name: "Ajouter" }).click();
    await expect(page).toHaveURL(/\/grocery-list\?error=name_required$/);
    await expect(page.getByText("Le nom est obligatoire.")).toBeVisible();
  });

  test("unknown item id shows the not-found page", async ({ page }) => {
    await registerAndLogin(page, "e2e-gl404", "Grocery 404");
    await createGroup(page, "Famille Courses 404");
    await page.goto("/grocery-list/00000000-0000-4000-8000-000000000000");
    await expect(page.getByRole("heading", { name: "Article introuvable" })).toBeVisible();
  });
});

test.describe("Grocery list — generate", () => {
  test("pulls missing recipe ingredients + low-stock items, and is idempotent on re-run", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-glgen", "Grocery Generator");
    await createGroup(page, "Famille Génération");

    // A low-stock item (qty 0, threshold 1) → 'low_stock' source.
    await createStockItem(page, "Beurre", "0", "kg", "1");
    // A recipe whose required ingredient isn't in stock → 'recipe' source.
    await createRecipe(page, "Gâteau", "Chocolat | 200 | g");

    await page.goto("/grocery-list");
    await page.getByRole("button", { name: "Générer depuis les recettes et les stocks" }).click();
    await expect(page).toHaveURL(/\/grocery-list\?notice=generated&added=2$/);
    await expect(page.getByText("2 articles ajoutés depuis les recettes et les stocks.")).toBeVisible();

    // Both sources present with their provenance badge.
    const chocolat = page.locator("li", { hasText: "Chocolat" });
    await expect(chocolat.getByText("Recette")).toBeVisible();
    const beurre = page.locator("li", { hasText: "Beurre" });
    await expect(beurre.getByText("Stock bas")).toBeVisible();

    // Re-run: nothing new qualifies (idempotent).
    await page.getByRole("button", { name: "Générer depuis les recettes et les stocks" }).click();
    await expect(page).toHaveURL(/\/grocery-list\?notice=generated&added=0$/);
    await expect(page.getByText("Aucun nouvel article à ajouter.")).toBeVisible();
    await expect(page.locator("li", { hasText: "Chocolat" })).toHaveCount(1);
  });
});

test.describe("Grocery list — check off", () => {
  test("checking an item marks it done; unchecking restores it", async ({ page }) => {
    await registerAndLogin(page, "e2e-glcheck", "Grocery Checker");
    await createGroup(page, "Famille Cases");
    await addItem(page, "Pain");

    await page.locator("li", { hasText: "Pain" }).getByRole("button", { name: "Cocher" }).click();
    await expect(page).toHaveURL(/\/grocery-list\?notice=item_checked$/);
    await expect(page.getByText("Article coché.")).toBeVisible();
    // Now checked: the toggle reads "Décocher".
    await expect(
      page.locator("li", { hasText: "Pain" }).getByRole("button", { name: "Décocher" }),
    ).toBeVisible();

    await page.locator("li", { hasText: "Pain" }).getByRole("button", { name: "Décocher" }).click();
    await expect(page).toHaveURL(/\/grocery-list\?notice=item_unchecked$/);
    await expect(
      page.locator("li", { hasText: "Pain" }).getByRole("button", { name: "Cocher" }),
    ).toBeVisible();
  });
});

test.describe("Grocery list — edit & delete", () => {
  test("edit round-trips through the item form", async ({ page }) => {
    await registerAndLogin(page, "e2e-gledit", "Grocery Editor");
    await createGroup(page, "Famille Édition Courses");
    await addItem(page, "Pommes", "3", "kg");

    await page.goto("/grocery-list");
    await page.locator("li", { hasText: "Pommes" }).getByRole("link", { name: "Modifier" }).click();
    // Pre-filled from the stored item.
    await expect(page.getByLabel("Nom")).toHaveValue("Pommes");
    await expect(page.getByLabel("Quantité")).toHaveValue("3");
    await expect(page.getByLabel("Unité")).toHaveValue("kg");

    await page.getByLabel("Nom").fill("Pommes rouges");
    await page.getByLabel("Quantité").fill("5");
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page).toHaveURL(/\/grocery-list\?notice=item_updated$/);
    await expect(page.getByText("Article mis à jour.")).toBeVisible();
    await expect(page.locator("li", { hasText: "Pommes rouges" }).getByText("5 kg")).toBeVisible();
  });

  test("delete removes the item from the list", async ({ page }) => {
    await registerAndLogin(page, "e2e-gldel", "Grocery Deleter");
    await createGroup(page, "Famille Suppression Courses");
    await addItem(page, "À supprimer");

    await page.goto("/grocery-list");
    await page.locator("li", { hasText: "À supprimer" }).getByRole("link", { name: "Modifier" }).click();
    await page.getByRole("button", { name: "Supprimer" }).click();
    await expect(page).toHaveURL(/\/grocery-list\?notice=item_deleted$/);
    await expect(page.getByText("Article supprimé.")).toBeVisible();
    await expect(page.locator("li", { hasText: "À supprimer" })).toHaveCount(0);
  });
});

test.describe("Grocery list — permission bar", () => {
  test("a standard member can check but not edit/delete another member's item", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-glpermowner", "Perm Owner");
    await createGroup(page, "Famille Droits Courses");
    const href = await createInvitationLink(page, "Famille Droits Courses");

    // Owner adds the item and captures its edit URL.
    await addItem(page, "Sel");
    await page.goto("/grocery-list");
    const editHref = await page
      .locator("li", { hasText: "Sel" })
      .getByRole("link", { name: "Modifier" })
      .getAttribute("href");
    if (!editHref) throw new Error("no edit link for the owner's item");

    const context = await browser.newContext();
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-glmember", "Perm Member");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    // The member sees no "Modifier" link on the owner's item...
    await member.goto("/grocery-list");
    await expect(
      member.locator("li", { hasText: "Sel" }).getByRole("link", { name: "Modifier" }),
    ).toHaveCount(0);
    // ...and GETting its edit screen directly is forbidden.
    await member.goto(editHref);
    await expect(member.getByRole("heading", { name: "Action non autorisée" })).toBeVisible();

    // ...but the member can still check the item off (any member may).
    await member.goto("/grocery-list");
    await member.locator("li", { hasText: "Sel" }).getByRole("button", { name: "Cocher" }).click();
    await expect(member.getByText("Article coché.")).toBeVisible();
    await context.close();
  });
});
