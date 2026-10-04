import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Reminders by notification (#306). A new account's reminders go out as
// notifications; with no device subscribed it would receive none, and no
// email in their place (controller's decision of 2026-10-01). The journeys
// below are the warning that says so, where reminders are set and in the
// preferences, and the way out it offers: switching to email.
//
// What the browser alone knows — a permission refused on this device, no
// Push API — is revealed by an inline script, and only once the server has
// a VAPID key; the CI stack runs without one (notifications off). Those
// branches (permission denied, default, granted) are verified by hand only:
// apps/web's unit tests check the markup they act on, not the script.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-notif-password-1";

/**
 * Avoids every form label of the pages under test (`getByLabel` matches
 * substrings, the family switcher included): Titre, Début, Fin, Lieu,
 * Description, Rappel, and the three channel choices.
 */
const FAMILY = "Foyer Cloche";

async function registerAndLogin(page: Page, prefix: string): Promise<void> {
  const email = uniqueEmail(prefix);
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill("Camille");
  await page.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
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

async function createGroup(page: Page): Promise<void> {
  await page.goto("/groups/new");
  await page.getByLabel("Nom du groupe").fill(FAMILY);
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
}

test.describe("Reminder notifications", () => {
  test("a new account is warned where it sets a reminder, and can switch to email", async ({
    page,
  }) => {
    await registerAndLogin(page, "notif-warn");
    await createGroup(page);

    await page.goto("/agenda/new");
    const warning = page.locator("#push-warning");
    await expect(warning).toBeVisible();
    await expect(warning).toContainText("Vous ne recevrez pas vos rappels par notification");

    // The warning's way out lands on the email choice itself.
    await warning.getByRole("link", { name: "passer aux rappels par email" }).click();
    await expect(page).toHaveURL(/\/account\/notifications#rappels-email$/);
    await expect(page.getByRole("radio", { name: "Par notification sur mes appareils" })).toBeChecked();
    await expect(page.locator("#push-warning")).toBeVisible();

    await page.getByRole("radio", { name: "Par email" }).check();
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page).toHaveURL(/\/account\/notifications\?notice=channel_saved$/);
    await expect(page.getByText("Préférence enregistrée.")).toBeVisible();
    await expect(page.getByRole("radio", { name: "Par email" })).toBeChecked();
    await expect(page.locator("#push-warning")).toHaveCount(0);

    // On email, the reminder form no longer warns.
    await page.goto("/agenda/new");
    await expect(page.getByLabel("Rappel")).toBeVisible();
    await expect(page.locator("#push-warning")).toHaveCount(0);
  });

  test("the account hub leads to the reminder preferences", async ({ page }) => {
    await registerAndLogin(page, "notif-hub");
    await page.goto("/account");
    await page.getByRole("link", { name: "Gérer mes notifications" }).click();
    await expect(page).toHaveURL(/\/account\/notifications$/);
    await expect(page.getByRole("heading", { name: "Notifications de rappel" })).toBeVisible();
    // What a notification shows is said before anyone subscribes.
    await expect(page.getByText("« Rappel d'un événement à venir »")).toBeVisible();
  });

  test("the service worker is served from the root and shows the neutral text only", async ({
    request,
  }) => {
    const res = await request.get("/sw.js");
    expect(res.status()).toBe(200);
    expect(res.headers()["content-type"]).toContain("javascript");
    expect(res.headers()["cache-control"]).toBe("no-cache");
    const body = await res.text();
    expect(body).toContain(`showNotification("Rappel d'un événement à venir"`);
    expect(body).not.toContain("event.data");
  });
});
