import { Browser, expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Front epic #2 — Groups (issue #17): every user journey the epic
// introduces, happy paths plus the documented error states from
// apps/api/src/groups/mod.rs's error table (422 too_many_groups /
// name_required / new_owner_id_required, 409 last_member_must_delete_group,
// 404 consumed or unknown invitation, 403 permission bar).

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

/** The active family, as the sidebar's profile block names it (#523). */
function activeFamily(page: Page) {
  return page.locator('header a[href="/account"] .muted');
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

    // The new group is the active family, named in the sidebar's profile
    // block.
    await page.goto("/");
    await expect(activeFamily(page)).toHaveText("Famille Création");
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
    await expect(guestRow.getByText("Standard", { exact: true })).toBeVisible();

    // Owner sees the new member listed with the standard role, in its
    // pastille (#530).
    await page.goto("/groups");
    await page
      .locator("li", { hasText: "Famille Invitation" })
      .getByRole("link", { name: "Membres" })
      .click();
    await expect(
      page.locator("li", { hasText: "Invited Guest" }).locator(".badge", { hasText: "Standard" }),
    ).toBeVisible();

    // Single-use, even for another fresh user: accepting deleted the
    // invitation (#138), so re-use is a 404.
    const { page: late } = await secondUser(browser, "e2e-late", "Late Guest");
    await late.goto(invite);
    await late.getByRole("button", { name: "Rejoindre le groupe" }).click();
    await expect(late.getByText("Invitation invalide")).toBeVisible();
  });

  test("unknown invitation token shows the invalid page; garbage paste is rejected inline", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-badinvite", "Bad Invite User");

    // Well-formed token (43 base64url characters, #335), but no such
    // invitation → apps/api 404.
    await page.goto(`/groups/invitations/${"A".repeat(43)}/accept`);
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

    // #530: nor, on the family settings, « Gérer » or the invitation —
    // while the owner has both there.
    await member.goto("/groups");
    await member
      .locator("li", { hasText: "Famille Permissions" })
      .getByRole("link", { name: "Paramètres" })
      .click();
    await expect(member.getByRole("heading", { level: 1, name: "Famille Permissions" })).toBeVisible();
    await expect(member.locator("li", { hasText: "Perm Owner" }).locator(".badge")).toHaveText(
      "Propriétaire",
    );
    await expect(member.getByRole("link", { name: "Gérer", exact: true })).toHaveCount(0);
    await expect(member.getByRole("button", { name: "Inviter" })).toHaveCount(0);
    await expect(member.getByLabel("E-mail à inviter")).toHaveCount(0);

    await page.goto("/groups");
    await page
      .locator("li", { hasText: "Famille Permissions" })
      .getByRole("link", { name: "Paramètres" })
      .click();
    await expect(page.getByLabel("E-mail à inviter")).toBeVisible();
    const memberRow = page.locator("li", { hasText: "Perm Member" });
    await expect(memberRow.locator(".badge")).toHaveText("Standard");
    await memberRow.getByRole("link", { name: "Gérer", exact: true }).click();
    await expect(page).toHaveURL(/\/groups\/[0-9a-f-]+\/members#member-[0-9a-f-]+$/);
    await expect(page.locator("li.list-row:target")).toContainText("Perm Member");
    await expect(
      page.locator("li.list-row:target").getByRole("button", { name: "Changer le rôle" }),
    ).toBeVisible();
  });

  // #462: an invitation opened without a session survives the login.
  test("logged out: the invite link leads to the login, then back to the invitation", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-backinviter", "Back Inviter");
    await createGroup(page, "Famille Retour");
    const invite = await createInviteLink(page, "Famille Retour");
    // The link to share is absolute (#462).
    expect(invite).toMatch(/^https?:\/\/[^/]+\/groups\/invitations\/[A-Za-z0-9_-]{43}\/accept$/);

    // An account of its own, then a browser without its session.
    const { email } = await secondUser(browser, "e2e-backguest", "Back Guest");
    const context = await browser.newContext();
    const guest = await context.newPage();
    await guest.goto(invite);
    await expect(guest).toHaveURL(/\/login$/);
    await guest.getByLabel("Email").fill(email);
    await guest.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
    await guest.getByRole("button", { name: "Se connecter" }).click();

    await expect(guest).toHaveURL(invite);
    await expect(guest.getByRole("heading", { name: "Rejoindre un groupe" })).toBeVisible();
    await guest.getByRole("button", { name: "Rejoindre le groupe" }).click();
    await expect(guest).toHaveURL(/\/groups\?notice=joined$/);
    await expect(guest.locator("li", { hasText: "Famille Retour" })).toBeVisible();

    // Once: the next visit of `/` is the dashboard.
    await guest.goto("/");
    await expect(guest).toHaveURL("/");
    await context.close();
  });

  // #462: and the registration, its email verification and the login.
  test("no account: the invite link leads through the registration back to the invitation", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-newinviter", "New Inviter");
    await createGroup(page, "Famille Nouvelle");
    const invite = await createInviteLink(page, "Famille Nouvelle");

    const context = await browser.newContext();
    const guest = await context.newPage();
    await guest.goto(invite);
    await expect(guest).toHaveURL(/\/login$/);
    await guest.getByRole("link", { name: "Créer un compte" }).click();

    const email = uniqueEmail("e2e-newguest");
    await guest.getByLabel("Email").fill(email);
    await guest.getByLabel("Nom affiché").fill("New Guest");
    await guest.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
    await guest.getByRole("textbox", { name: "Confirmation" }).fill(PASSWORD);
    await guest.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
    await guest
      .getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." })
      .check();
    await guest.getByRole("button", { name: "Créer mon compte" }).click();
    await expect(guest).toHaveURL(/\/register\/check-email$/);

    const token = await fetchVerificationToken(email);
    await guest.goto(`/verify-email?token=${token}`);
    await guest.getByRole("link", { name: "Se connecter" }).click();
    await guest.getByLabel("Email").fill(email);
    await guest.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
    await guest.getByRole("button", { name: "Se connecter" }).click();

    await expect(guest).toHaveURL(invite);
    await guest.getByRole("button", { name: "Rejoindre le groupe" }).click();
    await expect(guest).toHaveURL(/\/groups\?notice=joined$/);
    await expect(guest.locator("li", { hasText: "Famille Nouvelle" })).toBeVisible();
    await context.close();
  });

  // #462: a return address naming another site is ignored.
  test("a next parameter naming another site ends the login on the home page", async ({
    page,
    browser,
  }) => {
    const email = await registerAndLogin(page, "e2e-nextevil", "Next Evil");
    for (const next of ["https://evil.example", "//evil.example", "/\\evil", "javascript:alert(1)"]) {
      const context = await browser.newContext();
      const visitor = await context.newPage();
      await visitor.goto(`/login?next=${encodeURIComponent(next)}`);
      await visitor.getByLabel("Email").fill(email);
      await visitor.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
      await visitor.getByRole("button", { name: "Se connecter" }).click();
      await expect(visitor).toHaveURL("/");
      await context.close();
    }
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
    // The role pastille (#530), not the <select> option that shares the text.
    await expect(
      page.locator("li", { hasText: "Role Member" }).locator(".badge", { hasText: "Admin" }),
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
    // #530: the family's name is the page's title, under « Paramètres de
    // la famille ».
    await expect(page.getByRole("heading", { level: 1, name: "Famille Après" })).toBeVisible();
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

    // The most recently created group is active; switch back to the first,
    // from the profile the sidebar's block leads to (#523).
    await page.goto("/");
    await expect(activeFamily(page)).toHaveText("Famille Deux");
    await page.getByRole("link", { name: /^Profil/ }).click();
    await expect(page).toHaveURL("/account");
    await page.getByRole("radio", { name: /Famille Une/ }).check();
    await page.getByRole("button", { name: "Utiliser cette famille" }).click();
    await expect(page).toHaveURL("/account");
    await expect(activeFamily(page)).toHaveText("Famille Une");

    // Persists on another page load (cookie, not per-request state).
    await page.goto("/groups");
    await expect(activeFamily(page)).toHaveText("Famille Une");
  });

  // #529: the profile lists the families as radio buttons, posting to
  // `POST /groups/switch` — the one switcher since #523.
  test("the profile picks the active family, which the header then shows", async ({ page }) => {
    await registerAndLogin(page, "e2e-profile-switch", "Profil User");
    await createGroup(page, "Famille Une");
    await createGroup(page, "Famille Deux");

    await page.goto("/account");
    await expect(page.getByRole("heading", { name: "Profil User", level: 1 })).toBeVisible();
    const families = page.getByRole("radiogroup", { name: "Mes familles" });
    const one = families.getByRole("radio", { name: /Famille Une/ });
    const two = families.getByRole("radio", { name: /Famille Deux/ });
    await expect(two).toBeChecked();
    await expect(one).not.toBeChecked();
    await expect(families.getByText("1 membre · Propriétaire")).toHaveCount(2);

    await one.check();
    await page.getByRole("button", { name: "Utiliser cette famille" }).click();
    await expect(page).toHaveURL("/account");
    await expect(activeFamily(page)).toHaveText("Famille Une");
    await expect(one).toBeChecked();

    // The header keeps it on the next page.
    await page.goto("/");
    await expect(activeFamily(page)).toHaveText("Famille Une");

    // The RGPD rights stay two clicks away from any page: the profile,
    // then the row.
    await page.getByRole("link", { name: /^Profil/ }).click();
    await expect(page).toHaveURL("/account");
    await page.getByRole("link", { name: "Supprimer mon compte" }).click();
    await expect(page).toHaveURL("/account/delete");
    await page.getByRole("link", { name: /^Profil/ }).click();
    await expect(page.getByRole("link", { name: "Exporter mes données" })).toHaveAttribute(
      "href",
      "/account/export",
    );
  });
});
