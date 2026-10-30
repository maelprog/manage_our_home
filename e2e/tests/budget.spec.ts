import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Front epic F7 — Budget (issue #22): every user journey the epic introduces,
// happy paths plus the documented error states from apps/api/src/budget/'s
// error tables (see docs/front-epic-7-budget.md). Budget is tied to the grocery
// list: manual price entry (/budget/new) + price-on-checkout on a checked
// grocery item. Only an owner or an admin reaches the budget (#531): a standard
// member sees no link to it, no price field on the grocery list, and a 404 on
// its pages. Past that bar, the per-row "Modifier" link and the edit/delete
// screen stay behind can_modify (creator/admin/owner). Set-price upserts on the
// item, so a re-tap re-tarifies rather than double-counting.

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

test.describe("Budget — categories, caps and alerts (#533)", () => {
  test("a capped category's bar shows the share its expense takes", async ({ page }) => {
    await registerAndLogin(page, "e2e-budgetcat", "Budget Plafonds");
    await createGroup(page, "Famille Plafonds");

    // The family's cap, then a category with its own cap.
    await page.goto("/budget/settings");
    await page.getByRole("spinbutton", { name: "Plafond de la famille (€)" }).fill("1000");
    await page.getByRole("button", { name: "Enregistrer le plafond" }).click();
    await expect(page).toHaveURL(/\/budget\/settings\?notice=cap_saved$/);

    const add = page.locator('form[action="/budget/settings/categories"]');
    await add.getByRole("textbox", { name: "Nom de la catégorie" }).fill("Alimentation");
    await add.getByRole("combobox", { name: "Couleur" }).selectOption("green");
    await add.getByRole("spinbutton", { name: "Plafond de la catégorie (€, facultatif)" }).fill("200");
    await add.getByRole("button", { name: "Ajouter la catégorie" }).click();
    await expect(page).toHaveURL(/\/budget\/settings\?notice=category_created$/);
    await expect(page.getByText("Alimentation — plafond 200,00 €")).toBeVisible();

    // An expense filed under it, from its pill.
    await page.goto("/budget/new");
    await page.getByLabel("Montant").fill("50");
    await page.getByLabel("Nom").fill("Marché du samedi");
    await page.getByRole("radio", { name: "Alimentation" }).check();
    await page.getByRole("button", { name: "Ajouter la dépense" }).click();
    await expect(page).toHaveURL(/\/budget\?notice=entry_created$/);

    // 50 of 200 is 25 %, said in words and drawn on the bar.
    const category = page.locator("li", { has: page.locator("progress") }).filter({ hasText: "Alimentation" });
    await expect(category.getByText("25 % de 200,00 €")).toBeVisible();
    await expect(category.locator("progress")).toHaveAttribute("value", "25");
    await expect(category.getByText("Dépassé")).toHaveCount(0);
    // The family's figures, and the expense under its category.
    const summary = page.locator("#monthly-summary");
    await expect(summary.getByText("950,00 €")).toBeVisible();
    await expect(page.locator("li", { hasText: "Marché du samedi" }).getByText("Alimentation")).toBeVisible();

    // A second expense takes the category past its cap.
    await page.goto("/budget/new");
    await page.getByLabel("Montant").fill("160");
    await page.getByLabel("Nom").fill("Traiteur");
    await page.getByRole("radio", { name: "Alimentation" }).check();
    await page.getByRole("button", { name: "Ajouter la dépense" }).click();
    await expect(page).toHaveURL(/\/budget\?notice=entry_created$/);
    await expect(category.getByText("105 % de 200,00 €")).toBeVisible();
    await expect(category.getByText("Dépassé")).toBeVisible();
    await expect(category.locator("progress")).toHaveAttribute("value", "100");
  });

  test("an alert switch turned off stays off", async ({ page }) => {
    await registerAndLogin(page, "e2e-budgetalert", "Budget Alertes");
    await createGroup(page, "Famille Alertes");
    await page.goto("/budget/settings");
    const global = page.getByRole("checkbox", { name: "À 80 % puis à 100 % du plafond de la famille" });
    const category = page.getByRole("checkbox", { name: "Au dépassement du plafond d'une catégorie" });
    await expect(global).toBeChecked();
    await expect(category).toBeChecked();
    await category.uncheck();
    await page.getByRole("button", { name: "Enregistrer les alertes" }).click();
    await expect(page).toHaveURL(/\/budget\/settings\?notice=alerts_saved$/);
    await expect(global).toBeChecked();
    await expect(category).not.toBeChecked();
  });

  test("an expense links the checked grocery items it paid for", async ({ page }) => {
    await registerAndLogin(page, "e2e-budgetlink", "Budget Articles");
    await createGroup(page, "Famille Articles");
    for (const name of ["Farine", "Sucre"]) {
      await addGroceryItem(page, name);
      await page.goto("/grocery-list");
      await page.locator("li", { hasText: name }).getByRole("button", { name: "Cocher" }).click();
      await expect(page).toHaveURL(/\/grocery-list\?notice=item_checked$/);
    }

    await page.goto("/budget/new");
    await page.getByLabel("Montant").fill("7.40");
    await page.getByLabel("Nom").fill("Épicerie");
    await page.getByRole("checkbox", { name: "Farine" }).check();
    await page.getByRole("checkbox", { name: "Sucre" }).check();
    await page.getByRole("button", { name: "Ajouter la dépense" }).click();
    await expect(page).toHaveURL(/\/budget\?notice=entry_created$/);
    await expect(page.locator("li", { hasText: "Épicerie" }).getByText("Courses")).toBeVisible();

    // Both items are now paid: the next expense is not offered them.
    await page.goto("/budget/new");
    await expect(page.getByRole("checkbox", { name: "Farine" })).toHaveCount(0);
    await expect(page.getByText("Aucun article coché à rattacher.")).toBeVisible();

    // Re-pricing one of them from the list says where to correct it.
    await page.goto("/grocery-list");
    await page.locator("li", { hasText: "Farine" }).getByLabel("Prix pour Farine").fill("2");
    await page.locator("li", { hasText: "Farine" }).getByRole("button", { name: "Renseigner le prix" }).click();
    await expect(page).toHaveURL(/\/grocery-list\?error=grocery_item_priced_with_others$/);
    await expect(page.getByText(/regroupe plusieurs articles/)).toBeVisible();
  });
});

test.describe("Budget — who paid (#638)", () => {
  test("an expense paid by a member names them in the list and the month's totals", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-budgetpayer", "Owner Payeuse");
    await createGroup(page, "Famille Payeurs");
    const href = await createInvitationLink(page, "Famille Payeurs");
    const context = await browser.newContext();
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-budgetpaid", "Paul Payeur");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();
    await context.close();

    // The signed-in member is ticked, nobody stays possible; another
    // member is picked.
    await page.goto("/budget/new");
    await expect(page.getByRole("radio", { name: "Owner Payeuse", exact: true })).toBeChecked();
    await expect(page.getByRole("radio", { name: "Personne", exact: true })).not.toBeChecked();
    await page.getByLabel("Montant").fill("23.40");
    await page.getByLabel("Nom").fill("Pharmacie");
    await page.getByRole("radio", { name: "Paul Payeur", exact: true }).check();
    await page.getByRole("button", { name: "Ajouter la dépense" }).click();
    await expect(page).toHaveURL(/\/budget\?notice=entry_created$/);
    await addEntry(page, "Boulangerie", "4");

    await expect(page.locator("li", { hasText: "Pharmacie" }).getByText("payé par Paul Payeur")).toBeVisible();
    await expect(page.locator("li", { hasText: "Boulangerie" }).getByText("payé par Owner Payeuse")).toBeVisible();
    const payers = page.locator("section", { has: page.getByRole("heading", { name: "Par payeur" }) });
    await expect(payers.locator("li", { hasText: "Paul Payeur" }).getByText("23,40 €")).toBeVisible();
    await expect(payers.locator("li", { hasText: "Owner Payeuse" }).getByText("4,00 €")).toBeVisible();

    // The edit form keeps the payer; « Personne » leaves the expense without one.
    await page.locator("li", { hasText: "Pharmacie" }).getByRole("link", { name: "Modifier" }).click();
    await expect(page.getByRole("radio", { name: "Paul Payeur", exact: true })).toBeChecked();
    await page.getByRole("radio", { name: "Personne", exact: true }).check();
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page).toHaveURL(/\/budget\?notice=entry_updated$/);
    await expect(page.locator("li", { hasText: "Pharmacie" }).getByText("payé par")).toHaveCount(0);
    await expect(payers.locator("li", { hasText: "Sans payeur" }).getByText("23,40 €")).toBeVisible();
  });
});

