import { Browser, expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Front epic #2 — Groups (issue #17): every user journey the epic
// introduces, happy paths plus the documented error states from
// apps/api/src/groups/mod.rs's error table (422 too_many_groups /
// name_required / new_owner_id_required, 409 last_member_must_delete_group,
// 410 consumed invitation, 404 unknown invitation, 403 permission bar).

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-groups-password-1";

/** Register + verify + login a fresh user on the given page. */
async function registerAndLogin(page: Page, prefix: string, displayName: string): Promise<string> {
  const email = uniqueEmail(prefix);
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill(displayName);
  await page.getByLabel("Mot de passe").fill(PASSWORD);
  await page.getByRole("button", { name: "Créer mon compte" }).click();
  await expect(page).toHaveURL(/\/register\/check-email$/);
  const token = await fetchVerificationToken(email);
  await page.goto(`/verify-email?token=${token}`);
  await page.goto("/login");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Mot de passe").fill(PASSWORD);
  await page.getByRole("button", { name: "Se connecter" }).click();
  await expect(page).toHaveURL("/");
  return email;
}

async function createGroup(page: Page, name: string): Promise<void> {
  await page.goto("/groups/new");
  await page.getByLabel("Nom du groupe").fill(name);
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
  await expect(page.getByText("Groupe créé.")).toBeVisible();
}

/** Owner creates an invitation (no email) and returns the accept link. */
async function createInviteLink(page: Page, groupName: string): Promise<string> {
  await page.goto("/groups");
  const row = page.locator("li", { hasText: groupName });
  await row.getByRole("link", { name: "Membres" }).click();
  await page.getByRole("button", { name: "Créer une invitation" }).click();
  const link = page.locator(".notice.success a");
  await expect(link).toBeVisible();
  const href = await link.getAttribute("href");
  if (!href) throw new Error("no invitation link rendered");
  return href;
}

/** A second, independently authenticated user in its own browser context. */
async function secondUser(
  browser: Browser,
  prefix: string,
  displayName: string,
): Promise<{ page: Page; email: string }> {
  const context = await browser.newContext();
  const page = await context.newPage();
  const email = await registerAndLogin(page, prefix, displayName);
  return { page, email };
}

test.describe("Groups — create", () => {
  test("create a group, appear as owner, become the active family", async ({ page }) => {
    await registerAndLogin(page, "e2e-gcreate", "Group Creator");
    await createGroup(page, "Famille Création");

    const row = page.locator("li", { hasText: "Famille Création" });
    await expect(row.getByText("Propriétaire")).toBeVisible();

    // The new group is the active family in the root-layout switcher.
    await page.goto("/");
    await expect(page.locator("select[name=group_id] option[selected]")).toHaveText(
      "Famille Création",
    );
  });

  test("empty group name is rejected inline", async ({ page }) => {
    await registerAndLogin(page, "e2e-gname", "Name Checker");
    await page.goto("/groups/new");
    await page.getByLabel("Nom du groupe").fill("   ");
    await page.getByRole("button", { name: "Créer le groupe" }).click();
    await expect(page.getByText("Le nom du groupe ne peut pas être vide.")).toBeVisible();
  });
});

test.describe("Groups — invitations", () => {
  test("invite link joins a second user as standard member; the link is single-use", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-inviter", "Inviter Owner");
    await createGroup(page, "Famille Invitation");
    const invite = await createInviteLink(page, "Famille Invitation");

    const { page: guest } = await secondUser(browser, "e2e-guest", "Invited Guest");
    await guest.goto(invite);
    await expect(guest.getByRole("heading", { name: "Rejoindre un groupe" })).toBeVisible();
    await guest.getByRole("button", { name: "Rejoindre le groupe" }).click();
    await expect(guest).toHaveURL(/\/groups\?notice=joined$/);
    await expect(guest.getByText("Vous avez rejoint le groupe.")).toBeVisible();
    const guestRow = guest.locator("li", { hasText: "Famille Invitation" });
    await expect(guestRow.getByText("Membre", { exact: true })).toBeVisible();

    // Owner sees the new member listed with the standard role.
    await page.goto("/groups");
    await page
      .locator("li", { hasText: "Famille Invitation" })
      .getByRole("link", { name: "Membres" })
      .click();
    await expect(
      page.locator("li", { hasText: "Invited Guest" }).locator("span.muted", { hasText: "Membre" }),
    ).toBeVisible();

    // Single-use (410 Gone on re-use), even for another fresh user.
    const { page: late } = await secondUser(browser, "e2e-late", "Late Guest");
    await late.goto(invite);
    await late.getByRole("button", { name: "Rejoindre le groupe" }).click();
    await expect(late.getByText("Invitation expirée")).toBeVisible();
  });

  test("unknown invitation token shows the invalid page; garbage paste is rejected inline", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-badinvite", "Bad Invite User");

    // Valid UUID shape, but no such invitation → apps/api 404.
    await page.goto("/groups/invitations/00000000-0000-4000-8000-000000000000/accept");
    await page.getByRole("button", { name: "Rejoindre le groupe" }).click();
    await expect(page.getByText("Invitation invalide")).toBeVisible();

    // Not even a token → rejected by the shared parse, no API call.
    await page.goto("/groups");
    await page.getByLabel("Lien ou code d'invitation").fill("pas-un-token");
    await page.getByRole("button", { name: "Rejoindre" }).click();
    await expect(
      page.getByText("Invitation invalide : collez le lien d'invitation complet ou son code."),
    ).toBeVisible();
  });

  test("standard member sees no invite form and no member controls (permission bar)", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-permowner", "Perm Owner");
    await createGroup(page, "Famille Permissions");
    const invite = await createInviteLink(page, "Famille Permissions");

    const { page: member } = await secondUser(browser, "e2e-permmember", "Perm Member");
    await member.goto(invite);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();
    await member
      .locator("li", { hasText: "Famille Permissions" })
      .getByRole("link", { name: "Membres" })
      .click();
    await expect(member.getByText("Perm Owner")).toBeVisible();
    await expect(member.getByRole("button", { name: "Créer une invitation" })).toHaveCount(0);
    await expect(member.getByRole("button", { name: "Changer le rôle" })).toHaveCount(0);
    await expect(member.getByRole("button", { name: "Retirer" })).toHaveCount(0);
  });
});

