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

/** What `watchLiveSocket` counts on the current document of a page. */
type LiveSocketCounts = { opened: number; closed: number; frames: number };

/**
 * Counts, on every document `page` loads from now on, the WebSocket opens,
 * closes and received frames. The live script refreshes `#thread` on a socket
 * *close* too (its access probe), so a refused handshake still lands a new
 * message within its reconnect backoff: an update showing up proves nothing
 * about the socket (#373). These counts do.
 */
async function watchLiveSocket(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const Native = window.WebSocket;
    const counts = { opened: 0, closed: 0, frames: 0 };
    (window as unknown as { __liveSocket: typeof counts }).__liveSocket = counts;
    class Watched extends Native {
      constructor(url: string | URL, protocols?: string | string[]) {
        super(url, protocols);
        this.addEventListener("open", () => counts.opened++);
        this.addEventListener("close", () => counts.closed++);
        this.addEventListener("message", () => counts.frames++);
      }
    }
    window.WebSocket = Watched;
  });
}

function liveSocketCounts(page: Page): Promise<LiveSocketCounts> {
  return page.evaluate(
    () => (window as unknown as { __liveSocket: LiveSocketCounts }).__liveSocket,
  );
}

/** The thread's socket is open (handshake accepted) and has not dropped. */
async function expectLiveSocketOpen(page: Page): Promise<void> {
  await expect
    .poll(() => liveSocketCounts(page), { timeout: 10000 })
    .toMatchObject({ opened: 1, closed: 0 });
}

