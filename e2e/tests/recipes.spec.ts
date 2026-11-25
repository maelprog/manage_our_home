import { expect, Page, test } from "@playwright/test";
import { parisDay } from "../lib/dates";
import { fetchVerificationToken, publishToLibrary, withdrawFromLibrary } from "../lib/db";
import { fillIngredients, fillSteps, lineField } from "../lib/recipe-form";
import { skipMealStart } from "../lib/meal-start";

// The first questions about meals (#820) would open from the recipe list
// for every member registered here: tests/meal-start.spec.ts walks them.
test.beforeEach(async ({ context }) => {
  await skipMealStart(context);
});

// Front epic F5 — Recipes (issue #20): every user journey the epic
// introduces, happy paths plus the documented error states from
// apps/api/src/recipes/'s error tables (see docs/front-epic-5-recipes.md).
// Permission bar mirrors Stocks: any member may create/read a recipe or log a
// meal, so every member sees the log-a-meal form; the edit link and delete
// button stay behind can_modify (the creator, or a role meeting the family's
// `recipes_edit` threshold, #866 — admin/owner by default). The suggestion view
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

/** Adds a stock item so the suggestion scorer can match it by name. */
async function createStockItem(page: Page, name: string, quantity: string, unit: string): Promise<void> {
  await page.goto("/stocks/new");
  await page.getByLabel("Nom").fill(name);
  await page.getByLabel("Quantité").fill(quantity);
  await page.getByLabel("Unité", { exact: true }).fill(unit);
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
  if (opts.instructions) await fillSteps(page, opts.instructions);
  if (opts.ingredients) await fillIngredients(page, opts.ingredients);
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
    await fillIngredients(page, "Farine | 1 | kg");
    await page.getByRole("button", { name: "Créer la recette" }).click();
    await expect(page.getByText("Le nom est obligatoire.")).toBeVisible();
    // What was typed comes back.
    await expect(lineField(page, "Ingrédient", 1)).toHaveValue("Farine");
    await expect(lineField(page, "Quantité", 1)).toHaveValue("1");
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
    // The stock matching goes through Open Food Facts' categories (#406).
    await expect(page.getByText("Données produits : Open Food Facts, ODbL.")).toBeVisible();

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
  test("full edit round-trips through the ingredient lines", async ({ page }) => {
    await registerAndLogin(page, "e2e-rcedit", "Edit User");
    await createGroup(page, "Famille Édition Recette");
    await createRecipe(page, { name: "Soupe", ingredients: "Carotte | 3 | pièce" });

    await openRecipeDetail(page, "Soupe");
    await page.getByRole("link", { name: "Modifier la recette" }).click();
    // The lines are pre-filled from the stored ingredients (round-trip).
    await expect(lineField(page, "Ingrédient", 1)).toHaveValue("Carotte");
    await expect(lineField(page, "Quantité", 1)).toHaveValue("3");
    await expect(lineField(page, "Unité", 1)).toHaveValue("pièce");

    await page.getByLabel("Nom").fill("Soupe de légumes");
    await fillIngredients(page, "Carotte | 3 | pièce\nPoireau | 2 | pièce");
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

test.describe("Recipes — à réaliser (#470)", () => {
  test("the detail plans the recipe for its number of people, then takes it out", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-rcplan", "Recipe Planner");
    await createGroup(page, "Famille Planning");

    await page.goto("/recipes/new");
    await page.getByLabel("Nom").fill("Ratatouille");
    await fillIngredients(page, "Courgette | 2 | pièce");
    await page.getByLabel("Personnes").fill("4");
    await page.getByRole("button", { name: "Créer la recette" }).click();
    await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\?notice=recipe_created$/);
    await expect(page.getByText("Pour 4 personnes.")).toBeVisible();

    // The form proposes the recipe's own base.
    await expect(page.getByLabel("Personnes")).toHaveValue("4");
    await page.getByLabel("Personnes").fill("2");
    await page.getByRole("button", { name: "À réaliser" }).click();
    await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\?notice=recipe_selected$/);
    await expect(page.getByRole("spinbutton", { name: "Personnes, Ratatouille" })).toHaveValue("2");

    // The list says so too.
    await page.goto("/recipes");
    await expect(page.getByText("1 recette prévue pour la liste de courses")).toBeVisible();

    await page.getByRole("link", { name: "Ratatouille" }).first().click();
    await page.getByRole("button", { name: "Retirer" }).click();
    await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\?notice=recipe_deselected$/);
    await expect(page.getByRole("button", { name: "À réaliser" })).toBeVisible();

    // The edit form keeps the number of people; out of bounds is refused.
    await page.getByRole("link", { name: "Modifier la recette" }).click();
    await expect(page.getByLabel("Personnes")).toHaveValue("4");
    await page.getByLabel("Personnes").evaluate((el) => {
      el.removeAttribute("max");
    });
    await page.getByLabel("Personnes").fill("51");
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(
      page.getByText("Le nombre de personnes doit être un nombre entier de 1 à 50."),
    ).toBeVisible();
  });
});

// #812: the day and the meal of a planned recipe, set, changed and removed
// from its page without JavaScript, by a member who did not plan it.
test.describe("Recipes — jour et repas d'une recette planifiée (#812)", () => {
  test("another member dates the recipe, changes its meal, then removes the day", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-rcwhenowner", "When Owner");
    await createGroup(page, "Famille Quand");
    const href = await createInvitationLink(page, "Famille Quand");
    await createRecipe(page, { name: "Gratin du jeudi", ingredients: "Pomme de terre | 4 | pièce" });
    await page.getByRole("button", { name: "À réaliser" }).click();
    await expect(page).toHaveURL(/\?notice=recipe_selected$/);

    const context = await browser.newContext({ javaScriptEnabled: false });
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-rcwhenmember", "When Member");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();
    await openRecipeDetail(member, "Gratin du jeudi");

    // Undated: the form waits in a closed « Choisir un jour ».
    const form = member.locator("form.when-form");
    await expect(form).toBeHidden();
    await expect(member.getByRole("button", { name: "Retirer la date" })).toHaveCount(0);
    await member.getByText("Choisir un jour").click();
    await expect(form.getByText("Pas encore de jour choisi.")).toBeVisible();

    // A meal without a day is refused, and the form opens again on it.
    await form.getByRole("radio", { name: "Midi" }).check();
    await form.getByRole("button", { name: "Enregistrer" }).click();
    await expect(member).toHaveURL(/\?error=meal_without_day$/);
    await expect(member.getByText("Choisissez un jour pour préciser le repas.")).toBeVisible();
    await expect(form).toBeVisible();

    const day = parisDay(3);
    const [, mm, dd] = day.split("-");
    await form.getByLabel("Jour (facultatif)").fill(day);
    await form.getByRole("radio", { name: "Soir" }).check();
    await form.getByRole("button", { name: "Enregistrer" }).click();
    await expect(member).toHaveURL(/\?notice=plan_saved$/);
    await expect(member.getByText("Jour et repas enregistrés.")).toBeVisible();
    await expect(form.locator(".when-state")).toContainText("au dîner");
    await expect(form.getByLabel("Jour (facultatif)")).toHaveValue(day);
    await expect(form.getByRole("radio", { name: "Soir" })).toBeChecked();

    // The list says the day and the meal, in « Prévues » and « Toutes ».
    await member.goto("/recipes?vue=prevues");
    await expect(member.locator(".badge.accent", { hasText: `${dd}/${mm} · soir` })).toBeVisible();
    await member.goto("/recipes");
    await expect(member.locator(".badge.accent", { hasText: `${dd}/${mm} · soir` })).toBeVisible();

    // The meal changes; the day stays.
    await openRecipeDetail(member, "Gratin du jeudi");
    await form.getByRole("radio", { name: "Midi" }).check();
    await form.getByRole("button", { name: "Enregistrer" }).click();
    await expect(member).toHaveURL(/\?notice=plan_saved$/);
    await expect(form.locator(".when-state")).toContainText("au déjeuner");
    await expect(form.getByLabel("Jour (facultatif)")).toHaveValue(day);

    // « Retirer la date » takes the day and the meal, the recipe stays planned.
    await form.getByRole("button", { name: "Retirer la date" }).click();
    await expect(member).toHaveURL(/\?notice=plan_cleared$/);
    await expect(member.getByText("Date retirée : la recette reste à réaliser.")).toBeVisible();
    await expect(member.getByRole("button", { name: "Retirer", exact: true })).toBeVisible();
    await expect(form).toBeHidden();
    await member.goto("/recipes?vue=prevues");
    await expect(member.locator(".badge.accent")).toHaveCount(0);
    await context.close();
  });
});

