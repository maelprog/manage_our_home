import { BrowserContext, expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";
import { fillIngredients, fillSteps } from "../lib/recipe-form";
import { skipMealStart } from "../lib/meal-start";

// The cooking timers handed to the Android app's alarm (#1019): in the
// app, the page finds the CookTimers plugin (#1017) and Local
// Notifications on `window.Capacitor.Plugins`. Here both are fakes, put in
// the page before its scripts run, that note every call; Web Audio is
// watched so that a beep shows. Without the fakes, the page is a browser's.

test.beforeEach(async ({ context }) => {
  await skipMealStart(context);
});

const PASSWORD = "e2e-cook-native-password-1";

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
  await page.getByLabel("Nom du groupe").fill("Famille Alarmes");
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
}

// A recipe with a 4 s timer, then its Cuisiner: the recipe's id.
async function cookRecipe(page: Page): Promise<string> {
  await page.goto("/recipes/new");
  await page.getByLabel("Nom").fill("Ratatouille");
  await fillSteps(page, "Laisser reposer 4 secondes.\nServir.");
  await fillIngredients(page, "Aubergine | 2");
  await page.getByRole("button", { name: "Créer la recette" }).click();
  await expect(page).toHaveURL(/\/recipes\/[0-9a-f-]+\?notice=recipe_created$/);
  const id = new URL(page.url()).pathname.split("/")[2];
  await page.getByRole("link", { name: "Mode cuisine" }).click();
  await expect(page).toHaveURL(`/recipes/${id}/cook`);
  await expect(page.getByText("Étape 1 sur 2", { exact: true })).toBeVisible();
  return id;
}

// Counts the oscillators Web Audio makes: three per beep.
async function watchBeeps(context: BrowserContext): Promise<void> {
  await context.addInitScript(() => {
    const w = window as unknown as { __beeps: number; AudioContext?: typeof AudioContext };
    w.__beeps = 0;
    if (!w.AudioContext) return;
    const make = w.AudioContext.prototype.createOscillator;
    w.AudioContext.prototype.createOscillator = function (this: AudioContext) {
      w.__beeps += 1;
      return make.call(this);
    };
  });
}

// The app's plugins, faked. `exact`: whether Android gives exact alarms.
async function fakeApp(context: BrowserContext, exact: boolean): Promise<void> {
  await context.addInitScript((exact: boolean) => {
    const calls: unknown[][] = [];
    (window as unknown as { __calls: unknown[][] }).__calls = calls;
    (window as unknown as { Capacitor: unknown }).Capacitor = {
      Plugins: {
        CookTimers: {
          schedule: (o: unknown) => {
            calls.push(["schedule", o]);
            return Promise.resolve({ scheduled: true, exact, notifications: true });
          },
          cancel: (o: unknown) => {
            calls.push(["cancel", o]);
            return Promise.resolve();
          },
          stop: () => {
            calls.push(["stop"]);
            return Promise.resolve();
          },
        },
        LocalNotifications: {
          checkPermissions: () => Promise.resolve({ display: "granted" }),
          requestPermissions: () => Promise.resolve({ display: "granted" }),
          checkExactNotificationSetting: () => Promise.resolve({ exact_alarm: exact ? "granted" : "denied" }),
          changeExactNotificationSetting: () => {
            calls.push(["exact-setting"]);
            return Promise.resolve({ exact_alarm: "granted" });
          },
        },
      },
    };
  }, exact);
}

function calls(page: Page): Promise<unknown[][]> {
  return page.evaluate(() => (window as unknown as { __calls: unknown[][] }).__calls);
}

function beeps(page: Page): Promise<number> {
  return page.evaluate(() => (window as unknown as { __beeps: number }).__beeps);
}