/** The update came as a push frame, on a socket that never dropped. */
async function expectPushedLive(page: Page): Promise<void> {
  const counts = await liveSocketCounts(page);
  expect(counts.opened).toBe(1);
  expect(counts.closed).toBe(0);
  expect(counts.frames).toBeGreaterThan(0);
}

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
    await watchLiveSocket(member);
    await member.goto("/messagerie");
    await expect(member.getByText("Aucun message pour le moment.")).toBeVisible();
    await expectLiveSocketOpen(member);

    // The owner posts from their own session.
    await sendMessage(page, "Message en direct");

    // It shows up in the member's already-open thread with no manual reload —
    // the WS push triggers a server-rendered re-render of #thread.
    await expect(
      member.locator("#thread").getByText("Message en direct"),
    ).toBeVisible({ timeout: 15000 });
    await expectPushedLive(member);

    await context.close();
  });

  test("a live refresh does not destroy an inline edit in progress", async ({ page, browser }) => {
    // Issue #48: the refresh used to swap the whole #thread subtree, so another
    // member posting while you were correcting a message collapsed your edit
    // form and threw away the text you had typed into it.
    await registerAndLogin(page, "e2e-msgeditliveowner", "Edit Live Owner");
    await createGroup(page, "Famille Correction Directe");
    const href = await createInvitationLink(page, "Famille Correction Directe");

    const context = await browser.newContext();
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-msgeditlivemember", "Edit Live Member");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    // The owner starts correcting their own message: disclosure open, new text
    // typed, nothing submitted yet.
    await sendMessage(page, "Texte à corriger");
    await watchLiveSocket(page);
    await page.goto("/messagerie");
    await expectLiveSocketOpen(page);
    const row = page.locator("li[data-message-id]", { hasText: "Texte à corriger" });
    await row.locator("summary", { hasText: "Modifier" }).click();
    await row.getByLabel("Modifier le message").fill("Correction en cours");

    // Meanwhile the member posts, which pushes a WS frame to the owner's page.
    await sendMessage(member, "Message pendant l'édition");

    // The new message lands — the refresh really did happen... (scope to the
    // row: the owner may edit any message, so the content also sits in that
    // row's pre-filled edit textarea and a bare getByText matches twice.)
    await expect(
      page.locator("#thread li[data-message-id]", { hasText: "Message pendant l'édition" }),
    ).toHaveCount(1, { timeout: 15000 });
    await expectPushedLive(page);
    // ...and the open editor kept both its disclosure and its unsaved text.
    await expect(row.getByLabel("Modifier le message")).toBeVisible();
    await expect(row.getByLabel("Modifier le message")).toHaveValue("Correction en cours");

    // The edit still goes through from that untouched form.
    await row.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page).toHaveURL(/\/messagerie\?notice=message_updated$/);
    const updated = page.locator("li[data-message-id]", { hasText: "Correction en cours" });
    await expect(updated).toHaveCount(1);
    await expect(updated.getByText("(modifié)")).toBeVisible();

    await context.close();
  });

  test("closing an untouched inline edit costs no extra refresh", async ({ page, browser }) => {
    // Issue #50: the refresh held back during an edit was replayed on close no
    // matter what — even when the frame that triggered it (another member
    // posting) left the edited row byte-identical. The replay is a full
    // fetch(location.href) + page parse, so it must only happen when the row's
    // own render really changed.
    await registerAndLogin(page, "e2e-msgeditnoresync", "Edit NoResync Owner");
    await createGroup(page, "Famille Correction Silencieuse");
    const href = await createInvitationLink(page, "Famille Correction Silencieuse");

    const context = await browser.newContext();
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-msgeditnoresyncmember", "Edit NoResync Member");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    await sendMessage(page, "Texte à corriger sans resync");
    await watchLiveSocket(page);
    await page.goto("/messagerie");
    await expectLiveSocketOpen(page);
    const row = page.locator("li[data-message-id]", { hasText: "Texte à corriger sans resync" });
    await row.locator("summary", { hasText: "Modifier" }).click();
    await row.getByLabel("Modifier le message").fill("Correction silencieuse");

    // A frame that changes another row entirely: the reconcile lands it, and
    // owes the edited row nothing.
    await sendMessage(member, "Message sans rapport");
    await expect(
      page.locator("#thread li[data-message-id]", { hasText: "Message sans rapport" }),
    ).toHaveCount(1, { timeout: 15000 });
    await expectPushedLive(page);

    // Only the live script sends this header (see refresh() in thread.rs).
    let replays = 0;
    page.on("request", (r) => {
      if (r.headers()["x-requested-with"] === "fetch") replays++;
    });
    await row.locator("summary", { hasText: "Modifier" }).click(); // close the editor
    await expect(row.getByLabel("Modifier le message")).toBeHidden();
    await page.waitForTimeout(1000); // scheduleRerender coalesces for 120ms
    expect(replays).toBe(0);

    // The unsaved text survived the close/reopen and still saves.
    await row.locator("summary", { hasText: "Modifier" }).click();
    await expect(row.getByLabel("Modifier le message")).toHaveValue("Correction silencieuse");
    await row.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page).toHaveURL(/\/messagerie\?notice=message_updated$/);
    await expect(
      page.locator("li[data-message-id]", { hasText: "Correction silencieuse" }),
    ).toHaveCount(1);

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

test.describe("Messagerie — unread badge in the navigation (#463)", () => {
  test("the Messagerie tab counts another member's unread messages until the thread is opened", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-msgbadgeowner", "Badge Owner");
    await createGroup(page, "Famille Pastille");
    const href = await createInvitationLink(page, "Famille Pastille");

    // Nothing posted yet: no badge, the tab is plain "Messagerie".
    await page.goto("/");
    const tab = page.locator(".tabs").getByRole("link", { name: "Messagerie" });
    await expect(tab).toHaveAccessibleName("Messagerie");
    await expect(tab.locator(".badge")).toHaveCount(0);

    const context = await browser.newContext();
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-msgbadgemember", "Badge Member");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();
    // Posted without the redirect to /messagerie being followed: the member
    // never opens the thread, so their read marker stays behind their own
    // three messages — only the exclusion of the author keeps them off the
    // count.
    for (const content of ["Premier non lu", "Deuxième non lu", "Troisième non lu"]) {
      const response = await member.request.post("/messagerie", {
        form: { content },
        maxRedirects: 0,
      });
      expect(response.status()).toBe(303);
    }

    // The author is not told about their own messages.
    await member.goto("/");
    await expect(member.locator(".tabs .badge")).toHaveCount(0);
    await context.close();

    // The owner is: three, drawn for the eye and said in the link's name —
    // which still holds "Messagerie", the word the suite finds the tab by.
    await page.goto("/");
    await expect(tab.locator(".badge")).toHaveText("3");
    await expect(tab).toHaveAccessibleName("Messagerie (3 non lus)");

    // Same tab in the phone's bottom bar.
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(tab.locator(".badge")).toBeVisible();
    await page.setViewportSize({ width: 1280, height: 720 });

    // Opening the thread reads them: no badge there, none afterwards.
    await tab.click();
    await expect(page).toHaveURL("/messagerie");
    await expect(tab.locator(".badge")).toHaveCount(0);
    await page.goto("/agenda");
    await expect(tab.locator(".badge")).toHaveCount(0);
    await expect(tab).toHaveAccessibleName("Messagerie");
  });
});

