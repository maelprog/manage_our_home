import { expect, Page, test } from "@playwright/test";
import { parisDay } from "../lib/dates";
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

interface ItemOpts {
  name: string;
  quantity: string;
  unit: string;
  threshold?: string;
  category?: string;
  /** `YYYY-MM-DD`, as an `<input type="date">` takes it. */
  expiresOn?: string;
}

/** `YYYY-MM-DD` → the `DD/MM/YYYY` the pages print. */
function frDate(iso: string): string {
  const [y, m, d] = iso.split("-");
  return `${d}/${m}/${y}`;
}

/** Fill and submit /stocks/new; asserts the redirect + notice. */
async function createItem(page: Page, opts: ItemOpts): Promise<void> {
  await page.goto("/stocks/new");
  await page.getByLabel("Nom").fill(opts.name);
  await page.getByLabel("Quantité").fill(opts.quantity);
  await page.getByLabel("Unité").fill(opts.unit);
  if (opts.category) await page.getByLabel("Catégorie").fill(opts.category);
  if (opts.threshold !== undefined) await page.getByLabel("Seuil de réappro").fill(opts.threshold);
  if (opts.expiresOn !== undefined) await page.getByLabel("Date de péremption").fill(opts.expiresOn);
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
  // #527: the stepper of a card, the quantity with its unit, the place
  // filters and the search, without JavaScript in the way of any of them.
  test("add a product, one up and one down from the list, then find it with its unit", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-ststep", "Stepper User");
    await createGroup(page, "Famille Placard");
    await createItem(page, { name: "Pâtes complètes", quantity: "2", unit: "paquet", category: "Placard" });
    await createItem(page, { name: "Crème", quantity: "1", unit: "pot", category: "Frigo" });

    const qty = (name: string) => page.locator("li", { hasText: name }).locator(".stepper > span");
    await page.goto("/stocks");
    await expect(qty("Pâtes complètes")).toHaveText("2 paquets");
    await expect(qty("Crème")).toHaveText("1 pot");

    await page.getByRole("button", { name: "Ajouter 1 à Pâtes complètes" }).click();
    await expect(page).toHaveURL(/\/stocks\?notice=quantity_adjusted$/);
    await expect(page.getByText("Quantité mise à jour.")).toBeVisible();
    await expect(qty("Pâtes complètes")).toHaveText("3 paquets");
    await page.getByRole("button", { name: "Retirer 1 à Pâtes complètes" }).click();
    await page.getByRole("button", { name: "Retirer 1 à Pâtes complètes" }).click();
    await expect(qty("Pâtes complètes")).toHaveText("1 paquet");

    // A place filter, kept by the stepper's way back.
    const filters = page.getByRole("navigation", { name: "Filtres" });
    await filters.getByRole("link", { name: "Frigo", exact: true }).click();
    await expect(page).toHaveURL(/\/stocks\?lieu=Frigo$/);
    await expect(page.locator("li", { hasText: "Pâtes complètes" })).toHaveCount(0);
    await page.getByRole("button", { name: "Retirer 1 à Crème" }).click();
    await expect(page).toHaveURL(/\/stocks\?lieu=Frigo&notice=quantity_adjusted$/);
    await expect(qty("Crème")).toHaveText("0 pot");
    await expect(page.getByRole("button", { name: "Retirer 1 à Crème" })).toBeDisabled();

    // The search, which keeps the place it was typed in.
    await filters.getByRole("link", { name: "Tout", exact: true }).click();
    await page.getByRole("searchbox", { name: "Chercher un produit" }).fill("pâtes");
    await page.getByRole("searchbox", { name: "Chercher un produit" }).press("Enter");
    await expect(page).toHaveURL(/\/stocks\?q=p%C3%A2tes$/);
    await expect(qty("Pâtes complètes")).toHaveText("1 paquet");
    await expect(page.locator("li", { hasText: "Crème" })).toHaveCount(0);
  });

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

  // #458: the unit field is optional; left empty, the article gets « unité ».
  test("an empty unit is saved as « unité »", async ({ page }) => {
    await registerAndLogin(page, "e2e-stunit", "Unit User");
    await createGroup(page, "Famille Oeufs");
    await createItem(page, { name: "Oeufs", quantity: "6", unit: "" });
    await openItemDetail(page, "Oeufs");
    await expect(page.getByText("Quantité : 6", { exact: true })).toBeVisible();
  });

  test("unknown item id shows the not-found page", async ({ page }) => {
    await registerAndLogin(page, "e2e-st404", "Stock 404");
    await createGroup(page, "Famille 404");
    await page.goto("/stocks/00000000-0000-4000-8000-000000000000");
    await expect(page.getByRole("heading", { name: "Article introuvable" })).toBeVisible();
  });
});

