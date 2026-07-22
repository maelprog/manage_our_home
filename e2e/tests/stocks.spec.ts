import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Front epic #4 — Stocks (issue #19) + follow-up #39: every user journey the
// epic introduces, happy paths plus the documented error states from
// apps/api/src/stocks/'s error tables. Note the two-tier permission bar: a
// quantity-only adjustment is open to any family member (shared inventory), so
// every member sees the adjust form; the full-record edit and delete stay
// behind can_modify (creator/admin/owner), so a standard member gets no
// edit/delete controls on another member's item.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-stocks-password-1";

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

interface ItemOpts {
  name: string;
  quantity: string;
  unit: string;
  threshold?: string;
  category?: string;
}

/** Fill and submit /stocks/new; asserts the redirect + notice. */
async function createItem(page: Page, opts: ItemOpts): Promise<void> {
  await page.goto("/stocks/new");
  await page.getByLabel("Nom").fill(opts.name);
  await page.getByLabel("Quantité").fill(opts.quantity);
  await page.getByLabel("Unité").fill(opts.unit);
  if (opts.category) await page.getByLabel("Catégorie").fill(opts.category);
  if (opts.threshold !== undefined) await page.getByLabel("Seuil de réappro").fill(opts.threshold);
  await page.getByRole("button", { name: "Ajouter l'article" }).click();
  await expect(page).toHaveURL(/\/stocks\?notice=item_created$/);
  await expect(page.getByText("Article créé.")).toBeVisible();
}

/** Opens an item's detail by clicking its name in the list. */
async function openItemDetail(page: Page, name: string): Promise<void> {
  await page.goto("/stocks");
  await page.getByRole("link", { name, exact: true }).click();
  await expect(page.getByRole("heading", { name })).toBeVisible();
}

test.describe("Stocks — list & create", () => {
  test("create an item; low-stock badge shows when quantity is at/below threshold", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-stcreate", "Stock Creator");
    await createGroup(page, "Famille Stock");

    // Above threshold: no badge.
    await createItem(page, { name: "Farine", quantity: "2", unit: "kg", threshold: "0.5" });
    await page.goto("/stocks");
    await expect(page.getByRole("link", { name: "Farine", exact: true })).toBeVisible();
    await expect(
      page.locator("li", { hasText: "Farine" }).getByText("Stock bas"),
    ).toHaveCount(0);

    // At/below threshold: badge shows.
    await createItem(page, { name: "Sucre", quantity: "0.3", unit: "kg", threshold: "0.5" });
    await page.goto("/stocks");
    await expect(page.locator("li", { hasText: "Sucre" }).getByText("Stock bas")).toBeVisible();
  });

  test("low-stock filter shows only low items", async ({ page }) => {
    await registerAndLogin(page, "e2e-stfilter", "Filter User");
    await createGroup(page, "Famille Filtre");
    await createItem(page, { name: "Riz plein", quantity: "5", unit: "kg", threshold: "1" });
    await createItem(page, { name: "Lait bas", quantity: "0", unit: "L", threshold: "2" });

    await page.goto("/stocks?low_stock=1");
    await expect(page.getByRole("link", { name: "Lait bas", exact: true })).toBeVisible();
    await expect(page.getByRole("link", { name: "Riz plein", exact: true })).toHaveCount(0);
  });

  test("empty name is rejected before an API round-trip", async ({ page }) => {
    await registerAndLogin(page, "e2e-stvalid", "Validation User");
    await createGroup(page, "Famille Validation");
    await page.goto("/stocks/new");
    // Bypass the browser's `required` by removing the attribute, so the
    // server-side shared validation is what rejects it.
    await page.getByLabel("Nom").evaluate((el) => el.removeAttribute("required"));
    await page.getByLabel("Unité").fill("kg");
    await page.getByRole("button", { name: "Ajouter l'article" }).click();
    await expect(page.getByText("Le nom est obligatoire.")).toBeVisible();
  });

  test("unknown item id shows the not-found page", async ({ page }) => {
    await registerAndLogin(page, "e2e-st404", "Stock 404");
    await createGroup(page, "Famille 404");
    await page.goto("/stocks/00000000-0000-4000-8000-000000000000");
    await expect(page.getByRole("heading", { name: "Article introuvable" })).toBeVisible();
  });
});

