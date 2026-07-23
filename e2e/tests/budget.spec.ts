import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Front epic F7 — Budget (issue #22): every user journey the epic introduces,
// happy paths plus the documented error states from apps/api/src/budget/'s
// error tables (see docs/front-epic-7-budget.md). Budget is tied to the grocery
// list: manual price entry (/budget/new) + price-on-checkout on a checked
// grocery item. Any member may create/read/set a price, so those controls show
// for everyone; the per-row "Modifier" link and the edit/delete screen stay
// behind can_modify (creator/admin/owner). Set-price upserts on the item, so a
// re-tap re-tarifies rather than double-counting.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-budget-password-1";

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

/** Adds a manual budget entry via /budget/new (leaves the pre-filled date). */
async function addEntry(page: Page, name: string, amount: string): Promise<void> {
  await page.goto("/budget/new");
  await page.getByLabel("Nom").fill(name);
  await page.getByLabel("Montant").fill(amount);
  await page.getByRole("button", { name: "Ajouter la dépense" }).click();
  await expect(page).toHaveURL(/\/budget\?notice=entry_created$/);
}

/** Adds a manual grocery item via the inline form on /grocery-list. */
async function addGroceryItem(page: Page, name: string): Promise<void> {
  await page.goto("/grocery-list");
  await page.getByLabel("Article").fill(name);
  await page.getByRole("button", { name: "Ajouter" }).click();
  await expect(page).toHaveURL(/\/grocery-list\?notice=item_added$/);
}

test.describe("Budget — manual entry", () => {
  test("adds an entry and it appears in the list and the monthly summary", async ({ page }) => {
    await registerAndLogin(page, "e2e-budgetnew", "Budget Adder");
    await createGroup(page, "Famille Budget");

    await addEntry(page, "Courses Super U", "12.50");
    await expect(page.getByText("Dépense ajoutée.")).toBeVisible();

    // Entry list row.
    const row = page.locator("li", { hasText: "Courses Super U" });
    await expect(row.getByText("12,50 €")).toBeVisible();

    // Monthly summary section carries the same cumulated total.
    const summary = page.locator("#monthly-summary");
    await expect(summary.getByText("12,50 €")).toBeVisible();
  });

  test("server-side empty-name rejection shows the error", async ({ page }) => {
    await registerAndLogin(page, "e2e-budgetname", "Budget Name");
    await createGroup(page, "Famille Budget Vide");
    await page.goto("/budget/new");
    // Bypass the browser's `required` so the shared server-side validation is
    // what rejects it.
    await page.getByLabel("Nom").evaluate((el) => el.removeAttribute("required"));
    await page.getByLabel("Montant").fill("5");
    await page.getByRole("button", { name: "Ajouter la dépense" }).click();
    await expect(page.getByText("Le nom est obligatoire.")).toBeVisible();
  });

  test("server-side negative-amount rejection shows the error", async ({ page }) => {
    await registerAndLogin(page, "e2e-budgetneg", "Budget Neg");
    await createGroup(page, "Famille Budget Négatif");
    await page.goto("/budget/new");
    await page.getByLabel("Nom").fill("Remboursement");
    // Drop min=0 so the browser doesn't block submission — the Rust handler is
    // what must reject the negative amount.
    await page.getByLabel("Montant").evaluate((el) => el.removeAttribute("min"));
    await page.getByLabel("Montant").fill("-5");
    await page.getByRole("button", { name: "Ajouter la dépense" }).click();
    await expect(page.getByText("Le montant ne peut pas être négatif.")).toBeVisible();
  });

  test("unknown entry id shows the not-found page", async ({ page }) => {
    await registerAndLogin(page, "e2e-budget404", "Budget 404");
    await createGroup(page, "Famille Budget 404");
    await page.goto("/budget/00000000-0000-4000-8000-000000000000");
    await expect(page.getByRole("heading", { name: "Dépense introuvable" })).toBeVisible();
  });
});

test.describe("Budget — price on checkout", () => {
  test("prices a checked grocery item, and re-setting is idempotent", async ({ page }) => {
    await registerAndLogin(page, "e2e-budgetprice", "Budget Pricer");
    await createGroup(page, "Famille Prix");

    await addGroceryItem(page, "Bananes");
    // Check it off — the price form appears only on a bought (checked) item.
    await page.goto("/grocery-list");
    await page.locator("li", { hasText: "Bananes" }).getByRole("button", { name: "Cocher" }).click();
    await expect(page).toHaveURL(/\/grocery-list\?notice=item_checked$/);

    // Set the price inline.
    await page
      .locator("li", { hasText: "Bananes" })
      .getByLabel("Prix pour Bananes")
      .fill("3");
    await page
      .locator("li", { hasText: "Bananes" })
      .getByRole("button", { name: "Renseigner le prix" })
      .click();
    await expect(page).toHaveURL(/\/grocery-list\?notice=price_set$/);
    await expect(page.getByText("Prix enregistré.")).toBeVisible();

    // It shows up on /budget as an entry (denormalized name + "Courses" badge)
    // and in the monthly total.
    await page.goto("/budget");
    const row = page.locator("li", { hasText: "Bananes" });
    await expect(row).toHaveCount(1);
    await expect(row.getByText("3,00 €")).toBeVisible();
    await expect(row.getByText("Courses")).toBeVisible();
    const summary = page.locator("#monthly-summary");
    await expect(summary.getByText("3,00 €")).toBeVisible();

    // Re-set the price on the same item: the backend upserts, so it re-tarifies
    // the single entry instead of double-counting.
    await page.goto("/grocery-list");
    await page
      .locator("li", { hasText: "Bananes" })
      .getByLabel("Prix pour Bananes")
      .fill("4");
    await page
      .locator("li", { hasText: "Bananes" })
      .getByRole("button", { name: "Renseigner le prix" })
      .click();
    await expect(page).toHaveURL(/\/grocery-list\?notice=price_set$/);

    await page.goto("/budget");
    const rows = page.locator("li", { hasText: "Bananes" });
    await expect(rows).toHaveCount(1);
    await expect(rows.getByText("4,00 €")).toBeVisible();
    // Monthly total is 4,00 €, not 7,00 € — no double-count.
    const summary2 = page.locator("#monthly-summary");
    await expect(summary2.getByText("4,00 €")).toBeVisible();
    await expect(summary2.getByText("7,00 €")).toHaveCount(0);
  });
});

test.describe("Budget — edit & delete", () => {
  test("edit round-trips through the entry form", async ({ page }) => {
    await registerAndLogin(page, "e2e-budgetedit", "Budget Editor");
    await createGroup(page, "Famille Édition Budget");
    await addEntry(page, "Boulangerie", "8");

    await page.goto("/budget");
    await page.locator("li", { hasText: "Boulangerie" }).getByRole("link", { name: "Modifier" }).click();
    // Pre-filled from the stored entry (amount as two decimals for the input).
    await expect(page.getByLabel("Nom")).toHaveValue("Boulangerie");
    await expect(page.getByLabel("Montant")).toHaveValue("8.00");

    await page.getByLabel("Nom").fill("Boulangerie du coin");
    await page.getByLabel("Montant").fill("9.20");
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page).toHaveURL(/\/budget\?notice=entry_updated$/);
    await expect(page.getByText("Dépense mise à jour.")).toBeVisible();
    await expect(
      page.locator("li", { hasText: "Boulangerie du coin" }).getByText("9,20 €"),
    ).toBeVisible();
  });

  test("delete removes the entry", async ({ page }) => {
    await registerAndLogin(page, "e2e-budgetdel", "Budget Deleter");
    await createGroup(page, "Famille Suppression Budget");
    await addEntry(page, "À supprimer", "1");

    await page.goto("/budget");
    await page.locator("li", { hasText: "À supprimer" }).getByRole("link", { name: "Modifier" }).click();
    await page.getByRole("button", { name: "Supprimer" }).click();
    await expect(page).toHaveURL(/\/budget\?notice=entry_deleted$/);
    await expect(page.getByText("Dépense supprimée.")).toBeVisible();
    await expect(page.locator("li", { hasText: "À supprimer" })).toHaveCount(0);
  });
});

