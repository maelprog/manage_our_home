import { expect, Page, test } from "@playwright/test";
import { parisDay } from "../lib/dates";
import { fetchVerificationToken } from "../lib/db";
import { fillIngredients, fillSteps, lineField } from "../lib/recipe-form";

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
    await expect(page.getByText("À réaliser · 2 personnes")).toBeVisible();

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
    await page.getByLabel("Unité").fill("pot");
    const tomorrow = parisDay(1);
    await page.getByLabel("Date de péremption").fill(tomorrow);
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