test.describe("Budget — reserved to owners and admins (#531)", () => {
  test("a standard member sees no way to the budget, and its pages are a 404", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-budgetpermowner", "Perm Owner");
    await createGroup(page, "Famille Droits Budget");
    const href = await createInvitationLink(page, "Famille Droits Budget");

    // The owner adds an entry and captures its edit URL, plus a checked
    // grocery item — which the owner may price.
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
    await expect(page.getByLabel("Prix pour Lait partagé")).toHaveCount(1);
    await page.goto("/");
    await expect(page.locator('a[href="/budget"]').first()).toBeAttached();

    // A member who joins through an invitation is standard.
    const context = await browser.newContext();
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-budgetmember", "Perm Member");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    // No link to the budget anywhere on the home page — neither the PC
    // sidebar's entry nor the card the phone reaches it through — and none
    // on the grocery list either.
    await member.goto("/");
    await expect(member.getByRole("heading", { name: /^Budget d/ })).toHaveCount(0);
    await expect(member.locator('a[href^="/budget"]')).toHaveCount(0);
    await member.goto("/grocery-list");
    await expect(member.locator('a[href^="/budget"]')).toHaveCount(0);
    // The checked item carries no price field.
    await expect(member.locator("li", { hasText: "Lait partagé" })).toHaveCount(1);
    await expect(member.getByLabel("Prix pour Lait partagé")).toHaveCount(0);
    await expect(member.getByRole("button", { name: "Renseigner le prix" })).toHaveCount(0);

    // Typed in by hand, every budget page answers as an unknown path does.
    const unknown = await member.goto("/cette-page-n-existe-pas");
    for (const path of ["/budget", "/budget/new", "/budget/settings", editHref]) {
      const res = await member.goto(path);
      expect(res?.status(), path).toBe(404);
      expect(await res?.text(), path).toBe(await unknown?.text());
    }

    await context.close();
  });
});
