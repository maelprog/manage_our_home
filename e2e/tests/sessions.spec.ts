import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Issue #225 — the member's live sessions (/account/sessions): see them,
// end one, end them all. Dates only, no device information (arbitrated
// 2026-10-02), and the global button ends this session too: the member is
// sent back to the login page.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-sessions-password-1";

async function login(page: Page, email: string): Promise<void> {
  await page.goto("/login");
  await page.getByLabel("Email").fill(email);
  await page.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
  await page.getByRole("button", { name: "Se connecter" }).click();
  await expect(page).toHaveURL("/");
}

/** Register + verify + login a fresh user on the given page; returns the email. */
async function registerAndLogin(page: Page, prefix: string): Promise<string> {
  const email = uniqueEmail(prefix);
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill("Sessions E2E");
  await page.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
  await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
  await page.getByRole("button", { name: "Créer mon compte" }).click();
  await expect(page).toHaveURL(/\/register\/check-email$/);
  const token = await fetchVerificationToken(email);
  await page.goto(`/verify-email?token=${token}`);
  await login(page, email);
  return email;
}

test.describe("Account — active sessions (#225)", () => {
  test("lists this session and another, ends the other, then ends them all", async ({
    page,
    browser,
  }) => {
    const email = await registerAndLogin(page, "sessions");
    const elsewhere = await browser.newContext();
    const other = await elsewhere.newPage();
    await login(other, email);

    await page.goto("/account");
    await page.getByRole("link", { name: "Gérer mes sessions" }).click();
    await expect(page).toHaveURL(/\/account\/sessions$/);
    await expect(page.getByRole("heading", { name: "Sessions actives", level: 1 })).toBeVisible();

    const rows = page.locator("li.list-row");
    await expect(rows).toHaveCount(2);
    await expect(rows.first()).toContainText("Cette session");
    await expect(page.getByText("ni l'adresse IP ni le navigateur")).toBeVisible();

    // The other session's own button, named by its opening date.
    const revoke = page.getByRole("button", { name: /^Déconnecter la session ouverte le / });
    await expect(revoke).toHaveCount(1);
    await revoke.click();
    await expect(page).toHaveURL(/\/account\/sessions\?notice=session_revoked$/);
    await expect(page.getByText("La session a été déconnectée.")).toBeVisible();
    await expect(rows).toHaveCount(1);

    // The ended session is logged out on its next page load.
    await other.goto("/account");
    await expect(other).toHaveURL(/\/login$/);

    // A third session, then the global button: every session ends, this
    // one included.
    await login(other, email);
    await page.reload();
    await expect(rows).toHaveCount(2);
    await page.getByRole("button", { name: "Déconnecter toutes les sessions" }).click();
    await expect(page).toHaveURL(/\/login$/);
    await page.goto("/account");
    await expect(page).toHaveURL(/\/login$/);
    await other.goto("/account");
    await expect(other).toHaveURL(/\/login$/);

    await elsewhere.close();
  });
});
