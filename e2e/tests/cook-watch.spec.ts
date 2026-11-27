import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";
import { fillIngredients, fillSteps } from "../lib/recipe-form";
import { skipMealStart } from "../lib/meal-start";

// The cooking timers away from Cuisiner (#1018): the home's card « Minuteurs
// en cours », and at a timer's end, on any other page of the app, the window
// that rings until « Arrêter » and opens the recipe's Cuisiner 20 s later.

test.beforeEach(async ({ context }) => {
  await skipMealStart(context);
});

const PASSWORD = "e2e-cook-watch-password-1";

async function registerAndLogin(page: Page): Promise<void> {
  const email = `e2e-cook-watch-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill("Cuisinier");
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
  await page.goto("/groups/new");
  await page.getByLabel("Nom du groupe").fill("Famille Minuteurs");
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
}

test("a timer left running shows on the home, then rings there and leads back to its recipe", async ({
  page,
  context,
}) => {
  // An 8 s timer, then the 20 s before the redirection, in real time.
  test.setTimeout(90_000);
  await registerAndLogin(page);
  await page.goto("/recipes/new");
  await page.getByLabel("Nom").fill("Ratatouille");
  await fillSteps(page, "Laisser reposer 8 secondes.\nServir.");
  await fillIngredients(page, "Aubergine | 2");
  await page.getByRole("button", { name: "Créer la recette" }).click();
  await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\?notice=recipe_created$/);
  const recipe = page.url();

  // Without a timer the home has no card.
  await page.goto("/");
  const card = page.getByRole("region", { name: "Minuteurs en cours" });
  await expect(page.getByRole("heading", { name: "Bonjour" })).toBeVisible();
  await expect(card).toBeHidden();

  await page.goto(recipe);
  await page.getByRole("link", { name: "Mode cuisine" }).click();
  await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\/cook$/);
  const cook = page.url();
  await expect(page.getByText("Étape 1 sur 2", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Lancer un minuteur de 8 secondes" }).click();
  await expect(page.getByRole("timer", { name: "Étape 1 · 8 s" })).toBeVisible();
  const timers = page.getByRole("region", { name: "Minuteurs" });

  // Away from Cuisiner: the card, first of the grid, a row linking back.
  await page.goto("/");
  await expect(card).toBeVisible();
  await expect(page.locator(".dash > :first-child")).toHaveAttribute("data-home-timers", "");
  const row = card.getByRole("link", { name: /Ratatouille/ });
  await expect(row).toHaveAttribute("href", new URL(cook).pathname);
  await expect(row).toContainText("Étape 1 · 8 s");
  await expect(card.getByRole("timer", { name: "Étape 1 · 8 s, Ratatouille" })).toHaveText(/^00:0\d$/);

  // Its end: the window, « Arrêter » focused, and the card gone.
  const done = page.getByRole("dialog", { name: "Ratatouille" });
  await expect(done).toBeVisible({ timeout: 15_000 });
  await expect(done).toContainText("Minuteur terminé");
  await expect(done).toContainText("Étape 1 · 8 s");
  await expect(done.getByRole("timer")).toHaveText(/^Ouverture de la recette dans \d+ s…$/);
  await expect(done.getByRole("button", { name: "Arrêter" })).toBeFocused();
  await expect(done.getByRole("link", { name: "Ouvrir la recette" })).toHaveAttribute("href", new URL(cook).pathname);
  await expect(card).toBeHidden();

  // 20 s later, its recipe's Cuisiner, where the timer says « Terminé ! »
  // until its own « Arrêter ».
  await expect(page).toHaveURL(cook, { timeout: 30_000 });
  const ended = page.getByRole("alert").filter({ hasText: "Étape 1 · 8 s" });
  await expect(ended).toContainText("Terminé !");
  await ended.getByRole("button", { name: "Arrêter" }).click();
  await expect(timers).toBeHidden();

  // Storage that throws: no error, and the home as it was.
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await context.addInitScript(() => {
    Object.defineProperty(window, "localStorage", {
      configurable: true,
      get: () => {
        throw new Error("storage refused");
      },
    });
  });
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Bonjour" })).toBeVisible();
  await expect(card).toBeHidden();
  expect(errors).toEqual([]);
});

// What the Cuisiner pages keep, written straight into storage: a timer of
// each recipe given, ended `agoMs` ago and not yet marked finished.
async function keepEnded(page: Page, timers: { recipe: string; title: string; what: string; agoMs: number }[]) {
  await page.evaluate((timers) => {
    for (const t of timers) {
      localStorage.setItem(
        `mhome.cuisine.${t.recipe}`,
        JSON.stringify({
          at: Date.now() - t.agoMs,
          step: 0,
          title: t.title,
          url: `/recipes/${t.recipe}/cook`,
          timers: [{ id: "t-" + t.recipe, what: t.what, end: Date.now() - t.agoMs, left: 0, done: false }],
        })
      );
    }
  }, timers);
}

const GRATIN = "ef5a0c8e-b6d9-4c62-9051-0ebbcd646b24";
const RATATOUILLE = "7b1c2d44-5a0e-4f1b-9c3a-0d6e8f21a977";

test("several ended timers are listed oldest first; « Arrêter » forgets them and stays", async ({ page }) => {
  const ended = [
    { recipe: RATATOUILLE, title: "Ratatouille", what: "Étape 3 · 45 min", agoMs: 1_000 },
    { recipe: GRATIN, title: "Gratin dauphinois", what: "Étape 4 · 1 h", agoMs: 2 * 60_000 + 5_000 },
  ];
  const done = page.getByRole("dialog", { name: "Minuteurs terminés" });

  // Outside a session, nothing watches: no window, no recipe's name, and
  // what is kept stays as it was.
  await page.goto("/login");
  await keepEnded(page, ended);
  await page.reload();
  await page.waitForTimeout(1_500);
  await expect(done).toBeHidden();
  const untouched = await page.evaluate(
    (ids) => ids.map((id) => JSON.parse(localStorage.getItem(`mhome.cuisine.${id}`)!).timers[0].done),
    [RATATOUILLE, GRATIN]
  );
  expect(untouched).toEqual([false, false]);
  await page.evaluate(() => localStorage.clear());

  // Any page of the app that is not Cuisiner watches: the home will do.
  await registerAndLogin(page);
  await keepEnded(page, ended);
  await page.goto("/");
  await expect(done).toBeVisible();
  await expect(done).toContainText("2 minuteurs terminés");
  const rows = done.getByRole("listitem");
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(0)).toContainText("Gratin dauphinois");
  await expect(rows.nth(0)).toContainText("il y a 2 min");
  await expect(rows.nth(1)).toContainText("Ratatouille");
  await expect(rows.nth(1)).toContainText("à l'instant");
  await expect(done.getByRole("timer")).toHaveText(/^Ouverture de Gratin dauphinois, le plus ancien, dans \d+ s…$/);
  await expect(done.getByRole("link", { name: "Ouvrir la recette" })).toHaveAttribute("href", `/recipes/${GRATIN}/cook`);

  await done.getByRole("button", { name: "Arrêter" }).click();
  await expect(done).toBeHidden();
  const left = await page.evaluate(
    (ids) => ids.map((id) => JSON.parse(localStorage.getItem(`mhome.cuisine.${id}`)!).timers.length),
    [RATATOUILLE, GRATIN]
  );
  expect(left).toEqual([0, 0]);
  // Nothing comes back, nothing leaves.
  await page.waitForTimeout(1_500);
  await expect(done).toBeHidden();
  await expect(page).toHaveURL(/\/$/);
});

test("a timer whose Cuisiner is open in another tab rings there only", async ({ page, context }) => {
  // The Cuisiner page rings its own timers, even from the background: no
  // other tab claims them, opens a window or leads to a second Cuisiner.
  test.setTimeout(90_000);
  await registerAndLogin(page);
  await page.goto("/recipes/new");
  await page.getByLabel("Nom").fill("Ratatouille");
  await fillSteps(page, "Laisser reposer 8 secondes.\nServir.");
  await fillIngredients(page, "Aubergine | 2");
  await page.getByRole("button", { name: "Créer la recette" }).click();
  await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\?notice=recipe_created$/);
  // A background tab may pass once a minute only: the Cuisiner tab's
  // intervals are held until `frozen` is lifted.
  const cooking = await context.newPage();
  await cooking.addInitScript(() => {
    const w = window as unknown as { frozen: boolean };
    w.frozen = false;
    const real = window.setInterval.bind(window);
    window.setInterval = ((fn: () => void, ms?: number) =>
      real(() => {
        if (!w.frozen) fn();
      }, ms)) as typeof window.setInterval;
  });
  await cooking.goto(new URL(page.url()).pathname + "/cook");
  await cooking.getByRole("button", { name: "Lancer un minuteur de 8 secondes" }).click();
  await expect(cooking.getByRole("timer", { name: "Étape 1 · 8 s" })).toBeVisible();
  await cooking.evaluate(() => {
    (window as unknown as { frozen: boolean }).frozen = true;
  });

  await page.goto("/");
  const card = page.getByRole("region", { name: "Minuteurs en cours" });
  await expect(card).toBeVisible();
  // Its end comes and goes here; Cuisiner has not passed since.
  await expect(card).toBeHidden({ timeout: 15_000 });
  await page.waitForTimeout(3_000);
  await expect(page.getByRole("dialog")).toHaveCount(0);
  const kept = () =>
    page.evaluate(() => {
      const key = Object.keys(localStorage).find((k) => k.startsWith("mhome.cuisine."))!;
      const timer = JSON.parse(localStorage.getItem(key)!).timers[0];
      return { done: timer.done, owner: timer.owner ?? null };
    });
  expect(await kept()).toEqual({ done: false, owner: null });
  await expect(page).toHaveURL(/\/$/);

  // Cuisiner passes again: it rings its own timer, and nothing here does.
  await cooking.evaluate(() => {
    (window as unknown as { frozen: boolean }).frozen = false;
  });
  const ended = cooking.getByRole("alert").filter({ hasText: "Étape 1 · 8 s" });
  await expect(ended).toContainText("Terminé !");
  await page.waitForTimeout(1_500);
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(await kept()).toEqual({ done: true, owner: null });
});

test("the notification is never asked for without a press on « Prévenir à la fin »", async ({ page }) => {
  await page.addInitScript(() => {
    (window as unknown as { asked: number }).asked = 0;
    class FakeNotification {
      static permission = "default";
      static requestPermission() {
        (window as unknown as { asked: number }).asked += 1;
        FakeNotification.permission = "granted";
        return Promise.resolve("granted");
      }
    }
    Object.defineProperty(window, "Notification", { configurable: true, value: FakeNotification });
  });
  await registerAndLogin(page);
  await page.evaluate((id) => {
    localStorage.setItem(
      `mhome.cuisine.${id}`,
      JSON.stringify({
        at: Date.now(),
        step: 0,
        title: "Ratatouille",
        timers: [{ id: "t1", what: "Étape 3 · 45 min", end: Date.now() + 45 * 60_000, left: 0, done: false }],
      })
    );
  }, RATATOUILLE);
  await page.goto("/");
  const card = page.getByRole("region", { name: "Minuteurs en cours" });
  await expect(card).toBeVisible();
  const ask = card.getByRole("button", { name: "Prévenir à la fin" });
  await expect(ask).toBeVisible();
  expect(await page.evaluate(() => (window as unknown as { asked: number }).asked)).toBe(0);
  await ask.click();
  await expect(card.getByText("Vous serez prévenu à la fin, tant que cet onglet reste ouvert.")).toBeVisible();
  await expect(ask).toBeHidden();
  expect(await page.evaluate(() => (window as unknown as { asked: number }).asked)).toBe(1);
});