test.describe("Messagerie — replies (#465)", () => {
  test("a reply quotes its parent for its author and, live, for another member", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-msgreplyowner", "Reply Owner");
    await createGroup(page, "Famille Réponses");
    const href = await createInvitationLink(page, "Famille Réponses");

    const context = await browser.newContext();
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-msgreplymember", "Reply Member");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    await sendMessage(page, "On part à quelle heure ?");
    await page.goto("/messagerie");
    const parentId = await page
      .locator("li[data-message-id]", { hasText: "On part à quelle heure ?" })
      .getAttribute("data-message-id");
    if (!parentId) throw new Error("no message id for the parent");
    const parentRow = page.locator(`li[data-message-id="${parentId}"]`);

    // The member sits on the live thread; "Répondre" is offered to them too,
    // on a message they may not edit.
    await watchLiveSocket(member);
    await member.goto("/messagerie");
    await expectLiveSocketOpen(member);
    await expect(
      member.locator(`li[data-message-id="${parentId}"]`).getByRole("link", { name: "Répondre" }),
    ).toHaveCount(1);

    // The reply link is a plain link to the composer: no script involved.
    await parentRow.getByRole("link", { name: "Répondre" }).click();
    await expect(page).toHaveURL(new RegExp(`/messagerie\\?reply_to=${parentId}#composer$`));
    const banner = page.locator("#composer .notice");
    await expect(banner).toContainText("En réponse à Reply Owner");
    await expect(banner).toContainText("On part à quelle heure ?");
    await page.getByLabel("Votre message").fill("Vers 8h");
    await page.getByRole("button", { name: "Envoyer" }).click();
    await expect(page).toHaveURL(/\/messagerie\?notice=message_sent$/);

    // The author sees the quote above the reply, linking to the parent's row.
    const reply = page.locator("li[data-message-id]", { hasText: "Vers 8h" });
    const quote = reply.locator(".quote");
    await expect(quote).toContainText("En réponse à Reply Owner");
    await expect(quote).toContainText("On part à quelle heure ?");
    await expect(quote).toHaveAttribute("href", `#message-${parentId}`);
    await expect(page.locator(`#message-${parentId}`)).toHaveCount(1);

    // The other member gets it live, quote included.
    const memberReply = member.locator("#thread li[data-message-id]", { hasText: "Vers 8h" });
    await expect(memberReply.locator(".quote")).toContainText("On part à quelle heure ?", {
      timeout: 15000,
    });
    await expectPushedLive(member);

    // Deleting the parent keeps the reply, without its quote — live too: the
    // deletion frame re-renders the member's whole thread, replies included.
    await parentRow.getByRole("button", { name: "Supprimer" }).click();
    await expect(page).toHaveURL(/\/messagerie\?notice=message_deleted$/);
    await expect(
      page.locator("li[data-message-id]", { hasText: "Vers 8h" }).locator(".quote"),
    ).toHaveCount(0);
    await expect(member.locator(`#thread li[data-message-id="${parentId}"]`)).toHaveCount(0, {
      timeout: 15000,
    });
    await expect(memberReply).toHaveCount(1);
    await expect(memberReply.locator(".quote")).toHaveCount(0);
    await expectPushedLive(member);

    await context.close();
  });

  test("a reply whose parent vanished is refused with the text kept", async ({ page }) => {
    await registerAndLogin(page, "e2e-msgreplygone", "Reply Gone");
    await createGroup(page, "Famille Parent Disparu");

    // A parent that does not exist stands for one deleted after the
    // composer was rendered: the API refuses both alike.
    await page.goto("/messagerie?reply_to=00000000-0000-4000-8000-000000000000");
    await expect(page.locator("#composer .notice")).toContainText(
      "En réponse à un message précédent",
    );
    await page.getByLabel("Votre message").fill("Réponse orpheline");
    await page.getByRole("button", { name: "Envoyer" }).click();

    await expect(page.getByText("Le message auquel vous répondiez est introuvable.")).toBeVisible();
    await expect(page.getByLabel("Votre message")).toHaveValue("Réponse orpheline");
    // The reply is dropped: sending again posts a plain message.
    await expect(page.locator('#composer input[name="reply_to_id"]')).toHaveCount(0);
    await page.getByRole("button", { name: "Envoyer" }).click();
    await expect(page).toHaveURL(/\/messagerie\?notice=message_sent$/);
    const row = page.locator("li[data-message-id]", { hasText: "Réponse orpheline" });
    await expect(row).toHaveCount(1);
    await expect(row.locator(".quote")).toHaveCount(0);
  });
});