test.describe("Budget — permission bar", () => {
  test("a standard member can add/price but not edit/delete another member's entry", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-budgetpermowner", "Perm Owner");
    await createGroup(page, "Famille Droits Budget");
    const href = await createInvitationLink(page, "Famille Droits Budget");

    // Owner adds an entry and captures its edit URL, plus a grocery item the
    // member will price.
    await addEntry(page, "Dépense Owner", "20");
    await page.goto("/budget");
    const editHref = await page
      .locator("li", { hasText: "Dépense Owner" })
      .getByRole("link", { name: "Modifier" })
      .getAttribute("href");
    if (!editHref) throw new Error("no edit link for the owner's entry");
    await addGroceryItem(page, "Lait partagé");
    await page.goto("/grocery-list");
    await page.locator("li", { hasText: "Lait partagé" }).getByRole("button", { name: "Cocher" }).click();

    const context = await browser.newContext();
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-budgetmember", "Perm Member");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    // The member sees no "Modifier" link on the owner's entry...
    await member.goto("/budget");
    await expect(member.getByText("Dépense Owner")).toBeVisible();
    await expect(
      member.locator("li", { hasText: "Dépense Owner" }).getByRole("link", { name: "Modifier" }),
    ).toHaveCount(0);
    // ...and GETting its edit screen directly is forbidden.
    await member.goto(editHref);
    await expect(member.getByRole("heading", { name: "Action non autorisée" })).toBeVisible();

    // ...but the member can still add their own entry...
    await addEntry(member, "Dépense Membre", "5");
    await expect(member.locator("li", { hasText: "Dépense Membre" })).toHaveCount(1);

    // ...and set a price on a checked grocery item (any member may).
    await member.goto("/grocery-list");
    await member
      .locator("li", { hasText: "Lait partagé" })
      .getByLabel("Prix pour Lait partagé")
      .fill("2.30");
    await member
      .locator("li", { hasText: "Lait partagé" })
      .getByRole("button", { name: "Renseigner le prix" })
      .click();
    await expect(member.getByText("Prix enregistré.")).toBeVisible();
    await member.goto("/budget");
    await expect(member.locator("li", { hasText: "Lait partagé" }).getByText("2,30 €")).toBeVisible();

    await context.close();
  });
});
