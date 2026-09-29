import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken, makeSuperadmin } from "../lib/db";

// Front epic F9 — User admin (issue #24): the superadmin support screens.
// Read-only look-up of every family (/admin/groups) and every account
// (/admin/users) across all tenants — the one gated exception to the RLS
// boundary — plus the immediate `deactivate` action on a user and its
// `reactivate` counterpart (#256). The whole
// /admin tree is gated: the nav link and the pages render only for a
// superadmin; an authenticated non-superadmin is bounced to `/`. See
// docs/front-epic-9-user-admin.md.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-admin-password-1";

/** Register + verify + login a fresh user on the given page; returns the email. */
async function registerAndLogin(page: Page, prefix: string, displayName: string): Promise<string> {
  const email = uniqueEmail(prefix);
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill(displayName);
  await page.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
  await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
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

/** Registers a fresh superadmin (promoted via DB) on a new browser context. */
async function newSuperadmin(
  browser: import("@playwright/test").Browser,
  prefix: string,
  displayName: string,
): Promise<{ page: Page; email: string }> {
  const context = await browser.newContext();
  const page = await context.newPage();
  const email = await registerAndLogin(page, prefix, displayName);
  await makeSuperadmin(email);
  return { page, email };
}

test.describe("User admin — access gate", () => {
  test("a non-superadmin sees no Admin nav link and is bounced from /admin", async ({ page }) => {
    await registerAndLogin(page, "e2e-admin-plain", "Plain User");

    // No door on the home page.
    await page.goto("/");
    await expect(page.getByRole("link", { name: "Admin" })).toHaveCount(0);

    // Direct navigation is redirected to the home page (never revealed, not
    // even as a 403), for every /admin route.
    await page.goto("/admin/users");
    await expect(page).toHaveURL("/");
    await page.goto("/admin/groups");
    await expect(page).toHaveURL("/");
  });

  test("an unauthenticated visitor hitting /admin is redirected to /login", async ({ page }) => {
    await page.goto("/admin/users");
    await expect(page).toHaveURL(/\/login$/);
  });
});

test.describe("User admin — support look-up", () => {
  test("a superadmin sees the Admin nav and every family/account across tenants", async ({
    browser,
  }) => {
    // Two independent families, each owned by a different user.
    const ownerCtxA = await browser.newContext();
    const ownerA = await ownerCtxA.newPage();
    const ownerAEmail = await registerAndLogin(ownerA, "e2e-admin-ownera", "Owner A");
    const familyA = `Famille Alpha ${Date.now()}`;
    await createGroup(ownerA, familyA);

    const ownerCtxB = await browser.newContext();
    const ownerB = await ownerCtxB.newPage();
    const ownerBEmail = await registerAndLogin(ownerB, "e2e-admin-ownerb", "Owner B");
    const familyB = `Famille Beta ${Date.now()}`;
    await createGroup(ownerB, familyB);

    // The superadmin is a member of neither family.
    const { page } = await newSuperadmin(browser, "e2e-admin-super", "Super Admin");

    // The Admin door appears now.
    await page.goto("/");
    await expect(page.getByRole("link", { name: "Admin" })).toBeVisible();

    // Groups screen: both families are visible despite no membership.
    await page.goto("/admin/groups");
    await expect(page.getByRole("heading", { name: "Administration — Familles" })).toBeVisible();
    await expect(page.locator("tr", { hasText: familyA })).toHaveCount(1);
    await expect(page.locator("tr", { hasText: familyB })).toHaveCount(1);

    // Users screen: both owners' accounts are visible.
    await page.goto("/admin/users");
    await expect(page.getByRole("heading", { name: "Administration — Utilisateurs" })).toBeVisible();
    await expect(page.locator("tr", { hasText: ownerAEmail })).toHaveCount(1);
    await expect(page.locator("tr", { hasText: ownerBEmail })).toHaveCount(1);
    // A never-deactivated account reads as Actif.
    await expect(page.locator("tr", { hasText: ownerAEmail })).toContainText("Actif");
  });

  test("an unknown user id shows the not-found page", async ({ browser }) => {
    const { page } = await newSuperadmin(browser, "e2e-admin-super404", "Super NotFound");
    await page.goto(`/admin/users/${crypto.randomUUID()}`);
    await expect(page.getByRole("heading", { name: "Utilisateur introuvable" })).toBeVisible();
  });
});

test.describe("User admin — deactivate", () => {
  test("deactivating a user revokes their session until a reactivation", async ({ browser }) => {
    // Two browsers and five password logins (#289). The tripled budget is a
    // margin, not a measured need: no run of this test has been traced
    // past the default 30 s.
    test.slow();
    // A target user with a live session.
    const targetCtx = await browser.newContext();
    const target = await targetCtx.newPage();
    const targetEmail = await registerAndLogin(target, "e2e-admin-target", "Target User");
    // Prove the target's session works first.
    await target.goto("/");
    await expect(target).toHaveURL("/");

    // The superadmin finds the target and opens their detail via the row link.
    const { page } = await newSuperadmin(browser, "e2e-admin-superdeact", "Super Deact");
    await page.goto("/admin/users");
    const targetRow = page.locator("tr", { hasText: targetEmail });
    await expect(targetRow).toContainText("Actif");
    await targetRow.getByRole("link", { name: "Détails" }).click();
    await expect(page.getByRole("heading", { name: targetEmail })).toBeVisible();

    // Confirm the native confirm() dialog and deactivate.
    page.on("dialog", (d) => d.accept());
    await page.getByRole("button", { name: "Désactiver le compte" }).click();

    await expect(page).toHaveURL("/admin/users?notice=user_deactivated");
    await expect(page.getByText("Compte désactivé", { exact: false })).toBeVisible();
    // The row now reads Désactivé.
    await expect(page.locator("tr", { hasText: targetEmail })).toContainText("Désactivé");

    // The target's existing session is now revoked: any page bounces to /login.
    await target.goto("/");
    await expect(target).toHaveURL(/\/login$/);

    // #289: a wrong password on the deactivated account gets the generic
    // message; the right one lands on the deactivated-account page, and
    // every other page sends the holder back there.
    await target.getByLabel("Email").fill(targetEmail);
    await target.getByRole("textbox", { name: "Mot de passe" }).fill("not-the-password-1");
    await target.getByRole("button", { name: "Se connecter" }).click();
    await expect(target.getByText("Email ou mot de passe incorrect.")).toBeVisible();
    await target.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
    await target.getByRole("button", { name: "Se connecter" }).click();
    await expect(target).toHaveURL("/account/deactivated");
    await expect(target.getByRole("heading", { name: "Compte désactivé" })).toBeVisible();
    await target.goto("/groups");
    await expect(target).toHaveURL("/account/deactivated");

    // The holder asks for the reactivation, once.
    await target.getByLabel("Message pour l'administrateur (facultatif)").fill("Je souhaite revenir.");
    await target.getByRole("button", { name: "Demander la réactivation" }).click();
    await expect(target).toHaveURL("/account/deactivated?notice=reactivation_requested");
    await expect(target.getByText("attend la décision de l'administrateur")).toBeVisible();
    await expect(target.getByRole("button", { name: "Demander la réactivation" })).toHaveCount(0);

    // The holder logs out from that page, then back in to it.
    await target.getByRole("button", { name: "Se déconnecter" }).click();
    await expect(target).toHaveURL(/\/login$/);
    await target.goto("/groups");
    await expect(target).toHaveURL(/\/login$/);
    await target.getByLabel("Email").fill(targetEmail);
    await target.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
    await target.getByRole("button", { name: "Se connecter" }).click();
    await expect(target).toHaveURL("/account/deactivated");

    // The superadmin sees the pending request, with its note. A deactivated
    // account offers no second deactivation (the backend would 404 it), only
    // its reactivation (#256), which grants the request.
    await page.goto("/admin/users");
    await expect(page.locator("tr", { hasText: targetEmail })).toContainText("réactivation demandée");
    await page.locator("tr", { hasText: targetEmail }).getByRole("link", { name: "Détails" }).click();
    await expect(page.getByRole("heading", { name: "Demande de réactivation" })).toBeVisible();
    await expect(page.getByText("Je souhaite revenir.")).toBeVisible();
    await expect(page.getByRole("button", { name: "Refuser la demande" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Désactiver le compte" })).toHaveCount(0);
    await page.getByRole("button", { name: "Réactiver le compte" }).click();

    await expect(page).toHaveURL("/admin/users?notice=user_reactivated");
    await expect(page.getByText("Compte réactivé", { exact: false })).toBeVisible();
    await expect(page.locator("tr", { hasText: targetEmail })).toContainText("Actif");

    // The restricted session ends with the reactivation; the holder logs in
    // again.
    await target.goto("/account/deactivated");
    await expect(target).toHaveURL(/\/login$/);
    await target.goto("/login");
    await target.getByLabel("Email").fill(targetEmail);
    await target.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
    await target.getByRole("button", { name: "Se connecter" }).click();
    await expect(target).toHaveURL("/");
  });
});
