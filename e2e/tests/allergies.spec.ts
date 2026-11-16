import { Browser, expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";
import { fillIngredients } from "../lib/recipe-form";

// Issue #819 — a member's allergies and dislikes, and the household's
// kitchen equipment, entered from the web. The Rust tests check each page's
// markup and the API's rules; this suite walks the forms end to end, and
// reads another member's pages for anything of the first one's allergies.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-allergies-password-1";

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

/** The owner invites a new member into `family`; returns the member's page. */
async function invite(owner: Page, browser: Browser, family: string, displayName: string): Promise<Page> {
  await owner.goto("/groups");
  await owner.locator("li", { hasText: family }).getByRole("link", { name: "Membres" }).click();
  await owner.getByRole("button", { name: "Créer une invitation" }).click();
  const link = await owner.locator(".notice.success a").getAttribute("href");
  if (!link) throw new Error("no invitation link rendered");
  const member = await (await browser.newContext()).newPage();
  await registerAndLogin(member, "e2e-allergies-member", displayName);
  await member.goto(link);
  await member.getByRole("button", { name: "Rejoindre le groupe" }).click();
  return member;
}

async function settingsUrl(page: Page, family: string): Promise<string> {
  await page.goto("/groups");
  await page.locator("li", { hasText: family }).getByRole("link", { name: "Paramètres" }).click();
  await expect(page.getByRole("heading", { level: 1, name: family })).toBeVisible();
  return page.url();
}

function allergy(page: Page, name: string) {
  return page.locator("details", { hasText: name });
}

function level(page: Page, name: string, family: string, title: string) {
  return allergy(page, name).getByRole("group", { name: family, exact: true }).getByRole("radio", {
    name: new RegExp(`^${title}`),
  });
}

test.describe("Allergies and dislikes (#819)", () => {
  test("consent, add, share per family, remove, erase everything; another member reads none of it", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-allergies-holder", "Camille Allergique");
    await createGroup(page, "Foyer Alpha");
    await createGroup(page, "Foyer Beta");
    const member = await invite(page, browser, "Foyer Alpha", "Dominique Voisin");

    await page.goto("/account");
    await page.getByRole("link", { name: /Mes allergies et aversions/ }).click();
    await expect(page.getByRole("heading", { level: 1, name: "Mes allergies et aversions" })).toBeVisible();
    await expect(page.getByRole("heading", { name: "Avant de commencer" })).toBeVisible();

    // No allergy without the box of consent: « Ajouter » offers a dislike
    // only, and « Continuer » refuses an unticked box.
    const food = page.locator('select[name="tag"]');
    await expect(page.getByRole("radio", { name: /Une allergie/ })).toHaveCount(0);
    await page.getByRole("button", { name: "Continuer" }).click();
    await expect(page.getByRole("alert")).toContainText("Votre consentement est nécessaire");
    await expect(page.getByText("Ma liste (0)")).toBeVisible();

    await page.getByRole("checkbox", { name: /Je consens/ }).check();
    await page.getByRole("button", { name: "Continuer" }).click();
    await expect(page.getByRole("heading", { name: "Mon consentement" })).toBeVisible();
    await expect(page.getByRole("radio", { name: /Une allergie/ })).toBeChecked();
    await food.selectOption({ label: "Arachides" });
    await page.getByRole("button", { name: "Ajouter à ma liste" }).click();
    await expect(page.getByRole("status").first()).toContainText("Allergie ajoutée.");
    await expect(page.getByRole("heading", { name: "Mon consentement" })).toBeVisible();
    await expect(page.getByRole("checkbox", { name: /Je consens/ })).toHaveCount(0);

    // Beyond the fourteen: a search, then the same list.
    await page.locator('input[name="query"]').fill("kiwi");
    await page.getByRole("button", { name: "Chercher" }).click();
    await expect(food.locator("option:checked")).toHaveText("Kiwis");
    await page.getByRole("button", { name: "Ajouter à ma liste" }).click();
    await expect(page.getByText("Ma liste (2)")).toBeVisible();

    // An accented name as long as the field allows is looked up, not refused.
    await page.locator('input[name="query"]').fill("é".repeat(128));
    await page.getByRole("button", { name: "Chercher" }).click();
    await expect(page.getByRole("alert")).toHaveCount(0);
    await expect(page.getByRole("status").first()).toContainText("Aucun aliment trouvé");

    // Private by default, in both families.
    await allergy(page, "Arachides").locator("summary").click();
    await expect(level(page, "Arachides", "Foyer Alpha", "Privé")).toBeChecked();
    await expect(level(page, "Arachides", "Foyer Beta", "Privé")).toBeChecked();

    // One family, then the other: the first keeps its level.
    await level(page, "Arachides", "Foyer Alpha", "Public").check();
    await expect(allergy(page, "Arachides").getByText("Les membres de Foyer Alpha verront votre nom")).toBeVisible();
    await allergy(page, "Arachides").getByRole("button", { name: "Enregistrer le partage" }).click();
    await expect(page.getByRole("status").first()).toContainText("Partage enregistré.");
    await allergy(page, "Arachides").locator("summary").click();
    await level(page, "Arachides", "Foyer Beta", "Anonyme").check();
    await allergy(page, "Arachides").getByRole("button", { name: "Enregistrer le partage" }).click();
    await expect(page.getByRole("status").first()).toContainText("Partage enregistré.");
    const pills = allergy(page, "Arachides").locator("summary .badge");
    await expect(pills).toHaveText(["Foyer Alpha : public", "Foyer Beta : anonyme"]);

    await allergy(page, "Kiwis").locator("summary").click();
    await level(page, "Kiwis", "Foyer Alpha", "Public").check();
    await allergy(page, "Kiwis").getByRole("button", { name: "Enregistrer le partage" }).click();
    await expect(allergy(page, "Kiwis").locator("summary")).toContainText("Foyer Alpha : public");

    // The other member of Foyer Alpha: neither the allergy nor its level on
    // their pages, though it is public there.
    for (const path of ["/account/allergies", "/account"]) {
      await member.goto(path);
      const html = await member.content();
      expect(html).not.toContain("Kiwis");
      expect(html).not.toContain("en:kiwifruits");
      // Arachides is one of the fourteen every page offers: its level, not
      // its name, would betray it.
      expect(html).not.toContain("Foyer Alpha : public");
      expect(html).not.toContain("Camille Allergique");
    }
    await member.goto(await settingsUrl(member, "Foyer Alpha"));
    const settings = await member.content();
    expect(settings).not.toContain("Kiwis");
    expect(settings).not.toContain("Arachides");
    expect(settings).not.toContain("en:peanuts");

    // Remove one.
    await page.goto("/account/allergies");
    await allergy(page, "Kiwis").locator("summary").click();
    await page.getByRole("button", { name: "Retirer « Kiwis »" }).click();
    await expect(page.getByText("Ma liste (1)")).toBeVisible();

    // A dislike, never shared.
    await page.getByRole("radio", { name: /Une aversion/ }).check();
    await food.selectOption({ label: "Céleri" });
    await page.getByRole("button", { name: "Ajouter à ma liste" }).click();
    await expect(page.getByText("Aversion · jamais partagée")).toBeVisible();

    // Erase everything: confirmed on its own page, consent withdrawn.
    await page.getByRole("link", { name: "Tout effacer" }).click();
    await expect(page.getByRole("heading", { level: 1, name: "Tout effacer ?" })).toBeVisible();
    await page.getByRole("button", { name: "Oui, tout effacer" }).click();
    await expect(page.getByRole("status").first()).toContainText("Toutes vos allergies et aversions ont été effacées");
    await expect(page.getByText("Ma liste (0)")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Avant de commencer" })).toBeVisible();
    await expect(page.getByRole("checkbox", { name: /Je consens/ })).toBeVisible();
  });

  test("the owner ticks the household's equipment, a recipe requires it, a standard member only reads it", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-equipment-owner", "Équipement Owner");
    await createGroup(page, "Foyer Gamma");
    const member = await invite(page, browser, "Foyer Gamma", "Équipement Member");

    await page.goto(await settingsUrl(page, "Foyer Gamma"));
    const card = page.locator("#equipements");
    await card.getByRole("checkbox", { name: "Four" }).check();
    await card.getByRole("button", { name: "Enregistrer les équipements" }).click();
    await expect(page.getByText("Équipements enregistrés.")).toBeVisible();
    await expect(page.locator("#equipements").getByRole("checkbox", { name: "Four" })).toBeChecked();

    await member.goto(await settingsUrl(member, "Foyer Gamma"));
    const read = member.locator("#equipements");
    await expect(read.getByText("Four", { exact: true })).toBeVisible();
    await expect(read.getByRole("checkbox")).toHaveCount(0);

    // A recipe that needs the oven.
    await page.goto("/recipes/new");
    await page.getByRole("textbox", { name: "Nom" }).fill("Gratin du four");
    await fillIngredients(page, "Pommes de terre | 1 | kg");
    await page.getByRole("group", { name: "Équipements requis" }).getByRole("checkbox", { name: "Four" }).check();
    await page.getByRole("button", { name: "Créer la recette" }).click();
    await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\?notice=recipe_created$/);
    const required = page.locator("section", { has: page.getByRole("heading", { name: "Équipements requis" }) });
    await expect(required.getByText("Four", { exact: true })).toBeVisible();

    // Unticked on the edit form: none required any more.
    await page.getByRole("link", { name: "Modifier la recette" }).click();
    const box = page.getByRole("group", { name: "Équipements requis" }).getByRole("checkbox", { name: "Four" });
    await expect(box).toBeChecked();
    await box.uncheck();
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page).toHaveURL(/\?notice=recipe_updated$/);
    await expect(page.getByText("Aucun équipement requis.")).toBeVisible();
  });
});
