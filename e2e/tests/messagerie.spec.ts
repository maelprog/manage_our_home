import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Front epic F8 — Messagerie (issue #23): every user journey the epic
// introduces, happy paths plus the documented error states from
// apps/api/src/messagerie/'s error tables (see docs/front-epic-8-messagerie.md).
// One thread per family, text only. Any member reads/posts; the per-row
// "Modifier"/"Supprimer" controls stay behind can_modify (author/admin/owner).
// The WebSocket is push-only and an enhancement: a message posted from one
// session appears live in a second session's thread without it reloading (the
// journey backend epic #7 explicitly deferred here). Send/edit/delete also work
// as plain form posts (no-JS baseline).

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-messagerie-password-1";

/** Register + verify + login a fresh user on the given page. */
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

/** Creates an invitation link for `groupName` (caller must be owner/admin). */
async function createInvitationLink(page: Page, groupName: string): Promise<string> {
  await page.goto("/groups");
  await page.locator("li", { hasText: groupName }).getByRole("link", { name: "Membres" }).click();
  await page.getByRole("button", { name: "Créer une invitation" }).click();
  const href = await page.locator(".notice.success a").getAttribute("href");
  if (!href) throw new Error("no invitation link");
  return href;
}

/** Sends a message via the composer on /messagerie. */
async function sendMessage(page: Page, content: string): Promise<void> {
  await page.goto("/messagerie");
  await page.getByLabel("Votre message").fill(content);
  await page.getByRole("button", { name: "Envoyer" }).click();
  await expect(page).toHaveURL(/\/messagerie\?notice=message_sent$/);
}

test.describe("Messagerie — thread & composer", () => {
  test("posts a message and it appears in the thread with author + timestamp", async ({ page }) => {
    await registerAndLogin(page, "e2e-msgsend", "Msg Sender");
    await createGroup(page, "Famille Discussion");

    await page.goto("/messagerie");
    await expect(page.getByText("Aucun message pour le moment.")).toBeVisible();

    await sendMessage(page, "Bonjour la famille");
    await expect(page.getByText("Message envoyé.")).toBeVisible();

    const row = page.locator("li[data-message-id]", { hasText: "Bonjour la famille" });
    await expect(row).toHaveCount(1);
    // Author name is rendered on the row.
    await expect(row.getByText("Msg Sender")).toBeVisible();
    // Paris-local timestamp (dd/mm/yyyy à hh:mm).
    await expect(row.getByText(/\d{2}\/\d{2}\/\d{4} à \d{2}:\d{2}/)).toBeVisible();
  });

  test("server-side empty-content rejection preserves the typed text", async ({ page }) => {
    await registerAndLogin(page, "e2e-msgempty", "Msg Empty");
    await createGroup(page, "Famille Vide Message");
    await page.goto("/messagerie");

    // Bypass the browser's `required` so the shared server-side validation is
    // what rejects it — a whitespace-only message.
    await page.getByLabel("Votre message").evaluate((el) => el.removeAttribute("required"));
    await page.getByLabel("Votre message").fill("   ");
    await page.getByRole("button", { name: "Envoyer" }).click();

    await expect(page.getByText("Le message ne peut pas être vide.")).toBeVisible();
    // The composer keeps what was typed (re-rendered inline, not PRG).
    await expect(page.getByLabel("Votre message")).toHaveValue("   ");
  });
});

test.describe("Messagerie — live updates (WebSocket)", () => {
  test("a message posted in one session appears live in a second session", async ({
    page,
    browser,
  }) => {
    // Owner creates the family and invites a second member.
    await registerAndLogin(page, "e2e-msgliveowner", "Live Owner");
    await createGroup(page, "Famille Temps Réel");
    const href = await createInvitationLink(page, "Famille Temps Réel");

    const context = await browser.newContext();
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-msglivemember", "Live Member");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    // The member sits on the live thread (WS open) without reloading.
    await member.goto("/messagerie");
    await expect(member.getByText("Aucun message pour le moment.")).toBeVisible();

    // The owner posts from their own session.
    await sendMessage(page, "Message en direct");

    // It shows up in the member's already-open thread with no manual reload —
    // the WS push triggers a server-rendered re-render of #thread.
    await expect(
      member.locator("#thread").getByText("Message en direct"),
    ).toBeVisible({ timeout: 15000 });

    await context.close();
  });
});

test.describe("Messagerie — edit & delete", () => {
  test("author edits a message inline and it shows the modifié marker", async ({ page }) => {
    await registerAndLogin(page, "e2e-msgedit", "Msg Editor");
    await createGroup(page, "Famille Édition Msg");
    await sendMessage(page, "Texte initial");

    await page.goto("/messagerie");
    const row = page.locator("li[data-message-id]", { hasText: "Texte initial" });
    // The edit form lives in a native <details> disclosure — open it, then edit.
    await row.locator("summary", { hasText: "Modifier" }).click();
    await row.getByLabel("Modifier le message").fill("Texte corrigé");
    await row.getByRole("button", { name: "Enregistrer" }).click();

    await expect(page).toHaveURL(/\/messagerie\?notice=message_updated$/);
    await expect(page.getByText("Message modifié.")).toBeVisible();
    const updated = page.locator("li[data-message-id]", { hasText: "Texte corrigé" });
    await expect(updated).toHaveCount(1);
    await expect(updated.getByText("(modifié)")).toBeVisible();
  });

  test("author deletes a message", async ({ page }) => {
    await registerAndLogin(page, "e2e-msgdel", "Msg Deleter");
    await createGroup(page, "Famille Suppression Msg");
    await sendMessage(page, "À supprimer bientôt");

    await page.goto("/messagerie");
    await page
      .locator("li[data-message-id]", { hasText: "À supprimer bientôt" })
      .getByRole("button", { name: "Supprimer" })
      .click();

    await expect(page).toHaveURL(/\/messagerie\?notice=message_deleted$/);
    await expect(page.getByText("Message supprimé.")).toBeVisible();
    await expect(
      page.locator("li[data-message-id]", { hasText: "À supprimer bientôt" }),
    ).toHaveCount(0);
  });

  test("editing an unknown/deleted message id shows the not-found page", async ({ page }) => {
    await registerAndLogin(page, "e2e-msg404", "Msg 404");
    await createGroup(page, "Famille Msg 404");

    // POST an edit to a message id that doesn't exist — the handler maps the
    // backend 404 to the "Message introuvable" page.
    const response = await page.request.post(
      "/messagerie/00000000-0000-4000-8000-000000000000/edit",
      { form: { content: "peu importe" } },
    );
    expect(await response.text()).toContain("Message introuvable");
  });
});

test.describe("Messagerie — permission bar", () => {
  test("a standard member cannot edit/delete another member's message but can post", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-msgpermowner", "Perm Owner");
    await createGroup(page, "Famille Droits Msg");
    const href = await createInvitationLink(page, "Famille Droits Msg");

    // Owner posts a message and captures its id (for a forged edit attempt).
    await sendMessage(page, "Message du propriétaire");
    await page.goto("/messagerie");
    const ownerMsgId = await page
      .locator("li[data-message-id]", { hasText: "Message du propriétaire" })
      .getAttribute("data-message-id");
    if (!ownerMsgId) throw new Error("no message id for the owner's message");

    const context = await browser.newContext();
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-msgpermmember", "Perm Member");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    // The member sees the owner's message but no controls on it.
    await member.goto("/messagerie");
    const ownerRow = member.locator("li[data-message-id]", { hasText: "Message du propriétaire" });
    await expect(ownerRow).toBeVisible();
    await expect(ownerRow.locator("summary", { hasText: "Modifier" })).toHaveCount(0);
    await expect(ownerRow.getByRole("button", { name: "Supprimer" })).toHaveCount(0);

    // A forged edit POST is still rejected by the backend → forbidden page.
    const forged = await member.request.post(`/messagerie/${ownerMsgId}/edit`, {
      form: { content: "piratage" },
    });
    expect(await forged.text()).toContain("Action non autorisée");

    // ...but the member can still post their own message, and can edit it.
    await sendMessage(member, "Message du membre");
    await member.goto("/messagerie");
    const memberRow = member.locator("li[data-message-id]", { hasText: "Message du membre" });
    await expect(memberRow.locator("summary", { hasText: "Modifier" })).toHaveCount(1);

    await context.close();
  });
});

