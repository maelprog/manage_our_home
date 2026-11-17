import { BrowserContext, expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";
import { fillIngredients, fillSteps } from "../lib/recipe-form";
import { skipMealStart } from "../lib/meal-start";

// Cooking a recipe step by step (#823): « Mode cuisine » on a recipe that is
// neither planned nor dated, one step at a time, the timers a step's
// durations offer, kept when the page is left and found again, and the
// screen kept on while the page is shown, released when it is left.

test.beforeEach(async ({ context }) => {
  await skipMealStart(context);
});

const PASSWORD = "e2e-cook-password-1";

async function registerAndLogin(page: Page, prefix: string): Promise<void> {
  const email = `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
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
  await page.getByLabel("Nom du groupe").fill("Famille Cuisson");
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
}

/** Creates the recipe; ends on its page, which is neither planned nor dated. */
async function createRecipe(page: Page): Promise<void> {
  await page.goto("/recipes/new");
  await page.getByLabel("Nom").fill("Ratatouille");
  await fillSteps(
    page,
    "Tailler les légumes.\nFaire revenir 8-10 min.\nLaisser reposer 45 secondes, puis servir."
  );
  await fillIngredients(page, "Aubergine | 2");
  await page.getByRole("button", { name: "Créer la recette" }).click();
  await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\?notice=recipe_created$/);
}

// A Wake Lock that says, in sessionStorage, whether it is held or released:
// a headless browser has no screen to keep on, and the page that released
// it is gone by the time the test looks.
async function fakeWakeLock(context: BrowserContext): Promise<void> {
  await context.addInitScript(() => {
    Object.defineProperty(Navigator.prototype, "wakeLock", {
      configurable: true,
      get: () => ({
        request: () => {
          const lock = new EventTarget() as EventTarget & { release: () => Promise<void> };
          lock.release = () => {
            sessionStorage.setItem("e2e-wake", "released");
            lock.dispatchEvent(new Event("release"));
            return Promise.resolve();
          };
          sessionStorage.setItem("e2e-wake", "held");
          return Promise.resolve(lock);
        },
      }),
    });
  });
}

test("Mode cuisine: steps, a timer kept across a visit, the screen kept on then released", async ({
  page,
  context,
}) => {
  await fakeWakeLock(context);
  await registerAndLogin(page, "e2e-cook");
  await createRecipe(page);

  await page.getByRole("link", { name: "Mode cuisine" }).click();
  await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\/cook$/);
  await expect(page.getByText("Étape 1 sur 3", { exact: true })).toBeVisible();
  await expect(page.getByRole("region", { name: "Étape 1" })).toContainText("Tailler les légumes.");
  await expect(page.getByRole("region", { name: "Étape 2" })).toBeHidden();
  // The step under « Ingrédients » at --s4, as in the mock-up.
  const ingredients = (await page.getByRole("link", { name: "Ingrédients" }).boundingBox())!;
  const step = (await page.getByRole("region", { name: "Étape 1" }).boundingBox())!;
  expect(Math.round(step.y - ingredients.y - ingredients.height)).toBe(16);
  await expect(page.getByRole("button", { name: "Précédent" })).toBeDisabled();
  await expect(page.getByText("Écran maintenu allumé")).toBeVisible();
  expect(await page.evaluate(() => sessionStorage.getItem("e2e-wake"))).toBe("held");

  // Suivant, then the keyboard.
  await page.getByRole("button", { name: "Suivant" }).click();
  await expect(page.getByText("Étape 2 sur 3", { exact: true })).toBeVisible();
  const second = page.getByRole("region", { name: "Étape 2" });
  await expect(second).toContainText("Faire revenir 8-10 min.");
  await page.keyboard.press("ArrowRight");
  await expect(page.getByText("Étape 3 sur 3", { exact: true })).toBeVisible();
  await expect(page.getByRole("link", { name: "Terminer" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Suivant" })).toBeHidden();
  await page.keyboard.press("ArrowLeft");
  await expect(page.getByText("Étape 2 sur 3", { exact: true })).toBeVisible();

  // The ingredients in one tap.
  await page.getByRole("link", { name: "Ingrédients" }).click();
  const dialog = page.getByRole("dialog", { name: "Ingrédients" });
  await expect(dialog).toContainText("Aubergine");
  await dialog.getByRole("button", { name: "Fermer" }).click();
  await expect(dialog).toBeHidden();

  // « 8-10 min » starts 8 minutes: the low end.
  await second.getByRole("button", { name: "Lancer un minuteur de 8 minutes" }).click();
  const timer = page.getByRole("timer", { name: "Étape 2 · 8 min" });
  await expect(timer).toHaveText(/^0[78]:\d\d$/);

  // Leaving the page releases the screen; coming back finds the step and
  // the timer.
  await page.getByRole("link", { name: "Quitter le mode cuisine" }).click();
  await expect(page.getByRole("heading", { name: "Ratatouille", level: 1 })).toBeVisible();
  expect(await page.evaluate(() => sessionStorage.getItem("e2e-wake"))).toBe("released");
  await page.getByRole("link", { name: "Mode cuisine" }).click();
  await expect(page.getByText("Étape 2 sur 3", { exact: true })).toBeVisible();
  await expect(timer).toHaveText(/^0[78]:\d\d$/);
  expect(await page.evaluate(() => sessionStorage.getItem("e2e-wake"))).toBe("held");

  // « Terminer » goes back to the recipe and forgets the cooking.
  await page.getByRole("button", { name: "Suivant" }).click();
  await page.getByRole("link", { name: "Terminer" }).click();
  await expect(page.getByRole("heading", { name: "Ratatouille", level: 1 })).toBeVisible();
  await page.getByRole("link", { name: "Mode cuisine" }).click();
  await expect(page.getByText("Étape 1 sur 3", { exact: true })).toBeVisible();
  await expect(page.getByRole("region", { name: "Minuteurs" })).toBeHidden();
});

// What the page keeps of a cooking (`mhome.cuisine.<id>`), written as if it
// had been left `hoursAgo` hours ago, then the page opened again.
async function comeBackTo(
  page: Page,
  hoursAgo: number,
  timers: { what: string; end: number; left: number; done: boolean }[]
): Promise<void> {
  const id = /\/recipes\/([0-9a-f-]+)\/cook$/.exec(page.url())![1];
  await page.evaluate(
    ([key, hoursAgo, timers]) => {
      const at = Date.now() - (hoursAgo as number) * 3600_000;
      const kept = (timers as { end: number }[]).map((timer, i) => ({
        id: "t" + i,
        ...timer,
        end: timer.end ? Date.now() + timer.end : 0,
      }));
      localStorage.setItem(key as string, JSON.stringify({ at, step: 1, timers: kept }));
    },
    [`mhome.cuisine.${id}`, hoursAgo, timers] as const
  );
  await page.reload();
}

test("a timer not over is never forgotten; the half day starts at the last one's end", async ({ page }) => {
  await registerAndLogin(page, "e2e-cook-kept");
  await createRecipe(page);
  await page.getByRole("link", { name: "Mode cuisine" }).click();
  const H = 3600_000;
  const timers = page.getByRole("region", { name: "Minuteurs" });

  // A 24 h timer started 13 h ago: 11 h still to run.
  await comeBackTo(page, 13, [{ what: "Étape 2 · 24 h", end: 11 * H, left: 0, done: false }]);
  await expect(page.getByText("Étape 2 sur 3", { exact: true })).toBeVisible();
  await expect(page.getByRole("timer", { name: "Étape 2 · 24 h" })).toHaveText(/^1[01]:\d\d:\d\d$/);

  // A timer paused a month ago still has its hour to run.
  await comeBackTo(page, 24 * 30, [{ what: "Étape 2 · 1 h", end: 0, left: H, done: false }]);
  await expect(page.getByRole("timer", { name: "Étape 2 · 1 h" })).toHaveText("1:00:00");

  // Left a day ago, but the timer ended an hour ago: still there.
  await comeBackTo(page, 24, [{ what: "Étape 2 · 23 h", end: -H, left: 0, done: false }]);
  await expect(timers.getByRole("alert")).toContainText("Terminé !");

  // Ended 13 h ago, left as long: forgotten.
  await comeBackTo(page, 13, [{ what: "Étape 2 · 8 min", end: -13 * H, left: 0, done: true }]);
  await expect(page.getByText("Étape 1 sur 3", { exact: true })).toBeVisible();
  await expect(timers).toBeHidden();

  // Coming back is no change: the half day still counts from the leaving.
  await comeBackTo(page, 11, []);
  await expect(page.getByText("Étape 2 sur 3", { exact: true })).toBeVisible();
  const at = await page.evaluate(() => {
    const key = Object.keys(localStorage).find((k) => k.startsWith("mhome.cuisine."))!;
    return Date.now() - JSON.parse(localStorage.getItem(key)!).at;
  });
  expect(at).toBeGreaterThan(10 * H);
});

test("a timer that ends says « Terminé ! » until « Arrêter »", async ({ page }) => {
  await registerAndLogin(page, "e2e-cook-ring");
  await createRecipe(page);
  await page.clock.install();
  await page.getByRole("link", { name: "Mode cuisine" }).click();
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("ArrowRight");
  const third = page.getByRole("region", { name: "Étape 3" });
  await third.getByRole("button", { name: "Lancer un minuteur de 45 secondes" }).click();
  await expect(page.getByRole("timer", { name: "Étape 3 · 45 s" })).toHaveText("00:45");
  await page.clock.fastForward("00:46");
  const done = page.getByRole("alert").filter({ hasText: "Étape 3 · 45 s" });
  await expect(done).toContainText("Terminé !");
  await done.getByRole("button", { name: "Arrêter" }).click();
  await expect(page.getByRole("region", { name: "Minuteurs" })).toBeHidden();
});