test.describe("Stocks — edit, adjust & delete", () => {
  test("full edit updates the item's fields", async ({ page }) => {
    await registerAndLogin(page, "e2e-stedit", "Edit User");
    await createGroup(page, "Famille Édition");
    await createItem(page, { name: "Café", quantity: "1", unit: "paquet" });

    await openItemDetail(page, "Café");
    await page.getByRole("link", { name: "Modifier l'article" }).click();
    await page.getByLabel("Nom").fill("Café moulu");
    await page.getByLabel("Seuil de réappro").fill("2");
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page.getByText("Article mis à jour.")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Café moulu" })).toBeVisible();
    // Quantity 1 <= new threshold 2 → now flagged low.
    await expect(page.getByText("Stock bas")).toBeVisible();
  });

  test("delete removes the item from the list", async ({ page }) => {
    await registerAndLogin(page, "e2e-stdel", "Delete User");
    await createGroup(page, "Famille Suppression");
    await createItem(page, { name: "À jeter", quantity: "1", unit: "unité" });

    await openItemDetail(page, "À jeter");
    await page.getByRole("button", { name: "Supprimer" }).click();
    await expect(page).toHaveURL(/\/stocks\?notice=item_deleted$/);
    await expect(page.getByText("Article supprimé.")).toBeVisible();
    await page.goto("/stocks");
    await expect(page.getByRole("link", { name: "À jeter", exact: true })).toHaveCount(0);
  });
});

test.describe("Stocks — permission bar", () => {
  test("a standard member can adjust the quantity of an item they created", async ({
    page,
    browser,
  }) => {
    // Owner sets up the family and invites a second user.
    await registerAndLogin(page, "e2e-stpermowner", "Perm Owner");
    await createGroup(page, "Famille Droits Stock");
    const href = await createInvitationLink(page, "Famille Droits Stock");

    const context = await browser.newContext();
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-stmember", "Perm Member");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    // The member (standard role) creates their own item, then adjusts it —
    // creator ⇒ can_modify, so this is allowed.
    await createItem(member, { name: "Thé", quantity: "3", unit: "boîte" });
    await openItemDetail(member, "Thé");
    await member.getByLabel("Ajuster la quantité").fill("1");
    await member.getByRole("button", { name: "Mettre à jour la quantité" }).click();
    await expect(member.getByText("Quantité mise à jour.")).toBeVisible();
    await expect(member.getByText("1 boîte")).toBeVisible();
    await context.close();
  });

  test("a standard member can adjust another member's item but cannot edit or delete it", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-stpermowner2", "Perm Owner 2");
    await createGroup(page, "Famille Droits Stock 2");
    const href = await createInvitationLink(page, "Famille Droits Stock 2");

    // Owner creates the item.
    await createItem(page, { name: "Beurre du proprio", quantity: "2", unit: "plaquette" });

    const context = await browser.newContext();
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-stmember2", "Perm Member 2");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    // The member sees the adjust form (shared inventory) but no edit/delete.
    await openItemDetail(member, "Beurre du proprio");
    await expect(
      member.getByText("Seul le créateur ou un administrateur peut modifier ou supprimer"),
    ).toBeVisible();
    await expect(member.getByRole("link", { name: "Modifier l'article" })).toHaveCount(0);
    await expect(member.getByRole("button", { name: "Supprimer" })).toHaveCount(0);

    // ...and can actually adjust the quantity of the owner's item → 200.
    await member.getByLabel("Ajuster la quantité").fill("5");
    await member.getByRole("button", { name: "Mettre à jour la quantité" }).click();
    await expect(member.getByText("Quantité mise à jour.")).toBeVisible();
    await expect(member.getByText("5 plaquette")).toBeVisible();
    await context.close();
  });
});