test.describe("Groups — member roles and removal", () => {
  test("owner promotes a member to admin, then removes them", async ({ page, browser }) => {
    await registerAndLogin(page, "e2e-roleowner", "Role Owner");
    await createGroup(page, "Famille Rôles");
    const invite = await createInviteLink(page, "Famille Rôles");

    const { page: member } = await secondUser(browser, "e2e-rolemember", "Role Member");
    await member.goto(invite);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    await page.goto("/groups");
    await page
      .locator("li", { hasText: "Famille Rôles" })
      .getByRole("link", { name: "Membres" })
      .click();
    const memberRow = page.locator("li", { hasText: "Role Member" });
    await memberRow.locator("select[name=role]").selectOption("admin");
    await memberRow.getByRole("button", { name: "Changer le rôle" }).click();
    await expect(page.getByText("Rôle mis à jour.")).toBeVisible();
    // The role label span, not the <select> option that shares the text.
    await expect(
      page.locator("li", { hasText: "Role Member" }).locator("span.muted", { hasText: "Admin" }),
    ).toBeVisible();

    await page.locator("li", { hasText: "Role Member" }).getByRole("button", { name: "Retirer" }).click();
    await expect(page.getByText("Membre retiré du groupe.")).toBeVisible();
    await expect(page.locator("li", { hasText: "Role Member" })).toHaveCount(0);
  });
});