// #401: one expiry date per article (the nearest). Dates are built in
// Europe/Paris (`lib/dates.ts`), the day the app judges "today" against.
test.describe("Stocks — expiry date", () => {
  test("the list shows each date with a worded status and can put the soonest first", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-stexpiry", "Expiry User");
    await createGroup(page, "Famille Fraîcheur");
    const yesterday = parisDay(-1);
    const inTwoDays = parisDay(2);
    const inAMonth = parisDay(30);
    await createItem(page, { name: "Yaourt", quantity: "4", unit: "pot", expiresOn: yesterday });
    await createItem(page, { name: "Lait", quantity: "1", unit: "L", expiresOn: inTwoDays });
    await createItem(page, { name: "Riz", quantity: "2", unit: "kg", expiresOn: inAMonth });
    await createItem(page, { name: "Sel", quantity: "1", unit: "kg" });

    // The pastilles of #527: red from the day before, amber for the rest
    // of the week (named by its weekday), a later date written out.
    await page.goto("/stocks");
    const row = (name: string) => page.locator("li", { hasText: name });
    await expect(row("Yaourt").locator(".badge.late")).toHaveText("périmé");
    await expect(row("Lait").locator(".badge.soon")).toContainText("périme ");
    await expect(row("Riz").locator(".badge")).toHaveText(`jusqu'au ${frDate(inAMonth)}`);
    await expect(row("Riz").locator(".badge.late, .badge.soon")).toHaveCount(0);
    await expect(row("Sel").locator(".badge")).toHaveCount(0);

    const names = page.locator("ul.stock li strong");
    await expect(names).toHaveText(["Lait", "Riz", "Sel", "Yaourt"]);
    await page.getByRole("link", { name: "À consommer en premier" }).click();
    await expect(page).toHaveURL(/\/stocks\?sort=expires_on$/);
    await expect(names).toHaveText(["Yaourt", "Lait", "Riz", "Sel"]);
    await page.getByRole("link", { name: "Trier par nom" }).click();
    await expect(names).toHaveText(["Lait", "Riz", "Sel", "Yaourt"]);
  });

  test("a full edit changes the date, then clears it", async ({ page }) => {
    await registerAndLogin(page, "e2e-stexpedit", "Expiry Editor");
    await createGroup(page, "Famille Dates");
    await createItem(page, { name: "Crème", quantity: "1", unit: "pot", expiresOn: parisDay(20) });

    await openItemDetail(page, "Crème");
    await expect(page.getByText(`Péremption : ${frDate(parisDay(20))}`)).toBeVisible();
    await expect(page.getByText("À consommer bientôt")).toHaveCount(0);

    await page.getByRole("link", { name: "Modifier l'article" }).click();
    await expect(page.getByLabel("Date de péremption")).toHaveValue(parisDay(20));
    await page.getByLabel("Date de péremption").fill(parisDay(0));
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page.getByText("Article mis à jour.")).toBeVisible();
    await expect(page.getByText(`Péremption : ${frDate(parisDay(0))}`)).toBeVisible();
    await expect(page.getByText("À consommer bientôt")).toBeVisible();

    await page.getByRole("link", { name: "Modifier l'article" }).click();
    await page.getByLabel("Date de péremption").fill("");
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page.getByText("Article mis à jour.")).toBeVisible();
    await expect(page.getByText("Aucune date de péremption.")).toBeVisible();
  });

  // #564: the Stocks entry counts what is past or close to its date, on
  // the PC's sidebar and on the phone's Cuisine tab — the same link.
  test("an article due tomorrow puts « 1 » on the Stocks tab", async ({ page }) => {
    await registerAndLogin(page, "e2e-stbadge", "Badge Stocks");
    await createGroup(page, "Famille Pastille Stocks");
    const tab = page.locator('.tabs a[href="/stocks"].pc');

    await page.goto("/agenda");
    await expect(tab).toHaveAccessibleName("Stocks");
    await expect(page.locator(".tabs .badge")).toHaveCount(0);

    await createItem(page, { name: "Crème", quantity: "1", unit: "pot", expiresOn: parisDay(1) });
    await createItem(page, { name: "Riz", quantity: "1", unit: "kg", expiresOn: parisDay(30) });
    await page.goto("/agenda");
    await expect(tab.locator(".badge")).toHaveText("1");
    await expect(tab).toHaveAccessibleName("Stocks (1 bientôt périmé)");

    await page.setViewportSize({ width: 390, height: 844 });
    const cuisine = page.locator('.tabs a[href="/stocks"].phone');
    await expect(cuisine.locator(".badge")).toHaveText("1");
    await expect(cuisine).toHaveAccessibleName("Cuisine (1 bientôt périmé)");
    await expect(cuisine.locator(".badge")).toBeInViewport({ ratio: 1 });
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
    await expect(member.getByText("5 plaquettes")).toBeVisible();
    await context.close();
  });
});