test("in the app, a timer is the phone's alarm: scheduled, cancelled in pause, rung without a beep, stopped alone", async ({
  page,
  context,
}) => {
  test.setTimeout(60_000);
  await registerAndLogin(page, "e2e-cook-native");
  await watchBeeps(context);
  await fakeApp(context, true);
  const recipe = await cookRecipe(page);

  // Started: scheduled at its end, the touch opening this page.
  await page.getByRole("button", { name: "Lancer un minuteur de 4 secondes" }).click();
  await expect.poll(() => calls(page)).toHaveLength(1);
  const [first] = await calls(page);
  expect(first[0]).toBe("schedule");
  const alarm = first[1] as { id: string; endAt: number; title: string; text: string; url: string };
  expect(alarm.id).toMatch(new RegExp(`^${recipe}_\\d+$`));
  expect(alarm.endAt).toBeGreaterThan(Date.now());
  expect(alarm).toMatchObject({ title: "Ratatouille", text: "Étape 1 · 4 s", url: `/recipes/${recipe}/cook` });

  // Paused: cancelled; resumed: scheduled again.
  await page.getByRole("button", { name: "Pause" }).click();
  await expect.poll(() => calls(page)).toEqual([first, ["cancel", { id: alarm.id }]]);
  await page.getByRole("button", { name: "Reprendre" }).click();
  await expect.poll(async () => (await calls(page)).length).toBe(3);
  expect((await calls(page))[2]).toMatchObject(["schedule", { id: alarm.id }]);

  // Over: « Terminé ! », the alarm rings — not cancelled — and no beep.
  const done = page.getByRole("alert").filter({ hasText: "Étape 1 · 4 s" });
  await expect(done).toContainText("Terminé !", { timeout: 10_000 });
  await page.waitForTimeout(2_500);
  expect(await beeps(page)).toBe(0);
  expect(await calls(page)).toHaveLength(3);
  // Exact alarms granted: no button for them.
  await expect(page.getByRole("button", { name: "Autoriser les alarmes à l'heure exacte" })).toBeHidden();

  // « Arrêter »: its own alarm cancelled, never every one — at the press,
  // not at the next pass of cook-watch.js.
  const atPress = await done.getByRole("button", { name: "Arrêter" }).evaluate((button) => {
    (button as HTMLButtonElement).click();
    return (window as unknown as { __calls: unknown[][] }).__calls.slice(3);
  });
  expect(atPress).toEqual([["cancel", { id: alarm.id }]]);
  await page.waitForTimeout(1_500);
  expect((await calls(page)).slice(3)).toEqual([["cancel", { id: alarm.id }]]);
});

test("in the app without exact alarms, a button on Cuisiner opens « Alarmes et rappels »", async ({
  page,
  context,
}) => {
  await registerAndLogin(page, "e2e-cook-exact");
  await fakeApp(context, false);
  await cookRecipe(page);
  await page.getByRole("button", { name: "Lancer un minuteur de 4 secondes" }).click();
  const exact = page.getByRole("button", { name: "Autoriser les alarmes à l'heure exacte" });
  await expect(exact).toBeVisible();
  // Notifications granted: their button stays hidden.
  await expect(page.getByRole("button", { name: "Prévenir à la fin" })).toBeHidden();
  await exact.click();
  await expect.poll(async () => (await calls(page)).filter((c) => c[0] === "exact-setting")).toHaveLength(1);
});

test("in a browser, with no plugin, a timer that ends beeps as before", async ({ page, context }) => {
  test.setTimeout(60_000);
  await registerAndLogin(page, "e2e-cook-beep");
  await watchBeeps(context);
  await cookRecipe(page);
  await page.getByRole("button", { name: "Lancer un minuteur de 4 secondes" }).click();
  const done = page.getByRole("alert").filter({ hasText: "Étape 1 · 4 s" });
  await expect(done).toContainText("Terminé !", { timeout: 10_000 });
  await expect.poll(() => beeps(page)).toBeGreaterThan(0);
  await done.getByRole("button", { name: "Arrêter" }).click();
  const after = await beeps(page);
  await page.waitForTimeout(2_500);
  expect(await beeps(page)).toBe(after);
});
