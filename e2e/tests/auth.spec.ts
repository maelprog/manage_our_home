import { expect, test } from "@playwright/test";
import { fetchPasswordResetToken, fetchVerificationToken } from "../lib/db";

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

test.describe("Auth — register → verify → login → logout", () => {
  test("full journey", async ({ page }) => {
    const email = uniqueEmail("e2e-register");
    const password = "e2e-password-1";

    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("E2E User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("button", { name: "Créer mon compte" }).click();

    await expect(page).toHaveURL(/\/register\/check-email$/);
    await expect(page.getByText("Un email de confirmation vous a été envoyé")).toBeVisible();

    const token = await fetchVerificationToken(email);
    await page.goto(`/verify-email?token=${token}`);
    await expect(page.getByText("Email vérifié")).toBeVisible();

    await page.goto("/login");
    await page.getByLabel("Email").fill(email);
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("button", { name: "Se connecter" }).click();

    await expect(page).toHaveURL("/");
    await expect(page.getByText("Bienvenue")).toBeVisible();
    await expect(page.getByText("E2E User")).toBeVisible();

    await page.getByRole("button", { name: "Se déconnecter" }).click();
    await expect(page).toHaveURL(/\/login$/);
  });

  test("wrong password shows a single generic error", async ({ page }) => {
    const email = uniqueEmail("e2e-badlogin");
    const password = "e2e-password-2";

    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("Bad Login User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("button", { name: "Créer mon compte" }).click();
    const token = await fetchVerificationToken(email);
    await page.goto(`/verify-email?token=${token}`);

    await page.goto("/login");
    await page.getByLabel("Email").fill(email);
    await page.getByRole("textbox", { name: "Mot de passe" }).fill("totally-wrong");
    await page.getByRole("button", { name: "Se connecter" }).click();

    await expect(page.getByText("Email ou mot de passe incorrect.")).toBeVisible();
  });

  test("duplicate email registration shows an inline field error", async ({ page }) => {
    const email = uniqueEmail("e2e-dup");
    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("Dup User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill("password-one");
    await page.getByRole("button", { name: "Créer mon compte" }).click();
    await expect(page).toHaveURL(/\/register\/check-email$/);

    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("Dup User 2");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill("password-two");
    await page.getByRole("button", { name: "Créer mon compte" }).click();

    await expect(page.getByText("Un compte existe déjà avec cet email.")).toBeVisible();
  });
});

test.describe("Auth — forgot → reset → login with new password", () => {
  test("full journey", async ({ page }) => {
    const email = uniqueEmail("e2e-reset");
    const oldPassword = "e2e-old-password-1";
    const newPassword = "e2e-new-password-2";

    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("Reset User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(oldPassword);
    await page.getByRole("button", { name: "Créer mon compte" }).click();
    const verifyToken = await fetchVerificationToken(email);
    await page.goto(`/verify-email?token=${verifyToken}`);

    await page.goto("/forgot-password");
    await page.getByLabel("Email").fill(email);
    await page.getByRole("button", { name: "Envoyer le lien de réinitialisation" }).click();
    // Anti-enumeration: identical message regardless of whether the
    // account exists.
    await expect(page.getByText("Si ce compte existe, un email a été envoyé.")).toBeVisible();

    const resetToken = await fetchPasswordResetToken(email);
    await page.goto(`/reset-password?token=${resetToken}`);
    await page.getByLabel("Nouveau mot de passe").fill(newPassword);
    await page.getByRole("button", { name: "Réinitialiser" }).click();
    await expect(page.getByText("Mot de passe mis à jour")).toBeVisible();

    await page.goto("/login");
    await page.getByLabel("Email").fill(email);
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(newPassword);
    await page.getByRole("button", { name: "Se connecter" }).click();
    await expect(page).toHaveURL("/");
  });
});

test.describe("Auth-gate redirects", () => {
  test("unauthenticated visitor hitting a non-auth route is redirected to /login", async ({
    page,
  }) => {
    await page.goto("/");
    await expect(page).toHaveURL(/\/login$/);
  });

  test("authenticated visitor hitting /login or /register is redirected to /", async ({
    page,
  }) => {
    const email = uniqueEmail("e2e-redirect");
    const password = "e2e-password-3";

    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("Redirect User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("button", { name: "Créer mon compte" }).click();
    const token = await fetchVerificationToken(email);
    await page.goto(`/verify-email?token=${token}`);

    await page.goto("/login");
    await page.getByLabel("Email").fill(email);
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("button", { name: "Se connecter" }).click();
    await expect(page).toHaveURL("/");

    await page.goto("/login");
    await expect(page).toHaveURL("/");

    await page.goto("/register");
    await expect(page).toHaveURL("/");
  });
});

// Google OAuth E2E: best-effort skipped. There is no test Google OAuth
// client/credentials available in this environment (real Google consent
// screen, no sandboxed provider), and apps/api's flow talks to Google's
// live userinfo endpoint (apps/api/src/auth/oauth_google.rs) with no mock
// seam today. A real round-trip test would need either a recorded/stubbed
// OAuth provider or dedicated test Google credentials, neither of which
// exist yet — tracked as follow-up, not blocking this epic.
test.describe("Auth — Google OAuth", () => {
  test.skip(
    true,
    "No test Google OAuth provider/credentials available in this environment; " +
      "apps/api/src/auth/oauth_google.rs talks to Google's live endpoints with no mock seam.",
  );
  test("continuing with Google reaches an authenticated session", async () => {
    // Intentionally left unimplemented — see the module-level skip reason.
  });
});