test.describe("Messagerie — malformed URL parameters (#580)", () => {
  test("a parameter that does not convert reads as absent: the live thread renders", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-msgbadquery", "Bad Query");
    await createGroup(page, "Famille Paramètres");
    await sendMessage(page, "Toujours là");

    for (const query of [
      "reply_to=",
      "reply_to=pas-un-uuid",
      "before_id=x&before_created_at=y",
      "limit=abc",
    ]) {
      const response = await page.goto(`/messagerie?${query}`);
      expect(response?.status(), query).toBe(200);
      // The live view (no history cursor), the message, a plain composer.
      await expect(page.locator('#thread[data-live="true"]'), query).toHaveCount(1);
      await expect(page.locator("li[data-message-id]", { hasText: "Toujours là" })).toHaveCount(1);
      await expect(page.locator("#composer .notice"), query).toHaveCount(0);
      await expect(page.locator('#composer input[name="reply_to_id"]'), query).toHaveCount(0);
      await expect(page.getByLabel("Votre message")).toBeVisible();
    }
  });

  test("a repeated parameter keeps its first occurrence (#582)", async ({ page }) => {
    await registerAndLogin(page, "e2e-msgdupquery", "Dup Query");
    await createGroup(page, "Famille Doublons");
    await sendMessage(page, "Premier message");
    const id = await page
      .locator("li[data-message-id]", { hasText: "Premier message" })
      .getAttribute("data-message-id");
    if (!id) throw new Error("no message id");
    const other = "00000000-0000-4000-8000-000000000000";

    // The first reply_to is a real message: the composer answers it.
    let response = await page.goto(`/messagerie?reply_to=${id}&reply_to=${other}`);
    expect(response?.status()).toBe(200);
    await expect(page.locator("#composer .notice")).toContainText("Premier message");
    await expect(page.locator('#composer input[name="reply_to_id"]')).toHaveValue(id);

    // The first one does not convert: absent, even though the second would.
    response = await page.goto(`/messagerie?reply_to=x&reply_to=${id}`);
    expect(response?.status()).toBe(200);
    await expect(page.locator('#thread[data-live="true"]')).toHaveCount(1);
    await expect(page.locator('#composer input[name="reply_to_id"]')).toHaveCount(0);
  });
});
