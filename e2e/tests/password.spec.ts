import { expect, Page, test } from "@playwright/test";
import { countUsers, fetchPasswordResetToken, fetchVerificationToken } from "../lib/db";

// #461: a password being chosen is typed twice, and a gauge under the first
// entry says how strong it is.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

async function fillRegistration(page: Page, email: string, password: string, confirmation: string) {
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill("Password User");
  await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
  await page.getByRole("textbox", { name: "Confirmation" }).fill(confirmation);
  await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
  await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
}

const MISMATCH = "Les deux mots de passe ne correspondent pas.";

test.describe("Password — confirmation", () => {
  test("a registration whose two entries differ is refused, and creates nothing", async ({
    page,
  }) => {
    const email = uniqueEmail("e2e-pw-mismatch");
    await fillRegistration(page, email, "e2e-password-mismatch", "e2e-password-mismatsh");
    await page.getByRole("button", { name: "Créer mon compte" }).click();

    await expect(page).toHaveURL(/\/register$/);
    const confirmation = page.getByRole("textbox", { name: "Confirmation" });
    await expect(confirmation).toHaveAttribute("aria-invalid", "true");
    await expect(confirmation).toHaveAccessibleDescription(MISMATCH);
    expect(await countUsers(email)).toBe(0);
  });

  test("a reset whose two entries differ is refused, and the link still works", async ({
    page,
  }) => {
    const email = uniqueEmail("e2e-pw-reset");
    const newPassword = "e2e-new-password-461";
    await fillRegistration(page, email, "e2e-old-password-461", "e2e-old-password-461");
    await page.getByRole("button", { name: "Créer mon compte" }).click();
    await expect(page).toHaveURL(/\/register\/check-email$/);
    await page.goto(`/verify-email?token=${await fetchVerificationToken(email)}`);

    await page.goto("/forgot-password");
    await page.getByLabel("Email").fill(email);
    await page.getByRole("button", { name: "Envoyer le lien de réinitialisation" }).click();
    await expect(page.getByText("Si ce compte existe, un email a été envoyé.")).toBeVisible();
    await page.goto(`/reset-password#token=${await fetchPasswordResetToken(email)}`);

    await page.getByLabel("Nouveau mot de passe").fill(newPassword);
    await page.getByRole("textbox", { name: "Confirmation" }).fill(`${newPassword}x`);
    await page.getByRole("button", { name: "Réinitialiser" }).click();
    const confirmation = page.getByRole("textbox", { name: "Confirmation" });
    await expect(confirmation).toHaveAccessibleDescription(MISMATCH);

    // The form kept the token, which apps/api never saw: it is still good.
    await page.getByLabel("Nouveau mot de passe").fill(newPassword);
    await confirmation.fill(newPassword);
    await page.getByRole("button", { name: "Réinitialiser" }).click();
    await expect(page.getByText("Mot de passe mis à jour")).toBeVisible();

    await page.goto("/login");
    await page.getByLabel("Email").fill(email);
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(newPassword);
    await page.getByRole("button", { name: "Se connecter" }).click();
    await expect(page).toHaveURL("/");
  });
});

test.describe("Password — strength gauge", () => {
  test("the gauge follows what is typed, in words", async ({ page }) => {
    await page.goto("/register");
    const field = page.getByRole("textbox", { name: "Mot de passe" });
    const words = page.locator(".pw-meter [aria-live='polite']");
    await expect(words).toHaveText("");

    // Under the 12 characters apps/api asks for, whatever their variety.
    await field.fill("Ab1!Ab1!Ab1");
    await expect(words).toHaveText("Robustesse : trop court");
    await field.fill("abcdefghijkl");
    await expect(words).toHaveText("Robustesse : faible");
    await field.fill("abcdefghijklmnop");
    await expect(words).toHaveText("Robustesse : correct");
    // Variety counts too: same length, one level up.
    await field.fill("abcdefghijklmn1!");
    await expect(words).toHaveText("Robustesse : solide");
    await field.fill("une phrase de passe assez longue");
    await expect(words).toHaveText("Robustesse : solide");
    await field.fill("");
    await expect(words).toHaveText("");

    // Only the first entry has one.
    await expect(page.locator(".pw-meter")).toHaveCount(1);
  });

  test("the reset form has the gauge too", async ({ page }) => {
    await page.goto("/reset-password#token=Zq3_k9XvT2mQ8pLw4rYb7nHc1sDf6gJ0aEuIoVtBy5M");
    await page.getByLabel("Nouveau mot de passe").fill("court");
    await expect(page.locator(".pw-meter [aria-live='polite']")).toHaveText(
      "Robustesse : trop court",
    );
  });
});

test.describe("Password — without JavaScript", () => {
  test.use({ javaScriptEnabled: false });

  test("registration works with no gauge", async ({ page }) => {
    const email = uniqueEmail("e2e-pw-nojs");
    await fillRegistration(page, email, "e2e-password-nojs", "e2e-password-nojs");
    await expect(page.locator(".pw-meter")).toHaveCount(0);
    await page.getByRole("button", { name: "Créer mon compte" }).click();
    await expect(page).toHaveURL(/\/register\/check-email$/);
    expect(await countUsers(email)).toBe(1);
  });
});