// #846: the number of people of a planned recipe, changed in its field —
// on change with JavaScript, by « Appliquer » without — from its page and
// from the list; the day and the meal stay.
test.describe("Recipes — personnes d'une recette planifiée (#846)", () => {
  test("the field applies on change, refuses out of bounds and removes the number once emptied", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-rcpeople", "People Planner");
    await createGroup(page, "Famille Convives");
    await createRecipe(page, { name: "Hachis du lundi", ingredients: "Pomme de terre | 4 | pièce" });
    await page.getByLabel("Personnes").fill("4");
    await page.getByRole("button", { name: "À réaliser" }).click();
    await expect(page).toHaveURL(/\?notice=recipe_selected$/);

    // A day and a meal, which none of what follows touches.
    const day = parisDay(2);
    const form = page.locator("form.when-form");
    await page.getByText("Choisir un jour").click();
    await form.getByLabel("Jour (facultatif)").fill(day);
    await form.getByRole("radio", { name: "Soir" }).check();
    await form.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page).toHaveURL(/\?notice=plan_saved$/);

    // The badge says « À réaliser » alone: the number is in the field.
    const people = page.getByRole("spinbutton", { name: "Personnes, Hachis du lundi" });
    await expect(page.locator(".notice .badge")).toHaveText("À réaliser");
    await expect(people).toHaveValue("4");
    await expect(page.getByRole("button", { name: "Appliquer" })).toBeHidden();

    await people.fill("6");
    await people.press("Tab");
    await expect(page).toHaveURL(/\?notice=servings_updated&servings=6$/);
    await expect(page.getByRole("status").filter({ hasText: "Nombre de personnes mis à jour : 6." })).toBeVisible();
    await expect(people).toHaveValue("6");
    await expect(people).toBeFocused();
    await expect(form.getByLabel("Jour (facultatif)")).toHaveValue(day);
    await expect(form.getByRole("radio", { name: "Soir" })).toBeChecked();

    // Out of bounds: the existing message, nothing changed.
    await people.fill("0");
    await people.press("Tab");
    await expect(page).toHaveURL(/\?error=invalid_servings$/);
    await expect(page.getByText("Le nombre de personnes doit être un nombre entier de 1 à 50.")).toBeVisible();
    await expect(people).toHaveValue("6");

    // Emptied: the number goes, the recipe stays planned on its day.
    await people.fill("");
    await people.press("Tab");
    await expect(page).toHaveURL(/\?notice=servings_cleared$/);
    await expect(people).toHaveValue("");
    await expect(people).toHaveAttribute("placeholder", "Optionnel");
    await expect(form.getByLabel("Jour (facultatif)")).toHaveValue(day);

    // From the list, « Toutes » then « Prévues », back to each.
    await page.goto("/recipes");
    const row = page.getByRole("spinbutton", { name: "Personnes, Hachis du lundi" });
    await row.fill("3");
    await row.press("Tab");
    await expect(page).toHaveURL(/\/recipes\?notice=servings_updated&servings=3$/);
    await expect(row).toHaveValue("3");
    await page.goto("/recipes?vue=prevues");
    await row.fill("5");
    await row.press("Tab");
    await expect(page).toHaveURL(/\/recipes\?vue=prevues&notice=servings_updated&servings=5$/);
    await expect(row).toHaveValue("5");
    await expect(page.locator(".badge.accent")).toHaveCount(1);
  });

  test("without JavaScript, « Appliquer » sends the number", async ({ browser }) => {
    const context = await browser.newContext({ javaScriptEnabled: false });
    const page = await context.newPage();
    await registerAndLogin(page, "e2e-rcpeoplenojs", "People Without Script");
    await createGroup(page, "Famille Sans Script");
    await createRecipe(page, { name: "Potée", ingredients: "Chou | 1 | pièce" });
    await page.getByRole("button", { name: "À réaliser" }).click();
    await expect(page).toHaveURL(/\?notice=recipe_selected$/);

    const people = page.getByRole("spinbutton", { name: "Personnes, Potée" });
    await people.fill("8");
    await page.getByRole("button", { name: "Appliquer" }).click();
    await expect(page).toHaveURL(/\?notice=servings_updated&servings=8$/);
    await expect(page.getByText("Nombre de personnes mis à jour : 8.")).toBeVisible();
    await expect(people).toHaveValue("8");
    await context.close();
  });
});

