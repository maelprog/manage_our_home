import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Issue #523: the page transition, `@view-transition { navigation: auto; }`,
// asked for under `prefers-reduced-motion: no-preference` only.
//
// The rest of the suite browses under reduced motion (playwright.config.ts),
// where no transition is asked for, because of how Playwright launches
// Chromium: among its default arguments is `--disable-back-forward-cache`,
// and with it the page a cross-document view transition leads to gets no
// frame — no `requestAnimationFrame`, every click there waiting for
// "stable" until the test times out, headless or headed alike. This file
// leaves that argument out, on the full Chromium build (`channel:
// "chromium"`): the headless shell, Playwright's default, stays frozen even
// without it. Top-level, because both options start a browser of their own.
test.use({
  viewport: { width: 1440, height: 900 },
  channel: "chromium",
  launchOptions: { ignoreDefaultArgs: ["--disable-back-forward-cache"] },
  contextOptions: { reducedMotion: "no-preference" },
});

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-transition-password-1";

async function registerAndLogin(page: Page): Promise<void> {
  const email = uniqueEmail("e2e-transition");
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill("Transition User");
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

/** Whether the current page was revealed through a view transition. */
async function revealedByATransition(page: Page): Promise<boolean> {
  return page.evaluate(() => (window as unknown as { transitioned?: boolean }).transitioned === true);
}

/** How many of three animation frames come within two seconds. */
async function frames(page: Page): Promise<number> {
  return page.evaluate(
    () =>
      new Promise<number>((resolve) => {
        let count = 0;
        const tick = () => (++count === 3 ? resolve(count) : requestAnimationFrame(tick));
        requestAnimationFrame(tick);
        setTimeout(() => resolve(count), 2000);
      }),
  );
}

test("a link followed with the transition leads to a page that answers", async ({ page }) => {
  // Every step here, sign-up included, navigates with the transition asked.
  await registerAndLogin(page);
  await page.goto("/groups/new");
  await page.getByLabel("Nom du groupe").fill("Famille Mouvement");
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);

  await page.addInitScript(() => {
    addEventListener("pagereveal", (event) => {
      (window as unknown as { transitioned: boolean }).transitioned =
        (event as Event & { viewTransition: unknown }).viewTransition != null;
    });
  });
  await page.goto("/");
  const nav = page.getByRole("navigation", { name: "Navigation principale" });

  await nav.getByRole("link", { name: "Agenda", exact: true }).click();
  await expect(page).toHaveURL("/agenda");
  await expect.poll(() => revealedByATransition(page)).toBe(true);
  expect(await frames(page)).toBe(3);

  // A click there lands, through a second transition.
  await nav.getByRole("link", { name: "Courses", exact: true }).click({ timeout: 5000 });
  await expect(page).toHaveURL("/grocery-list");
  await expect.poll(() => revealedByATransition(page)).toBe(true);
  expect(await frames(page)).toBe(3);
});
