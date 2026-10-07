import { BrowserContext, expect, Page, test } from "@playwright/test";
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

/**
 * The session cookies the browser still holds: `session_id`, or
 * `__Host-session_id` under SECURE_COOKIES. The removal apps/api sends
 * (`Max-Age=0`) drops the cookie from the jar once apps/web relays it.
 * Logged out server-side yet still holding the cookie is what this catches
 * (#368): the redirect to /login alone does not tell.
 */
async function sessionCookies(context: BrowserContext): Promise<string[]> {
  return (await context.cookies())
    .filter((c) => /(^|-)session_id$/.test(c.name))
    .map((c) => c.name);
}

/** Register + verify + login a fresh user on the given page; returns the email. */
async function registerAndLogin(page: Page, prefix: string): Promise<string> {
  const email = uniqueEmail(prefix);
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill("Sessions E2E");
  await page.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
  await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
  await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
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
    context,
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
    expect(await sessionCookies(context)).toHaveLength(1);
    await page.getByRole("button", { name: "Déconnecter toutes les sessions" }).click();
    await expect(page).toHaveURL(/\/login$/);
    // apps/api's removal of the cookie reached the browser (#368).
    expect(await sessionCookies(context)).toEqual([]);
    await page.goto("/account");
    await expect(page).toHaveURL(/\/login$/);
    await other.goto("/account");
    await expect(other).toHaveURL(/\/login$/);

    await elsewhere.close();
  });

  test("ending this very session clears its cookie in the browser (#368)", async ({
    page,
    context,
  }) => {
    await registerAndLogin(page, "sessions-self");
    await page.goto("/account/sessions");
    const current = page.locator("li.list-row", { hasText: "Cette session" });
    await expect(current).toHaveCount(1);
    const id = await current.getAttribute("data-session");
    expect(id).toMatch(/^[0-9a-f-]{36}$/);
    expect(await sessionCookies(context)).toHaveLength(1);

    // The page offers no button for this session, but a request can name
    // it: the form the other rows carry, posted from this page.
    await page.evaluate((sessionId) => {
      const form = document.createElement("form");
      form.method = "post";
      form.action = `/account/sessions/${sessionId}/revoke`;
      document.body.appendChild(form);
      form.submit();
    }, id);
    await expect(page).toHaveURL(/\/login$/);
    expect(await sessionCookies(context)).toEqual([]);
  });
});