// #539: the recipes in three tabs, each a link (`?vue=`), no JavaScript.
test.describe("Recipes — Toutes, Prévues, Possibles (#539)", () => {
  test("a planned recipe is in « Prévues »; once its ingredients are stocked, in « Possibles »", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-rctabs", "Tabs User");
    await createGroup(page, "Famille Onglets");
    await createRecipe(page, {
      name: "Crêpes",
      ingredients: "Farine | 250 | g\nLait | 500 | ml\nRhum |  |  |  | optionnel",
    });
    await createRecipe(page, { name: "Soupe", ingredients: "Poireau | 2 | pièce" });

    const tabs = page.getByRole("navigation", { name: "Groupes de recettes" });
    await page.goto("/recipes");
    await expect(tabs.getByRole("link", { name: "Toutes" })).toHaveAttribute("aria-current", "true");
    await expect(tabs.getByRole("link", { name: "Prévues · 0" })).toBeVisible();
    await expect(tabs.getByRole("link", { name: "Possibles · 0" })).toBeVisible();

    // Planned from its row of the full list.
    await page
      .locator("li", { hasText: "Crêpes" })
      .filter({ has: page.getByRole("button", { name: "À réaliser" }) })
      .getByRole("button", { name: "À réaliser" })
      .click();
    await expect(page).toHaveURL(/\/recipes\?notice=recipe_selected$/);

    await tabs.getByRole("link", { name: "Prévues · 1" }).click();
    await expect(page).toHaveURL(/\/recipes\?vue=prevues$/);
    await expect(tabs.getByRole("link", { name: "Prévues · 1" })).toHaveAttribute(
      "aria-current",
      "true",
    );
    const planned = page.locator("li", { hasText: "Crêpes" });
    await expect(planned.getByRole("link", { name: "Faite" })).toBeVisible();
    await expect(planned.getByRole("link", { name: "Abandonner" })).toBeVisible();
    await expect(planned.getByRole("button", { name: "Retirer" })).toBeVisible();
    await expect(page.getByRole("link", { name: "Soupe" })).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "Générer les courses de cette recette" }),
    ).toBeVisible();

    // Nothing in stock yet: nothing possible.
    await page.getByRole("link", { name: "Possibles · 0" }).click();
    await expect(page).toHaveURL(/\/recipes\?vue=possibles$/);
    await expect(page.getByText("Aucune recette ne se fait avec le stock actuel.")).toBeVisible();

    // Its required ingredients stocked, the rum (optional) not: possible.
    await createStockItem(page, "Farine", "1", "kg");
    await createStockItem(page, "Lait", "1", "L");
    await page.goto("/recipes?vue=possibles");
    await expect(tabs.getByRole("link", { name: "Possibles · 1" })).toHaveAttribute(
      "aria-current",
      "true",
    );
    const possible = page.locator("li", { hasText: "Crêpes" });
    await expect(possible.getByText("Tous les ingrédients en stock")).toBeVisible();
    await expect(page.getByRole("link", { name: "Soupe" })).toHaveCount(0);
  });

  test("« Retirer » from « Prévues » comes back to « Prévues »", async ({ page }) => {
    await registerAndLogin(page, "e2e-rcback", "Back User");
    await createGroup(page, "Famille Retour");
    await createRecipe(page, { name: "Gratin", ingredients: "Pomme de terre | 1 | kg" });
    await page.goto("/recipes");
    await page
      .locator("li", { hasText: "Gratin" })
      .getByRole("button", { name: "À réaliser" })
      .click();

    await page.goto("/recipes?vue=prevues");
    await page
      .locator("li", { hasText: "Gratin" })
      .getByRole("button", { name: "Retirer" })
      .click();
    await expect(page).toHaveURL(/\/recipes\?vue=prevues&notice=recipe_deselected$/);
    await expect(page.getByText("Recette retirée des recettes à réaliser.")).toBeVisible();
    const tabs = page.getByRole("navigation", { name: "Groupes de recettes" });
    await expect(tabs.getByRole("link", { name: "Prévues · 0" })).toHaveAttribute(
      "aria-current",
      "true",
    );
    await expect(page.getByText("Aucune recette prévue.")).toBeVisible();
  });

  test("the form takes a time, lines to remove, more lines and numbered steps", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-rcform", "Form User");
    await createGroup(page, "Famille Formulaire");

    await page.goto("/recipes/new");
    await page.getByLabel("Nom").fill("Crêpes");
    await page.getByLabel("Temps (minutes)").fill("45");
    await fillIngredients(page, "Farine | 250 | g | 1,2\nSel | 1 | pincée");
    await fillSteps(page, "Mélanger.\nCuire.");
    // Sel taken out, then more lines: nothing is saved yet.
    await page.getByRole("checkbox", { name: "Retirer (ligne 2)", exact: true }).check();
    await page.getByRole("button", { name: "Ajouter des lignes" }).click();
    await expect(page).toHaveURL(/\/recipes\/new$/);
    await expect(lineField(page, "Ingrédient", 1)).toHaveValue("Farine");
    await expect(lineField(page, "Saison", 1)).toHaveValue("1,2");
    await expect(lineField(page, "Ingrédient", 2)).toHaveValue("");
    await expect(page.getByLabel("Temps (minutes)")).toHaveValue("45");
    await lineField(page, "Ingrédient", 7).fill("Lait");
    await lineField(page, "Quantité", 7).fill("0,5");
    await lineField(page, "Unité", 7).fill("L");
    await page.getByRole("button", { name: "Créer la recette" }).click();
    await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\?notice=recipe_created$/);

    await expect(page.getByText("Temps : 45 min.")).toBeVisible();
    await expect(page.getByText("saison : 1, 2")).toBeVisible();
    const ingredients = page.locator("h2:text-is('Ingrédients') ~ ul").first().locator("li strong");
    await expect(ingredients).toHaveText(["Farine", "Lait"]);
    await expect(page.locator("h2:text-is('Étapes') + ol li")).toHaveText(["Mélanger.", "Cuire."]);

    // The edit form brings it all back, seasonal months included.
    await page.getByRole("link", { name: "Modifier la recette" }).click();
    await expect(page.getByLabel("Temps (minutes)")).toHaveValue("45");
    await expect(lineField(page, "Ingrédient", 1)).toHaveValue("Farine");
    await expect(lineField(page, "Saison", 1)).toHaveValue("1,2");
    await expect(lineField(page, "Ingrédient", 2)).toHaveValue("Lait");
    await expect(lineField(page, "Quantité", 2)).toHaveValue("0.5");
    await expect(page.getByRole("textbox", { name: /^Étape 2/ })).toHaveValue("Cuire.");
  });
});

/** Plans `name` from its row of the full list, for its own base. */
async function planFromList(page: Page, name: string): Promise<void> {
  await page.goto("/recipes");
  await page
    .locator("li", { hasText: name })
    .filter({ has: page.getByRole("button", { name: "À réaliser" }) })
    .getByRole("button", { name: "À réaliser" })
    .click();
  await expect(page).toHaveURL(/\/recipes\?notice=recipe_selected$/);
}