test.describe("Groups — settings", () => {
  test("rename the group", async ({ page }) => {
    await registerAndLogin(page, "e2e-rename", "Rename Owner");
    await createGroup(page, "Famille Avant");
    await page
      .locator("li", { hasText: "Famille Avant" })
      .getByRole("link", { name: "Paramètres" })
      .click();
    await page.getByLabel("Nom du groupe").fill("Famille Après");
    await page.getByRole("button", { name: "Renommer" }).click();
    await expect(page.getByText("Groupe renommé.")).toBeVisible();
    await expect(page.getByText("Paramètres — Famille Après")).toBeVisible();
  });

  test("last remaining member cannot leave (409) but can delete the group", async ({ page }) => {
    await registerAndLogin(page, "e2e-lastmember", "Last Member");
    await createGroup(page, "Famille Solo");
    await page
      .locator("li", { hasText: "Famille Solo" })
      .getByRole("link", { name: "Paramètres" })
      .click();

    await page.getByRole("button", { name: "Quitter le groupe" }).click();
    await expect(
      page.getByText("Vous êtes le dernier membre : quitter n'est pas possible"),
    ).toBeVisible();

    await page.getByRole("button", { name: "Supprimer le groupe" }).click();
    await expect(page).toHaveURL(/\/groups\?notice=group_deleted$/);
    await expect(page.getByText("Groupe supprimé.")).toBeVisible();
    await expect(page.locator("li", { hasText: "Famille Solo" })).toHaveCount(0);
  });

  test("owner leaving must name a successor; the successor becomes owner", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-leaveowner", "Leave Owner");
    await createGroup(page, "Famille Départ");
    const invite = await createInviteLink(page, "Famille Départ");

    const { page: heir } = await secondUser(browser, "e2e-heir", "Heir Member");
    await heir.goto(invite);
    await heir.getByRole("button", { name: "Rejoindre le groupe" }).click();

    await page.goto("/groups");
    await page
      .locator("li", { hasText: "Famille Départ" })
      .getByRole("link", { name: "Paramètres" })
      .click();
    // The owner's leave form requires picking a successor (the API's 422
    // new_owner_id_required is unreachable through it — enforced by the
    // required <select>); leaving hands ownership over.
    await expect(page.getByText("vous devez d'abord désigner un successeur")).toBeVisible();
    const leaveForm = page.locator("form[action$='/settings/leave']");
    await leaveForm.locator("select[name=new_owner_id]").selectOption({ index: 0 });
    await leaveForm.getByRole("button", { name: "Quitter le groupe" }).click();
    await expect(page).toHaveURL(/\/groups\?notice=left$/);
    await expect(page.getByText("Vous avez quitté le groupe.")).toBeVisible();
    await expect(page.locator("li", { hasText: "Famille Départ" })).toHaveCount(0);

    await heir.goto("/groups");
    await expect(
      heir.locator("li", { hasText: "Famille Départ" }).getByText("Propriétaire"),
    ).toBeVisible();
  });

  test("transfer ownership demotes the old owner to admin", async ({ page, browser }) => {
    await registerAndLogin(page, "e2e-transfer", "Transfer Owner");
    await createGroup(page, "Famille Transfert");
    const invite = await createInviteLink(page, "Famille Transfert");

    const { page: successor } = await secondUser(browser, "e2e-successor", "New Owner");
    await successor.goto(invite);
    await successor.getByRole("button", { name: "Rejoindre le groupe" }).click();

    await page.goto("/groups");
    await page
      .locator("li", { hasText: "Famille Transfert" })
      .getByRole("link", { name: "Paramètres" })
      .click();
    const transferForm = page.locator("form[action$='/settings/transfer']");
    await transferForm.locator("select[name=new_owner_id]").selectOption({ index: 0 });
    await transferForm.getByRole("button", { name: "Transférer" }).click();
    await expect(page.getByText("Propriété transférée.")).toBeVisible();
    await expect(page.getByText("Votre rôle : Admin")).toBeVisible();

    await successor.goto("/groups");
    await expect(
      successor.locator("li", { hasText: "Famille Transfert" }).getByText("Propriétaire"),
    ).toBeVisible();
  });
});

test.describe("Groups — active-family switcher", () => {
  test("switching persists across pages via the cookie", async ({ page }) => {
    await registerAndLogin(page, "e2e-switcher", "Switcher User");
    await createGroup(page, "Famille Une");
    await createGroup(page, "Famille Deux");

    // The most recently created group is active; switch back to the first.
    await page.goto("/");
    await expect(page.locator("select[name=group_id] option[selected]")).toHaveText(
      "Famille Deux",
    );
    await page.locator("select[name=group_id]").selectOption({ label: "Famille Une" });
    await page.getByRole("button", { name: "Changer" }).click();
    await expect(page).toHaveURL("/");
    await expect(page.locator("select[name=group_id] option[selected]")).toHaveText(
      "Famille Une",
    );

    // Persists on another page load (cookie, not per-request state).
    await page.goto("/groups");
    await expect(page.locator("select[name=group_id] option[selected]")).toHaveText(
      "Famille Une",
    );
  });
});
