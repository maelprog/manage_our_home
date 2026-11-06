import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";
import { parisDayOfMonth } from "../lib/dates";

// Issue #622 — the agenda of several families. The Rust tests cover the
// selection's parsing, its check against the visitor's families, the merge
// of their occurrences and the page when one family fails; only a browser
// says that the selector, the cookie and the links of the event pages work
// together, and that the active family is left where it was.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-families-password-1";

// Neither name holds a word of a label on the pages under test (getByLabel
// matches substrings, the family's name included: see agenda.spec.ts).
const F1 = "Foyer Un";
const F2 = "Foyer Deux";

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

/** Creates a family, which becomes the active one. */
async function createGroup(page: Page, name: string): Promise<void> {
  await page.goto("/groups/new");
  await page.getByLabel("Nom du groupe").fill(name);
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
}

/** Fills and submits the sheet already open on `page`. */
async function submitSheet(page: Page, title: string, day: number): Promise<void> {
  const date = parisDayOfMonth(day);
  await page.getByLabel("Titre").fill(title);
  await page.getByLabel("Début").fill(`${date}T10:00`);
  await page.getByLabel("Fin").fill(`${date}T11:00`);
  await page.getByRole("button", { name: "Créer l'événement" }).click();
  await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);
}

/** The active family, as the sidebar's profile block names it (#523). */
function activeFamily(page: Page) {
  return page.locator('header a[href="/account"] .muted');
}

/** Ticks exactly `families` in the selector and submits it. */
async function show(page: Page, families: string[]): Promise<void> {
  const selector = page.getByRole("form", { name: "Familles affichées" });
  for (const name of [F1, F2]) {
    await selector.getByRole("checkbox", { name, exact: true }).setChecked(families.includes(name));
  }
  await selector.getByRole("checkbox", { name: "Toutes", exact: true }).setChecked(false);
  await selector.getByRole("button", { name: "Afficher" }).click();
  await expect(page).toHaveURL(/familles=/);
}

/** The chip of `title`, whose name ends with its family's in a view of several. */
function chip(page: Page, title: string) {
  return page.locator("a.chip", { hasText: title });
}

test.describe("Agenda — several families (#622)", () => {
  test("one family, the other, both; the selection survives › and a reload; the active family stays", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-agfam", "Agenda Familles");
    // F2 first, then F1: F1 is the active family from here on.
    await createGroup(page, F2);
    await page.goto("/agenda/new");
    await submitSheet(page, "Piscine Deux", 15);
    await createGroup(page, F1);
    await page.goto("/agenda/new");
    await submitSheet(page, "Dentiste Un", 16);

    // Without a selection: the active family alone, as before.
    await page.goto("/agenda");
    await expect(activeFamily(page)).toHaveText(F1);
    await expect(chip(page, "Dentiste Un")).toBeVisible();
    await expect(chip(page, "Piscine Deux")).toHaveCount(0);

    // F2 alone.
    await show(page, [F2]);
    await expect(chip(page, "Piscine Deux")).toBeVisible();
    await expect(chip(page, "Dentiste Un")).toHaveCount(0);

    // F1 alone again.
    await show(page, [F1]);
    await expect(chip(page, "Dentiste Un")).toBeVisible();
    await expect(chip(page, "Piscine Deux")).toHaveCount(0);

    // Both: each event with its family's name.
    await show(page, [F1, F2]);
    await expect(page.getByRole("link", { name: `Piscine Deux (${F2})` })).toBeVisible();
    await expect(page.getByRole("link", { name: `Dentiste Un (${F1})` })).toBeVisible();

    // The next month keeps the selection, in the URL.
    await page.getByRole("link", { name: "Mois suivant" }).click();
    await expect(page).toHaveURL(/familles=/);
    const selector = page.getByRole("form", { name: "Familles affichées" });
    await expect(selector.getByRole("checkbox", { name: F1, exact: true })).toBeChecked();
    await expect(selector.getByRole("checkbox", { name: F2, exact: true })).toBeChecked();

    // And /agenda with no parameter at all, through the cookie.
    await page.goto("/agenda");
    await expect(page.getByRole("link", { name: `Piscine Deux (${F2})` })).toBeVisible();
    await expect(page.getByRole("link", { name: `Dentiste Un (${F1})` })).toBeVisible();

    // The active family has not moved.
    await expect(activeFamily(page)).toHaveText(F1);
  });

  test("from the combined view, open then edit an event of the other family; create in it", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-agfamedit", "Agenda Familles Edit");
    await createGroup(page, F2);
    await page.goto("/agenda/new");
    await submitSheet(page, "Piscine Deux", 15);
    await createGroup(page, F1);
    await page.goto("/agenda");
    await show(page, [F1, F2]);

    // Open F2's event from the view of both, F1 being active.
    await chip(page, "Piscine Deux").click();
    await expect(page.getByRole("heading", { level: 1, name: "Piscine Deux" })).toBeVisible();
    await expect(page).toHaveURL(/famille=/);
    await expect(page.getByText(`Famille : ${F2}`)).toBeVisible();

    // Edit it.
    await page.getByRole("link", { name: "Modifier" }).click();
    await page.getByLabel("Titre").fill("Piscine Deux déplacée");
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page.getByText("Événement mis à jour.")).toBeVisible();
    await expect(page.getByRole("heading", { level: 1, name: "Piscine Deux déplacée" })).toBeVisible();
    await expect(activeFamily(page)).toHaveText(F1);

    // Create from the view of both, choosing F2.
    await page.goto("/agenda/new");
    const choice = page.getByRole("navigation", { name: "Famille" });
    await expect(choice.getByRole("link", { name: F1 })).toHaveAttribute("aria-current", "true");
    await choice.getByRole("link", { name: F2 }).click();
    await expect(page.locator(".sheet .eyebrow")).toHaveText(F2);
    await submitSheet(page, "Goûter Deux", 17);

    // It is F2's: shown with F2 alone, not with F1 alone.
    await page.goto("/agenda");
    await show(page, [F2]);
    await expect(chip(page, "Goûter Deux")).toBeVisible();
    await expect(chip(page, "Piscine Deux déplacée")).toBeVisible();
    await show(page, [F1]);
    await expect(chip(page, "Goûter Deux")).toHaveCount(0);
    await expect(activeFamily(page)).toHaveText(F1);
  });
});