// #560: what the stock lacks of a planned recipe, and by when to cook it.
test.describe("Recipes — Prévues : manquants et priorité (#560)", () => {
  test("a stock lowered under the recipe's need: badge, dialog, then onto the grocery list", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-rcmiss", "Missing User");
    await createGroup(page, "Famille Manquants");
    await createRecipe(page, { name: "Crêpes", ingredients: "Lait | 500 | ml" });
    await createStockItem(page, "Lait", "500", "ml");
    await planFromList(page, "Crêpes");

    // Everything in stock: no warning.
    await page.goto("/recipes?vue=prevues");
    const row = page.locator("li", { hasText: "Crêpes" });
    await expect(row.getByRole("link", { name: "Faite" })).toBeVisible();
    await expect(page.getByText(/Il manque/)).toHaveCount(0);

    // One ml less: the stocks say a planned recipe is now incomplete.
    await page.goto("/stocks");
    await page.getByRole("button", { name: "Retirer 1 à Lait" }).click();
    await expect(page).toHaveURL(/\/stocks\?notice=quantity_adjusted$/);
    await expect(page.getByText("1 recette prévue incomplète.")).toBeVisible();
    await page.getByRole("link", { name: "Voir les recettes prévues" }).click();
    await expect(page).toHaveURL(/\/recipes\?vue=prevues$/);

    // The badge opens the list, focus inside; Escape gives it back.
    const badge = row.getByRole("link", { name: "Il manque 1 élément" });
    await badge.click();
    const dialog = page.getByRole("dialog", { name: "Il manque à « Crêpes »" });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByRole("listitem")).toHaveText(["Lait — 1 ml"]);
    await expect(dialog.getByRole("button", { name: "Ajouter à la liste de courses" })).toBeFocused();
    // Centred in the viewport, on a desktop and on a phone: the row it sits
    // in strips its children's margins, the browser's `margin: auto` with them.
    const initialViewport = page.viewportSize();
    for (const size of [
      { width: 1280, height: 800 },
      { width: 390, height: 844 },
    ]) {
      await page.setViewportSize(size);
      const box = await dialog.boundingBox();
      expect(box).not.toBeNull();
      const left = box!.x;
      const right = size.width - box!.x - box!.width;
      const top = box!.y;
      const bottom = size.height - box!.y - box!.height;
      expect(left).toBeGreaterThan(0);
      expect(top).toBeGreaterThan(0);
      expect(Math.abs(left - right)).toBeLessThanOrEqual(1);
      expect(Math.abs(top - bottom)).toBeLessThanOrEqual(1);
    }
    if (initialViewport) await page.setViewportSize(initialViewport);
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
    await expect(badge).toBeFocused();
    // Still a recipe one can mark done.
    await expect(row.getByRole("link", { name: "Faite" })).toBeVisible();

    await badge.click();
    await dialog.getByRole("button", { name: "Ajouter à la liste de courses" }).click();
    await expect(page).toHaveURL(/\/recipes\?vue=prevues&notice=missing_added$/);
    await expect(page.getByText("Éléments manquants ajoutés à la liste de courses.")).toBeVisible();

    await page.goto("/grocery-list");
    await expect(page.getByText("Lait", { exact: true })).toBeVisible();
  });

  test("a stock needed by tomorrow puts its recipe first, with the date", async ({ page }) => {
    await registerAndLogin(page, "e2e-rcprio", "Priority User");
    await createGroup(page, "Famille Urgence");
    await createRecipe(page, { name: "Soupe", ingredients: "Poireau | 2 | pièce" });
    await createRecipe(page, { name: "Gratin", ingredients: "Crème | 1 | pot" });
    await page.goto("/stocks/new");
    await page.getByLabel("Nom").fill("Crème");
    await page.getByLabel("Quantité").fill("1");
    await page.getByLabel("Unité", { exact: true }).fill("pot");
    const tomorrow = parisDay(1);
    await page.getByLabel(/^Date de péremption/).fill(tomorrow);
    await page.getByRole("button", { name: "Ajouter l'article" }).click();
    await expect(page).toHaveURL(/\/stocks\?notice=item_created$/);
    // The undated one planned first.
    await planFromList(page, "Soupe");
    await planFromList(page, "Gratin");

    await page.goto("/recipes?vue=prevues");
    const [, m, d] = tomorrow.split("-");
    const rows = page.locator("ul.list > li");
    await expect(rows.first()).toContainText("Gratin");
    await expect(rows.first().getByText(`À faire avant le ${d}/${m} (sous 1 jour)`)).toBeVisible();
    await expect(rows.nth(1)).toContainText("Soupe");
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
    await skipMealStart(context);
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-rcmember", "Perm Member");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    // The member sees the log form but no edit/delete controls.
    await openRecipeDetail(member, "Gratin du proprio");
    await expect(
      member.getByText("Seuls le créateur, les administrateurs et le propriétaire peuvent modifier ou supprimer"),
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

// #405: import a recipe from a page's address. The nominal path (a real
// page read, then the pre-filled form) is covered by
// apps/api/tests/recipe_import_flow.rs against a local site: this suite
// has no recipe site to point at, and the server refuses every local or
// private address by design — which is what the first test checks.
test.describe("Recipes — import from a URL", () => {
  test("a local address is refused with a clear message, the address kept", async ({ page }) => {
    await registerAndLogin(page, "e2e-rcimport", "Import User");
    await createGroup(page, "Famille Import");

    await page.goto("/recipes");
    await page.getByRole("link", { name: "Importer depuis une URL" }).click();
    await expect(page).toHaveURL(/\/recipes\/import$/);
    await expect(page.getByRole("heading", { name: "Importer une recette", level: 1 })).toBeVisible();

    const address = page.getByRole("textbox", { name: "Adresse de la page" });
    await address.fill("http://127.0.0.1/recette-locale");
    await page.getByRole("button", { name: "Importer", exact: true }).click();

    await expect(
      page.getByText("Cette adresse mène à un réseau local ou privé : elle ne peut pas être importée."),
    ).toBeVisible();
    await expect(address).toHaveValue("http://127.0.0.1/recette-locale");
    // Nothing was created.
    await page.goto("/recipes");
    await expect(page.getByText("Aucune recette pour le moment.")).toBeVisible();
  });

  test("a recipe created with its source links to the page it came from", async ({ page }) => {
    await registerAndLogin(page, "e2e-rcsource", "Source User");
    await createGroup(page, "Famille Origine");

    // The import by link opens the page (#539), its own form.
    await page.goto("/recipes/new");
    await expect(
      page.locator('form[action="/recipes/import"]').getByRole("textbox", {
        name: "Importer depuis un lien",
      }),
    ).toBeVisible();
    // The create form as the import leaves it: the page's address in a
    // hidden `source_url` field, next to the fields the member reviews.
    const source = "https://www.marmiton.org/recettes/recette_crepes-faciles_27121.aspx";
    await page.locator('form[action="/recipes/new"]').evaluate((form, url) => {
      const field = document.createElement("input");
      field.type = "hidden";
      field.name = "source_url";
      field.value = url;
      form.appendChild(field);
    }, source);
    await page.getByLabel("Nom").fill("Crêpes importées");
    await page.getByRole("button", { name: "Créer la recette" }).click();
    await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\?notice=recipe_created$/);

    const link = page.getByRole("link", { name: "marmiton.org" });
    await expect(link).toHaveAttribute("href", source);
    await expect(link).toHaveAttribute("rel", "noopener noreferrer nofollow");

    // A recipe typed by hand shows no source.
    await createRecipe(page, { name: "Crêpes maison" });
    await expect(page.getByText("Source :")).toHaveCount(0);
  });
});

// #813: the planning of the meals, without JavaScript — a slot filled from
// its « Ajouter », an undated recipe placed from « À placer », the window
// moved by seven days.
test.describe("Recipes — planning des repas (#813)", () => {
  test("a meal added from an empty slot, another placed, the window moved", async ({ browser }) => {
    const context = await browser.newContext({ javaScriptEnabled: false });
    const page = await context.newPage();
    await registerAndLogin(page, "e2e-rcweek", "Week User");
    await createGroup(page, "Famille Semainier");
    await createRecipe(page, { name: "Velouté de potiron", ingredients: "Potiron | 1 | pièce" });
    await createRecipe(page, { name: "Tarte fine" });

    // Reached from the recipes' filters, lit there; nothing planned yet.
    await page.goto("/recipes");
    const filters = page.getByRole("navigation", { name: "Groupes de recettes" });
    await filters.getByRole("link", { name: "Planning" }).click();
    await expect(page).toHaveURL(/\/recipes\/week$/);
    await expect(page.getByRole("heading", { name: "Planning des repas", level: 1 })).toBeVisible();
    await expect(filters.getByRole("link", { name: "Planning" })).toHaveAttribute("aria-current", "true");
    await expect(page.getByText("Aucun repas planifié cette semaine.")).toBeVisible();
    const days = page.locator("ol.meals > li");
    await expect(days).toHaveCount(9);
    await expect(page.locator("ol.meals .empty a")).toHaveCount(18);
    const today = page.locator("ol.meals > li.current");
    await expect(today.getByText("Aujourd'hui")).toBeVisible();

    // « Ajouter » on today's soir: the list names the slot, « À réaliser »
    // plans the recipe on it and comes back to the planning.
    await today.locator(`a[href="/recipes?date=${parisDay(0)}&repas=soir"]`).click();
    await expect(page.getByText("« À réaliser » sur une recette la place dans le planning.")).toBeVisible();
    await page
      .locator("li", { hasText: "Velouté de potiron" })
      .filter({ has: page.getByRole("button", { name: "À réaliser" }) })
      .getByRole("button", { name: "À réaliser" })
      .click();
    await expect(page).toHaveURL(/\/recipes\/week\?notice=plan_saved$/);
    const soir = today.locator("li", { hasText: "Soir" });
    await expect(soir.getByRole("link", { name: "Velouté de potiron", exact: true })).toBeVisible();
    await expect(soir.getByRole("link", { name: "Il manque 1 élément" })).toBeVisible();
    await expect(page.locator(".summary")).toHaveText(/1 repas planifié\s*1 ingrédient à acheter/);
    await expect(page.locator("ol.meals .empty a")).toHaveCount(17);

    // Planned without a day: under « À placer », placed ten days ahead at
    // midi, then seen on the window that shows that day.
    await planFromList(page, "Tarte fine");
    await page.goto("/recipes/week");
    const toPlace = page.locator("section", { has: page.getByRole("heading", { name: "À placer" }) });
    await toPlace.getByText("Choisir un jour").click();
    await toPlace.getByLabel("Jour (facultatif)").fill(parisDay(10));
    await toPlace.getByRole("radio", { name: "Midi" }).check();
    await toPlace.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page).toHaveURL(new RegExp(`/recipes/week\\?date=${parisDay(6)}&notice=plan_saved$`));
    await expect(page.getByText("Jour et repas enregistrés.")).toBeVisible();
    await expect(page.getByRole("heading", { name: "À placer" })).toHaveCount(0);
    await expect(days).toHaveCount(7);
    await expect(page.locator("ol.meals").getByRole("link", { name: "Tarte fine", exact: true })).toBeVisible();

    // ‹ comes back to the current window, › goes seven days further.
    await page.getByRole("link", { name: "7 jours précédents" }).click();
    await expect(page).toHaveURL(/\/recipes\/week$/);
    await expect(days).toHaveCount(9);
    await page.getByRole("link", { name: "7 jours suivants" }).click();
    await expect(page).toHaveURL(new RegExp(`/recipes/week\\?date=${parisDay(6)}$`));

    // A date that does not read is the current window.
    await page.goto("/recipes/week?date=demain");
    await expect(days).toHaveCount(9);
    await expect(page.locator("ol.meals > li.current")).toHaveCount(1);
    await context.close();
  });
});

// #816: « Changer ce plat », without JavaScript — from the planning, an
// offer chosen, back on the planning with the new dish on the same slot;
// then, from the recipe's page, no offer left, and nothing changed.
test.describe("Recipes — changer ce plat (#816)", () => {
  test("a dish replaced from the planning keeps its slot; none left is said", async ({ browser }) => {
    const context = await browser.newContext({ javaScriptEnabled: false });
    const page = await context.newPage();
    await registerAndLogin(page, "e2e-rcswap", "Swap User");
    await createGroup(page, "Famille Échange");
    await createRecipe(page, { name: "Gratin du soir", ingredients: "Pommes de terre | 1 | kg" });
    await createRecipe(page, { name: "Soupe du jour", ingredients: "Potiron | 1 | pièce" });

    // Gratin planned on today's soir from its « Ajouter ».
    await page.goto("/recipes/week");
    const today = page.locator("ol.meals > li.current");
    await today.locator(`a[href="/recipes?date=${parisDay(0)}&repas=soir"]`).click();
    await page
      .locator("li", { hasText: "Gratin du soir" })
      .filter({ has: page.getByRole("button", { name: "À réaliser" }) })
      .getByRole("button", { name: "À réaliser" })
      .click();
    await expect(page).toHaveURL(/\/recipes\/week\?notice=plan_saved$/);

    // « Changer ce plat »: what is replaced, and the soup offered.
    await today.getByRole("link", { name: /^Changer ce plat : Gratin du soir, .+, soir$/ }).click();
    await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\/replace\?back=week$/);
    await expect(page.getByRole("heading", { name: "Changer ce plat", level: 1 })).toBeVisible();
    const context816 = page.locator("section.ctx");
    await expect(context816.getByText("Vous remplacez")).toBeVisible();
    await expect(context816.getByText("Gratin du soir")).toBeVisible();
    await expect(context816.getByText("Le jour, le repas et le nombre de personnes sont conservés.")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Proposition", level: 2 })).toBeVisible();
    const offer = page.locator("li.prop", { hasText: "Soupe du jour" });
    await expect(offer.getByText("Il manque 1 élément")).toBeVisible();

    // Chosen: back on the planning, the soup on the same slot.
    await offer.getByRole("button", { name: "Choisir Soupe du jour" }).click();
    await expect(page).toHaveURL(/\/recipes\/week\?notice=recipe_replaced&from=[0-9a-f-]+&to=[0-9a-f-]+$/);
    await expect(page.getByText("Gratin du soir remplacé par Soupe du jour.")).toBeVisible();
    const soir = today.locator("li", { hasText: "Soir" });
    await expect(soir.getByRole("link", { name: "Soupe du jour", exact: true })).toBeVisible();
    await expect(page.locator("ol.meals").getByRole("link", { name: "Gratin du soir", exact: true })).toHaveCount(0);

    // The gratin planned again: from the soup's page, no offer is left.
    await planFromList(page, "Gratin du soir");
    await openRecipeDetail(page, "Soupe du jour");
    await page.getByRole("link", { name: "Changer ce plat" }).click();
    await expect(page.getByText("Aucune autre recette à proposer pour l'instant.")).toBeVisible();
    await expect(page.getByRole("button", { name: /^Choisir/ })).toHaveCount(0);
    await page.getByRole("link", { name: "Annuler" }).click();
    await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+$/);
    await expect(page.getByRole("heading", { name: "Soupe du jour", level: 1 })).toBeVisible();
    await page.goto("/recipes/week");
    await expect(soir.getByRole("link", { name: "Soupe du jour", exact: true })).toBeVisible();
    await context.close();
  });
});

