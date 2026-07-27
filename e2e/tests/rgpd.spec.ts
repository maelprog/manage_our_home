import { readFileSync } from "node:fs";
import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Front epic F10 — RGPD self-service (issue #25): every user journey the epic
// introduces, happy paths plus the documented error states from
// apps/api/src/rgpd/ and apps/api/src/auth/mod.rs::delete_account /
// cancel_delete_account (see docs/front-epic-10-rgpd.md).
//
// Three screens: the account hub (/account), the data export download
// (/account/export, Art. 20) and the grace-period deletion flow
// (/account/delete, Art. 17) — plus the public /privacy-policy page, the only
// non-auth page besides the auth entry points, reachable from the login and
// register footers.
//
// Deletion here is self-service and *deferred* (deletion_requested_at + a 30-day
// grace period, cancellable), not F9's immediate superadmin deactivate — the
// assertions below pin that copy so the two can't drift back together.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-rgpd-password-1";

/** Register + verify + login a fresh user on the given page; returns the email. */
async function registerAndLogin(page: Page, prefix: string, displayName: string): Promise<string> {
  const email = uniqueEmail(prefix);
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill(displayName);
  await page.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
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

/**
 * Fills the deletion confirmation on /account/delete and submits it. The native
 * confirm() is progressive enhancement (the form posts fine without JS), so the
 * dialog is auto-accepted here.
 */
async function requestDeletion(page: Page, password: string | null, consent: boolean) {
  page.once("dialog", (d) => d.accept());
  if (password !== null) {
    await page.getByRole("textbox", { name: "Mot de passe actuel" }).fill(password);
  }
  if (consent) {
    await page.getByRole("checkbox").check();
  }
  await page.getByRole("button", { name: "Demander la suppression de mon compte" }).click();
}

test.describe("RGPD — privacy policy (public)", () => {
  test("is reachable without a session from the login and register footers", async ({ page }) => {
    await page.goto("/login");
    await page.getByRole("link", { name: "Politique de confidentialité" }).click();
    await expect(page).toHaveURL(/\/privacy-policy$/);

    // Rendered from the markdown apps/api serves (docs/privacy-policy.md):
    // headings, the legal-basis table and the bullet lists all become HTML, and
    // no raw markdown survives.
    await expect(
      page.getByRole("heading", { name: /Politique de confidentialité/ }),
    ).toBeVisible();
    await expect(page.getByRole("heading", { name: "Vos droits" })).toBeVisible();
    await expect(page.locator("article.prose table")).toBeVisible();
    await expect(page.locator("article.prose td", { hasText: "Messagerie" })).toBeVisible();
    await expect(page.locator("article.prose")).not.toContainText("**");
    // An anonymous visitor gets a way back into the app.
    await expect(page.getByRole("link", { name: /Retour à la connexion/ })).toBeVisible();

    await page.goto("/register");
    await page.getByRole("link", { name: "Politique de confidentialité" }).click();
    await expect(page).toHaveURL(/\/privacy-policy$/);
  });
});

test.describe("RGPD — account hub gate", () => {
  test("an unauthenticated visitor is redirected to /login on every account route", async ({
    page,
  }) => {
    for (const path of ["/account", "/account/export", "/account/export/download", "/account/delete"]) {
      await page.goto(path);
      await expect(page).toHaveURL(/\/login$/);
    }
  });
});

test.describe("RGPD — data export", () => {
  test("downloads a dated JSON file containing the user's own data", async ({ page }) => {
    const email = await registerAndLogin(page, "e2e-rgpd-export", "Export User");
    const family = `Foyer Portabilite ${Date.now()}`;
    await createGroup(page, family);

    // Reached from the header's "Mon compte" link, then the hub.
    await page.goto("/");
    await page.getByRole("link", { name: "Mon compte" }).click();
    await expect(page).toHaveURL("/account");
    await page.getByRole("link", { name: "Exporter mes données" }).click();
    await expect(page).toHaveURL("/account/export");

    const download = await Promise.all([
      page.waitForEvent("download"),
      page.getByRole("link", { name: "Télécharger mes données (JSON)" }).click(),
    ]).then(([d]) => d);

    expect(download.suggestedFilename()).toMatch(
      /^mes-donnees-manage-our-home-\d{4}-\d{2}-\d{2}\.json$/,
    );

    const path = await download.path();
    if (!path) throw new Error("no downloaded file on disk");
    const doc = JSON.parse(readFileSync(path, "utf8"));
    expect(doc.profile.email).toBe(email);
    expect(doc.group_memberships[0].name).toBe(family);
    expect(doc.group_memberships[0].role).toBe("owner");
    // Categories are always present, even when empty — the shape is stable.
    expect(Array.isArray(doc.messages)).toBe(true);
  });
});

test.describe("RGPD — account deletion", () => {
  test("the sole owner of a family is blocked with actionable guidance, then can delete", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-rgpd-owner", "Owner To Delete");
    const family = `Foyer Bloquant ${Date.now()}`;
    await createGroup(page, family);

    await page.goto("/account/delete");
    await requestDeletion(page, PASSWORD, true);

    // 409 owner_of_groups → the block, the blocking family by name, and the two
    // ways out (transfer or delete) — never a raw error.
    await expect(page.getByText("Suppression impossible pour le moment")).toBeVisible();
    const blocked = page.locator("section.notice.error");
    await expect(blocked).toContainText(family);
    await expect(blocked).toContainText("transférez la propriété");

    // The guidance link leads to that family's settings, where deleting it
    // clears the block.
    await blocked.getByRole("link", { name: "gérer cette famille" }).click();
    await expect(page).toHaveURL(/\/groups\/[0-9a-f-]+\/settings$/);
    await page.getByRole("button", { name: "Supprimer le groupe" }).click();
    await expect(page).toHaveURL(/\/groups\?notice=group_deleted$/);

    await page.goto("/account/delete");
    await requestDeletion(page, PASSWORD, true);
    await expect(page).toHaveURL("/account?notice=deletion_requested");
    await expect(page.getByText("Demande de suppression enregistrée")).toBeVisible();
  });

  test("requesting deletion schedules it with a grace period, and it can be cancelled", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-rgpd-delete", "Deleting User");

    await page.goto("/account");
    await page.getByRole("link", { name: "Supprimer mon compte" }).click();
    await expect(page).toHaveURL("/account/delete");
    // Deferred, not immediate — the copy must not read like F9's deactivate.
    await expect(page.getByText("La suppression est programmée, pas immédiate")).toBeVisible();

    await requestDeletion(page, PASSWORD, true);
    await expect(page).toHaveURL("/account?notice=deletion_requested");

    // The pending panel replaces the request entry point and states both dates.
    const panel = page.locator("section.notice.error");
    await expect(panel).toContainText("Suppression de compte programmée");
    await expect(panel).toContainText(/Demandée le \d{2}\/\d{2}\/\d{4} à \d{2}:\d{2}/);
    await expect(panel).toContainText(/anonymisé le \d{2}\/\d{2}\/\d{4}/);
    await expect(page.getByRole("link", { name: "Supprimer mon compte" })).toHaveCount(0);

    // The session keeps working during the grace period (the account is only
    // *scheduled* for deletion), and /account/delete shows the same panel rather
    // than a second request form.
    await page.goto("/account/delete");
    await expect(page.getByRole("button", { name: "Demander la suppression de mon compte" })).toHaveCount(0);
    await page.getByRole("button", { name: "Annuler la demande de suppression" }).click();

    await expect(page).toHaveURL("/account?notice=deletion_cancelled");
    await expect(page.getByText("votre compte reste actif")).toBeVisible();
    // Back to the request entry point.
    await expect(page.getByRole("link", { name: "Supprimer mon compte" })).toBeVisible();
  });

  test("the confirmation is guarded: consent required, password required, wrong password", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-rgpd-guard", "Guarded User");
    await page.goto("/account/delete");

    // Consent box not ticked — the checkbox carries no `required` attribute (the
    // guard is deliberately server-side, so a no-JS post is caught too), so the
    // form does submit and `validate_deletion_confirmation` rejects it locally,
    // without ever calling the API.
    await requestDeletion(page, PASSWORD, false);
    await expect(page.getByText("Cochez la case de confirmation")).toBeVisible();

    // Consent ticked but no password (the account has one). The password input is
    // `required`, so the browser's native constraint blocks submission *before*
    // the onsubmit confirm() — the form never posts, and the server-side
    // "Saisissez votre mot de passe actuel" branch is unreachable from a browser
    // by design (it stays the authority for no-JS and forged posts, and is
    // unit-tested in apps/shared as `a_password_account_must_type_its_password`).
    const password = page.getByRole("textbox", { name: "Mot de passe actuel" });
    await expect(password).toHaveJSProperty("required", true);
    await page.getByRole("checkbox").check();
    await page.getByRole("button", { name: "Demander la suppression de mon compte" }).click();
    await expect(password).toHaveJSProperty("validity.valueMissing", true);
    // Still on the previous render — nothing was submitted.
    await expect(page.getByText("Cochez la case de confirmation")).toBeVisible();

    // Wrong password → the backend's 401, surfaced on the form.
    await requestDeletion(page, "not-the-right-password", true);
    await expect(page.getByText("Mot de passe incorrect.")).toBeVisible();

    // Still no request recorded: the entry point is unchanged on the hub.
    await page.goto("/account");
    await expect(page.getByRole("link", { name: "Supprimer mon compte" })).toBeVisible();
    await expect(page.getByText("Suppression de compte programmée")).toHaveCount(0);
  });
});