test.describe("Messagerie — pagination", () => {
  test("load older messages opens a history window that links back to recent", async ({ page }) => {
    await registerAndLogin(page, "e2e-msgpage", "Msg Pager");
    await createGroup(page, "Famille Pagination");

    // Post three messages; view the thread with a page size of 2 so has_more
    // fires with just three messages instead of fifty-one.
    await sendMessage(page, "Premier message");
    await sendMessage(page, "Deuxième message");
    await sendMessage(page, "Troisième message");

    await page.goto("/messagerie?limit=2");
    // The live view (newest window) shows the two most recent, and offers to
    // load older. (Scope to the row <li>: the author's own rows carry a
    // pre-filled edit <textarea> with the same content, so a bare getByText
    // would match twice.)
    await expect(page.locator("li[data-message-id]", { hasText: "Deuxième message" })).toBeVisible();
    await expect(page.locator("li[data-message-id]", { hasText: "Troisième message" })).toBeVisible();
    await expect(page.locator("li[data-message-id]", { hasText: "Premier message" })).toHaveCount(0);

    await page.getByRole("link", { name: "Charger les messages plus anciens" }).click();
    // The older window shows the first message and no composer.
    await expect(page).toHaveURL(/before_created_at=/);
    await expect(page.locator("li[data-message-id]", { hasText: "Premier message" })).toBeVisible();
    await expect(page.getByLabel("Votre message")).toHaveCount(0);

    // ...and links back to the live view.
    await page.getByRole("link", { name: "Revenir aux messages récents" }).click();
    await expect(page).toHaveURL("/messagerie");
    await expect(page.getByLabel("Votre message")).toBeVisible();
  });
});