// #910: the library shared by every family (API #906, #907). The
// publication is written straight into the database (`publishToLibrary`):
// the screen that publishes is #911's.
test.describe("Recipes — bibliothèque (#910)", () => {
  test("search, open, add, then find the copy in « Toutes les recettes »", async ({ page }) => {
    const email = await registerAndLogin(page, "e2e-rclib", "Library User");
    await createGroup(page, "Famille Partage");
    const token = `Biblio${Date.now()}${Math.floor(Math.random() * 1e6)}`;
    const name = `Gratin ${token}`;
    const id = await publishToLibrary(email, {
      name,
      instructions: "Préchauffer le four.\nCuire une heure.",
      servings: 4,
      ingredients: ["Pommes de terre", "Crème"],
    });

    // The way in: « Bibliothèque » in the header of the recipes.
    await page.goto("/recipes");
    await page.getByRole("link", { name: "Bibliothèque", exact: true }).click();
    await expect(page).toHaveURL(/\/recipes\/library$/);
    await expect(page.getByRole("heading", { name: "Bibliothèque", level: 1 })).toBeVisible();

    // A search that finds nothing says so, and offers to start again.
    await page.getByLabel("Rechercher une recette").fill(`${token}zz`);
    await page.getByRole("button", { name: "Rechercher" }).click();
    await expect(page.getByText(`0 résultat pour « ${token}zz ».`)).toBeVisible();
    await expect(page.getByRole("heading", { name: "Aucune recette ne correspond" })).toBeVisible();
    await expect(page.getByRole("link", { name: "Effacer la recherche" })).toHaveAttribute(
      "href",
      "/recipes/library",
    );

    // The search by name, case aside, finds it; its count is said.
    await page.getByLabel("Rechercher une recette").fill(token.toLowerCase());
    await page.getByRole("button", { name: "Rechercher" }).click();
    await expect(page.getByText(`1 résultat pour « ${token.toLowerCase()} »`)).toBeVisible();
    await expect(page.getByText("Page 1 sur 1")).toBeVisible();
    const row = page.getByRole("link", { name: new RegExp(name) });
    await expect(row).toContainText("4 personnes · 2 ingrédients");
    await expect(row).not.toContainText("Issue de la bibliothèque");
    await row.click();

    // Its page: what it holds.
    await expect(page).toHaveURL(new RegExp(`/recipes/library/${id}$`));
    await expect(page.getByRole("heading", { name, level: 1 })).toBeVisible();
    await expect(page.getByText("Pour 4 personnes")).toBeVisible();
    await expect(page.getByText("Pommes de terre")).toBeVisible();
    await expect(page.getByText("Cuire une heure.")).toBeVisible();

    await page.getByRole("button", { name: "Ajouter à mes recettes" }).click();
    await expect(page).toHaveURL(new RegExp(`/recipes/library/${id}\\?notice=added$`));
    await expect(page.getByText("Recette ajoutée à vos recettes.")).toBeVisible();
    // Once copied, no second « Ajouter »: the copy instead.
    await expect(page.getByRole("button", { name: "Ajouter à mes recettes" })).toHaveCount(0);
    await expect(page.getByText("Cette recette est déjà dans vos recettes.")).toBeVisible();
    await page.getByRole("link", { name: "Voir ma copie" }).click();
    await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]{36}$/);
    await expect(page.getByRole("heading", { name, level: 1 })).toBeVisible();
    const copyUrl = page.url();

    // The list marks the copy.
    await page.goto(`/recipes/library?q=${token}`);
    await expect(page.getByRole("link", { name: new RegExp(name) })).toContainText(
      "Issue de la bibliothèque",
    );

    // The family has it among its own recipes.
    await openRecipeDetail(page, name);

    // Withdrawn: its page says so; the copy stays whole, without a word of it.
    await withdrawFromLibrary(id);
    const response = await page.goto(`/recipes/library/${id}`);
    expect(response?.status()).toBe(404);
    await expect(
      page.getByText("Cette recette a été retirée de la bibliothèque et ne peut plus être ajoutée."),
    ).toBeVisible();
    await expect(page.getByRole("button", { name: "Ajouter à mes recettes" })).toHaveCount(0);
    await openRecipeDetail(page, name);
    expect(page.url()).toBe(copyUrl);
    await expect(page.getByText("Pommes de terre")).toBeVisible();
    await expect(page.getByText("Cuire une heure.")).toBeVisible();
    await expect(page.getByText(/retirée/)).toHaveCount(0);
  });
});

// #911: publishing from the recipe's page, the consent asked first (and
// nothing published without it, without JavaScript too); another account
// reads the publication and its source, then reports it; the publisher
// updates it, withdraws it from the recipe, publishes it again and
// withdraws it from « Mes publications ».
test.describe("Recipes — publier et signaler (#911)", () => {
  test("publish with consent, read and report from another account, withdraw", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-rcpub", "Publish Author");
    await createGroup(page, "Famille Partageuse");
    const token = `Pub${Date.now()}${Math.floor(Math.random() * 1e6)}`;
    const name = `Tarte ${token}`;
    // An imported recipe: the page's address in the create form's hidden
    // `source_url`, as the import leaves it.
    const source = "https://www.marmiton.org/recettes/recette_tarte-aux-poireaux_1.aspx";
    await page.goto("/recipes/new");
    await page.locator('form[action="/recipes/new"]').evaluate((form, url) => {
      const field = document.createElement("input");
      field.type = "hidden";
      field.name = "source_url";
      field.value = url;
      form.appendChild(field);
    }, source);
    await page.getByLabel("Nom").fill(name);
    await fillSteps(page, "Étaler la pâte.\nCuire 40 minutes.");
    await page.getByRole("button", { name: "Créer la recette" }).click();
    await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\?notice=recipe_created$/);
    const recipeUrl = page.url().replace(/\?.*$/, "");

    // The card of the recipe offers to publish it.
    const card = page.locator("#bibliotheque");
    await expect(card.getByRole("heading", { name: "Bibliothèque" })).toBeVisible();
    await card.getByRole("link", { name: "Publier dans la bibliothèque" }).click();
    await expect(page).toHaveURL(`${recipeUrl}/publish`);
    await expect(
      page.getByRole("heading", { name: "Publier dans la bibliothèque", level: 1 }),
    ).toBeVisible();
    await expect(page.getByText("(ici : marmiton.org)")).toBeVisible();
    await expect(page.getByText("Votre nom, votre foyer, votre photo")).toBeVisible();

    // Without JavaScript, the box left unticked: nothing is published.
    const bare = await browser.newContext({
      javaScriptEnabled: false,
      storageState: await page.context().storageState(),
    });
    const noJs = await bare.newPage();
    await noJs.goto(`${recipeUrl}/publish`);
    const [refused] = await Promise.all([
      noJs.waitForResponse((r) => r.url().endsWith("/publish") && r.request().method() === "POST"),
      noJs.getByRole("button", { name: "Publier", exact: true }).click(),
    ]);
    expect(refused.status()).toBe(422);
    await expect(
      noJs.getByText("Rien n'a été publié : cochez la case de consentement pour continuer."),
    ).toBeVisible();
    await expect(noJs.getByText("Cochez cette case pour publier.")).toBeVisible();
    await noJs.goto("/recipes/library/mine");
    await expect(noJs.getByRole("heading", { name: "Aucune publication" })).toBeVisible();
    await bare.close();

    // Ticked: published, back on the recipe.
    await page.getByRole("checkbox", { name: /Je confirme que cette recette/ }).check();
    await page.getByRole("button", { name: "Publier", exact: true }).click();
    await expect(page).toHaveURL(`${recipeUrl}?notice=published`);
    await expect(page.getByText("Recette publiée dans la bibliothèque.")).toBeVisible();
    await expect(card.getByText("Publiée", { exact: true })).toBeVisible();
    await expect(card.getByRole("button", { name: "Retirer de la bibliothèque" })).toBeVisible();

    // Another account, in another family, reads it — its source, nothing of
    // its author — and reports it.
    const readerContext = await browser.newContext();
    await skipMealStart(readerContext);
    const reader = await readerContext.newPage();
    await registerAndLogin(reader, "e2e-rcpubreader", "Library Reader");
    await createGroup(reader, "Famille Lectrice");
    await reader.goto(`/recipes/library?q=${token}`);
    await reader.getByRole("link", { name: new RegExp(name) }).click();
    await expect(reader).toHaveURL(/\/recipes\/library\/[0-9a-f-]{36}$/);
    const publication = reader.url();
    await expect(reader.getByRole("heading", { name, level: 1 })).toBeVisible();
    await expect(reader.getByText("Cuire 40 minutes.")).toBeVisible();
    await expect(reader.getByRole("link", { name: "marmiton.org" })).toHaveAttribute(
      "href",
      source,
    );
    await expect(reader.getByText("Publish Author")).toHaveCount(0);
    await expect(reader.getByText("Famille Partageuse")).toHaveCount(0);

    await reader.getByRole("link", { name: "Signaler cette recette" }).click();
    await expect(reader).toHaveURL(`${publication}/report`);
    await expect(reader.getByRole("radio")).toHaveCount(6);
    // No reason, then « Autre » without a comment: said, nothing sent.
    await reader.getByRole("button", { name: "Envoyer le signalement" }).click();
    await expect(reader.getByText("Choisissez un motif.")).toBeVisible();
    await reader.getByRole("radio", { name: "Autre (commentaire obligatoire)" }).check();
    await reader.getByRole("button", { name: "Envoyer le signalement" }).click();
    await expect(reader.getByText("Précisez le problème dans le commentaire.")).toBeVisible();
    await expect(
      reader.getByRole("radio", { name: "Autre (commentaire obligatoire)" }),
    ).toBeChecked();
    await reader.getByRole("textbox", { name: /Commentaire/ }).fill("Recette en double.");
    await reader.getByRole("button", { name: "Envoyer le signalement" }).click();
    await expect(reader).toHaveURL(`${publication}?notice=reported`);
    await expect(reader.getByText("Merci, votre signalement a été transmis.")).toBeVisible();
    await expect(reader.getByText(/Vous avez signalé cette recette le/)).toBeVisible();
    await expect(reader.getByRole("link", { name: "Signaler cette recette" })).toHaveCount(0);

    // The publisher updates the publication: the consent asked again.
    await page.goto(recipeUrl);
    await card.getByRole("link", { name: "Mettre à jour la publication" }).click();
    await expect(
      page.getByRole("heading", { name: "Mettre à jour la publication", level: 1 }),
    ).toBeVisible();
    await page.getByRole("checkbox", { name: /Je confirme à nouveau/ }).check();
    await page.getByRole("button", { name: "Mettre à jour", exact: true }).click();
    await expect(page).toHaveURL(`${recipeUrl}?notice=publication_updated`);

    // Withdrawn from the recipe, in one click.
    await card.getByRole("button", { name: "Retirer de la bibliothèque" }).click();
    await expect(page).toHaveURL(`${recipeUrl}?notice=unpublished`);
    await expect(page.getByText("Recette retirée de la bibliothèque.")).toBeVisible();
    await expect(card.getByText("Cette recette n'est plus dans la bibliothèque.")).toBeVisible();
    const gone = await reader.goto(publication);
    expect(gone?.status()).toBe(404);

    // Published again, then withdrawn from « Mes publications ».
    await card.getByRole("link", { name: "Publier à nouveau" }).click();
    await page.getByRole("checkbox", { name: /Je confirme que cette recette/ }).check();
    await page.getByRole("button", { name: "Publier", exact: true }).click();
    await expect(page).toHaveURL(`${recipeUrl}?notice=published`);
    await card.getByRole("link", { name: "Mes publications" }).click();
    await expect(page).toHaveURL(/\/recipes\/library\/mine$/);
    await expect(page.getByRole("heading", { name: "Mes publications", level: 1 })).toBeVisible();
    await expect(page.getByRole("heading", { name, level: 2 })).toBeVisible();
    await page.getByRole("button", { name: `Retirer ${name} de la bibliothèque` }).click();
    await expect(page).toHaveURL(/\/recipes\/library\/mine\?notice=unpublished$/);
    await expect(page.getByText("Recette retirée de la bibliothèque.")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Aucune publication" })).toBeVisible();
    await reader.goto(`/recipes/library?q=${token}`);
    await expect(
      reader.getByRole("heading", { name: "Aucune recette ne correspond" }),
    ).toBeVisible();
    await readerContext.close();
  });

  // A publication another member made (#991): the family's owner reads its
  // card on the publish screen, « Retirer » with it; its publisher, made a
  // standard member below the `recipes_edit` threshold, still sees it on
  // the recipe and withdraws it, without being offered to publish again.
  test("an owner meets another member's publication; its publisher, demoted, withdraws it", async ({
    page,
    browser,
  }) => {
    const family = `Famille Retrait ${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
    await registerAndLogin(page, "e2e-rcpubowner", "Retrait Owner");
    await createGroup(page, family);
    const href = await createInvitationLink(page, family);
    const name = `Quiche ${Date.now()}${Math.floor(Math.random() * 1e6)}`;
    await createRecipe(page, { name, ingredients: "Oeuf | 3 | pièce" });
    const recipeUrl = page.url().replace(/\?.*$/, "");

    const context = await browser.newContext();
    await skipMealStart(context);
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-rcpubadmin", "Retrait Admin");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    const setRole = async (role: string) => {
      await page.goto("/groups");
      await page.locator("li", { hasText: family }).getByRole("link", { name: "Membres" }).click();
      const row = page.locator("li", { hasText: "Retrait Admin" });
      await row.locator("select[name=role]").selectOption(role);
      await row.getByRole("button", { name: "Changer le rôle" }).click();
      await expect(page.getByText("Rôle mis à jour.")).toBeVisible();
    };

    // An admin, they publish the owner's recipe.
    await setRole("admin");
    await member.goto(`${recipeUrl}/publish`);
    await member.getByRole("checkbox", { name: /Je confirme que cette recette/ }).check();
    await member.getByRole("button", { name: "Publier", exact: true }).click();
    await expect(member).toHaveURL(`${recipeUrl}?notice=published`);

    // The owner tries too: the card of the publication, « Retirer » on it.
    await page.goto(`${recipeUrl}/publish`);
    await page.getByRole("checkbox", { name: /Je confirme que cette recette/ }).check();
    const [already] = await Promise.all([
      page.waitForResponse((r) => r.url().endsWith("/publish") && r.request().method() === "POST"),
      page.getByRole("button", { name: "Publier", exact: true }).click(),
    ]);
    expect(already.status()).toBe(409);
    await expect(page.getByText("déjà publiée dans la bibliothèque par un autre membre")).toBeVisible();
    const alreadyCard = page.locator("section.card.already");
    await expect(alreadyCard.getByRole("heading", { name, level: 2 })).toBeVisible();
    await expect(alreadyCard.getByText("Publiée", { exact: true })).toBeVisible();
    await expect(alreadyCard.getByRole("button", { name: "Retirer de la bibliothèque" })).toBeVisible();

    // Made a standard member: the recipe is no longer theirs to edit, but
    // their publication still shows, and goes in one click.
    await setRole("standard");
    await member.goto(recipeUrl);
    const card = member.locator("#bibliotheque");
    await expect(card.getByText("Publiée", { exact: true })).toBeVisible();
    await expect(card.getByRole("link", { name: "Mettre à jour la publication" })).toHaveCount(0);
    await expect(member.getByRole("link", { name: "Modifier la recette" })).toHaveCount(0);
    await card.getByRole("button", { name: "Retirer de la bibliothèque" }).click();
    await expect(member).toHaveURL(`${recipeUrl}?notice=unpublished`);
    await expect(member.getByText("Recette retirée de la bibliothèque.")).toBeVisible();
    await expect(card.getByText("Cette recette n'est plus dans la bibliothèque.")).toBeVisible();
    await expect(card.getByRole("link", { name: "Publier à nouveau" })).toHaveCount(0);
    await member.goto(`/recipes/library?q=${encodeURIComponent(name)}`);
    await expect(member.getByRole("heading", { name: "Aucune recette ne correspond" })).toBeVisible();
    await context.close();
  });

  // A publication outlives its family: its publisher, in no family any
  // more, still reads « Mes publications » and the library (#990), and
  // withdraws it from there.
  test("the library without a family: read, reported, listed and withdrawn", async ({ page }) => {
    const email = await registerAndLogin(page, "e2e-rcpubalone", "Lone Publisher");
    const name = `Gratin ${Date.now()}${Math.floor(Math.random() * 1e6)}`;
    await publishToLibrary(email, { name, ingredients: ["pommes de terre"] });
    const listed = await page.goto("/recipes/library/mine");
    expect(listed?.status()).toBe(200);
    await expect(page).toHaveURL(/\/recipes\/library\/mine$/);
    await expect(page.getByRole("heading", { name: "Mes publications", level: 1 })).toBeVisible();
    await expect(page.getByRole("heading", { name, level: 2 })).toBeVisible();
    await expect(page.getByText("Cette recette n'est plus dans vos recettes")).toBeVisible();
    // Without a family, both kitchen tabs would redirect: no segment at all.
    await expect(page.locator("nav.segment")).toHaveCount(0);

    // The library reads without a family too (#990): its list, the
    // publication and its report open, and nothing leads to the family's
    // recipes, which would only redirect to /groups/new.
    await page.getByRole("link", { name: `Voir la fiche de ${name} dans la bibliothèque` }).click();
    await expect(page).toHaveURL(/\/recipes\/library\/[0-9a-f-]{36}$/);
    const publication = page.url();
    await expect(page.getByRole("heading", { name, level: 1 })).toBeVisible();
    await expect(page.getByText("pommes de terre")).toBeVisible();
    await expect(page.getByRole("button", { name: "Ajouter à mes recettes" })).toHaveCount(0);
    await expect(page.getByText("Ajouter cette recette à un groupe")).toBeVisible();
    await expect(page.locator('main a[href="/recipes"]')).toHaveCount(0);
    await expect(page.locator("nav.segment")).toHaveCount(0);
    await page.getByRole("link", { name: "Signaler cette recette" }).click();
    await expect(page).toHaveURL(`${publication}/report`);
    await expect(page.getByRole("radio")).toHaveCount(6);
    await expect(page.locator("nav.segment")).toHaveCount(0);
    await page.goto(publication);
    await page.getByRole("link", { name: "Ajouter à mon groupe" }).click();
    await expect(page).toHaveURL(/\/groups\/new$/);
    await expect(page.getByRole("heading", { name: "Créer un groupe", level: 1 })).toBeVisible();

    const list = await page.goto(`/recipes/library?q=${encodeURIComponent(name)}`);
    expect(list?.status()).toBe(200);
    await expect(page).toHaveURL(/\/recipes\/library\?q=/);
    await expect(page.getByRole("heading", { name: "Bibliothèque", level: 1 })).toBeVisible();
    await expect(page.getByRole("link", { name: new RegExp(name) })).toHaveAttribute(
      "href",
      new URL(publication).pathname,
    );
    await expect(page.getByText("Issue de la bibliothèque")).toHaveCount(0);
    await expect(page.locator('main a[href="/recipes"]')).toHaveCount(0);
    await expect(page.locator("nav.segment")).toHaveCount(0);
    await page.getByRole("link", { name: "Mes publications" }).click();
    await expect(page).toHaveURL(/\/recipes\/library\/mine$/);

    await page.getByRole("button", { name: `Retirer ${name} de la bibliothèque` }).click();
    await expect(page).toHaveURL(/\/recipes\/library\/mine\?notice=unpublished$/);
    await expect(page.getByText("Recette retirée de la bibliothèque.")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Aucune publication" })).toBeVisible();
    await expect(page.getByRole("link", { name: "Voir mes recettes" })).toHaveCount(0);
    // A forged URL names nothing: the name is never read from it.
    await page.goto("/recipes/library/mine?notice=unpublished&nom=Arnaque");
    await expect(page.getByText("Recette retirée de la bibliothèque.")).toBeVisible();
    await expect(page.getByText("Arnaque")).toHaveCount(0);
  });
});
