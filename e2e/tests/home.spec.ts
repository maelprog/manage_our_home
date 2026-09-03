import { Browser, expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Issue #73 — the home dashboard. `/` is the first page after login and,
// until this epic, it said "Bienvenue / Vous êtes connecté." and led
// nowhere. It now carries one card per domain, each rendered from the same
// API the domain's own page reads.
//
// This file exists because the independent verification of PR #98 found two
// reproducible blockers living exactly here, in a surface that had no
// end-to-end coverage at all:
//
//   * an all-day event today (a birthday, a holiday) starts at Paris
//     midnight, so a filter on the start instant hid the whole class from
//     the moment the day began — as did merely being *in progress*;
//   * following the messagerie's own "Charger les messages plus anciens"
//     link marked every unread message read, irreversibly.
//
// Both are asserted below against a populated family, not an empty one.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-home-password-1";

/**
 * Family names here must avoid every form label of the pages under test:
 * the family switcher is a `<select>` whose option text is the group name,
 * and `getByLabel` matches case-insensitive substrings, so a group called
 * "Famille Nom" collides with the "Nom" field. "Foyer Accueil" contains
 * none of Nom / Fin / Titre / Début / Lieu / Article / Montant / Unité /
 * Quantité / Email / Rappel / Description.
 */
const FAMILY = "Foyer Accueil";

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

async function createInvitationLink(page: Page, groupName: string): Promise<string> {
  await page.goto("/groups");
  await page.locator("li", { hasText: groupName }).getByRole("link", { name: "Membres" }).click();
  await page.getByRole("button", { name: "Créer une invitation" }).click();
  const href = await page.locator(".notice.success a").getAttribute("href");
  if (!href) throw new Error("no invitation link");
  return href;
}

/** Registers a second member in `browser` and joins them to the family. */
async function joinAsMember(
  browser: Browser,
  href: string,
  prefix: string,
  displayName: string,
): Promise<Page> {
  const context = await browser.newContext();
  const member = await context.newPage();
  await registerAndLogin(member, prefix, displayName);
  await member.goto(href);
  await member.getByRole("button", { name: "Rejoindre le groupe" }).click();
  return member;
}

/** `YYYY-MM-DD` for today (+`offset` days) in the browser's own timezone. */
function isoDay(offset = 0): string {
  const d = new Date();
  d.setDate(d.getDate() + offset);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(
    d.getDate(),
  ).padStart(2, "0")}`;
}

interface EventOpts {
  title: string;
  start: string;
  end: string;
  allDay?: boolean;
}

async function createEvent(page: Page, opts: EventOpts): Promise<void> {
  await page.goto("/agenda/new");
  await page.getByLabel("Titre").fill(opts.title);
  if (opts.allDay) await page.locator('input[name="all_day"]').check();
  await page.getByLabel("Début").fill(opts.start);
  await page.getByLabel("Fin").fill(opts.end);
  await page.getByRole("button", { name: "Créer l'événement" }).click();
  await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);
}

async function createLowStockItem(page: Page, name: string): Promise<void> {
  await page.goto("/stocks/new");
  await page.getByLabel("Nom").fill(name);
  await page.getByLabel("Quantité").fill("0");
  await page.getByLabel("Unité").fill("kg");
  await page.getByLabel("Seuil de réappro").fill("2");
  await page.getByRole("button", { name: "Ajouter l'article" }).click();
}

async function addGroceryItem(page: Page, name: string): Promise<void> {
  await page.goto("/grocery-list");
  await page.getByLabel("Article").fill(name);
  await page.getByRole("button", { name: "Ajouter" }).click();
  await expect(page).toHaveURL(/\/grocery-list\?notice=item_added$/);
}

async function addBudgetEntry(page: Page, name: string, amount: string): Promise<void> {
  await page.goto("/budget/new");
  await page.getByLabel("Nom").fill(name);
  await page.getByLabel("Montant").fill(amount);
  await page.getByRole("button", { name: "Ajouter la dépense" }).click();
}

async function sendMessage(page: Page, content: string): Promise<void> {
  await page.goto("/messagerie");
  await page.getByLabel("Votre message").fill(content);
  await page.getByRole("button", { name: "Envoyer" }).click();
  await expect(page).toHaveURL(/\/messagerie\?notice=message_sent$/);
}

/** The `<section class="card">` whose `<h2>` is `heading`. */
function card(page: Page, heading: string) {
  return page.locator("section.card").filter({ has: page.getByRole("heading", { name: heading }) });
}

test.describe("Accueil — le tableau de bord d'une famille peuplée", () => {
  test("chaque carte rend les données réelles de la famille", async ({ page }) => {
    await registerAndLogin(page, "e2e-homefull", "Dash Owner");
    await createGroup(page, FAMILY);

    const tomorrow = isoDay(1);
    await createEvent(page, {
      title: "Rendez-vous pédiatre",
      start: `${tomorrow}T10:00`,
      end: `${tomorrow}T11:00`,
    });
    await createLowStockItem(page, "Farine");
    await addGroceryItem(page, "Pain de mie");
    await addBudgetEntry(page, "Courses de la semaine", "42.50");

    await page.goto("/");
    await expect(page.getByRole("heading", { name: "Accueil" })).toBeVisible();

    // Agenda: the event, with the creator's name as its (default) assignee.
    const agenda = card(page, "Prochains événements");
    await expect(agenda.getByText("Rendez-vous pédiatre")).toBeVisible();
    await expect(agenda.getByText("Dash Owner")).toBeVisible();

    // Stock, grocery, budget: each card's own real number, not a placeholder.
    await expect(card(page, "Stock bas").getByText("Farine")).toBeVisible();
    await expect(card(page, "Liste de courses").getByText("1 article à acheter.")).toBeVisible();
    await expect(card(page, "Budget").getByText("42,50 €")).toBeVisible();

    // Nothing anyone else wrote: the unread card is empty, and says so with
    // "tout est lu" rather than "aucun message".
    await expect(card(page, "Messages non lus").getByText("Tout est lu.")).toBeVisible();

    // Every card leads somewhere — the whole point of replacing the old
    // "Vous êtes connecté." placeholder.
    for (const href of ["/agenda", "/stocks?low_stock=1", "/grocery-list", "/budget", "/messagerie"]) {
      await expect(page.locator(`a[href="${href}"]`).first()).toBeVisible();
    }
  });

  test("un événement toute la journée aujourd'hui est bien affiché", async ({ page }) => {
    await registerAndLogin(page, "e2e-homeallday", "AllDay Owner");
    await createGroup(page, FAMILY);

    const today = isoDay();
    await createEvent(page, {
      title: "Anniversaire de Léa",
      start: `${today}T00:00`,
      end: `${today}T23:59`,
      allDay: true,
    });

    // Its start instant is Paris midnight, i.e. in the past at every hour
    // of the day — the blocker was a filter on that instant, which made
    // birthdays, holidays and school breaks invisible outright.
    await page.goto("/");
    const agenda = card(page, "Prochains événements");
    await expect(agenda.getByText("Anniversaire de Léa")).toBeVisible();
    // The all-day row says "journée" instead of a clock time; that branch
    // was unreachable for the current day.
    await expect(agenda.getByText("journée")).toBeVisible();
  });

  test("un événement déjà commencé mais pas terminé reste affiché", async ({ page }) => {
    await registerAndLogin(page, "e2e-homerunning", "Running Owner");
    await createGroup(page, FAMILY);

    // Starts at midnight today, ends tomorrow evening: whatever hour the
    // suite runs at, this one has started and is not over.
    await createEvent(page, {
      title: "Séjour à la montagne",
      start: `${isoDay()}T00:00`,
      end: `${isoDay(1)}T20:00`,
    });

    await page.goto("/");
    await expect(
      card(page, "Prochains événements").getByText("Séjour à la montagne"),
    ).toBeVisible();
  });
});

test.describe("Accueil — les messages non lus", () => {
  test("un message d'un autre membre apparaît puis disparaît une fois lu", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-homeunread", "Unread Owner");
    await createGroup(page, FAMILY);
    const href = await createInvitationLink(page, FAMILY);
    const member = await joinAsMember(browser, href, "e2e-homeunreadmem", "Unread Member");

    await sendMessage(member, "Il reste du pain ?");

    // The owner has never opened the thread: the message is unread, and the
    // card names who wrote it.
    await page.goto("/");
    const unread = card(page, "Messages non lus");
    await expect(unread.getByText("Il reste du pain ?")).toBeVisible();
    await expect(unread.getByText("Unread Member")).toBeVisible();

    // Opening the live thread is what marks it read. (Scoped to the row:
    // an owner may edit anyone's message, so the text also lives inside a
    // hidden "Modifier le message" textarea.)
    await page.goto("/messagerie");
    await expect(
      page.locator("li[data-message-id]", { hasText: "Il reste du pain ?" }),
    ).toHaveCount(1);

    await page.goto("/");
    await expect(card(page, "Messages non lus").getByText("Tout est lu.")).toBeVisible();

    await member.context().close();
  });

  test("la carte dit combien de messages non lus elle ne montre pas", async ({ page, browser }) => {
    await registerAndLogin(page, "e2e-homemore", "More Owner");
    await createGroup(page, FAMILY);
    const href = await createInvitationLink(page, FAMILY);
    const member = await joinAsMember(browser, href, "e2e-homemoremem", "More Member");

    for (let i = 1; i <= 7; i += 1) {
      await sendMessage(member, `Message numéro ${i}`);
    }

    // The card holds five; it must not swallow the other two.
    await page.goto("/");
    const unread = card(page, "Messages non lus");
    await expect(unread.locator("li")).toHaveCount(5);
    await expect(unread.getByText("+2 autre(s).")).toBeVisible();

    await member.context().close();
  });

  test("charger l'historique du fil ne marque pas les messages comme lus", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-homehistory", "History Owner");
    await createGroup(page, FAMILY);
    const href = await createInvitationLink(page, FAMILY);
    const member = await joinAsMember(browser, href, "e2e-homehistorymem", "History Member");

    await sendMessage(member, "Premier message");
    await sendMessage(member, "Deuxième message");

    // The owner reads the live thread one message at a time, which leaves a
    // "Charger les messages plus anciens" link — the app's own affordance,
    // and the one the verification used to reproduce the blocker.
    await page.goto("/messagerie?limit=1");
    const olderHref = await page
      .getByRole("link", { name: "Charger les messages plus anciens" })
      .getAttribute("href");
    expect(olderHref).toBeTruthy();

    // Leave the thread so the live socket is closed, then three messages
    // the owner never sees arrive.
    await page.goto("/");
    await expect(card(page, "Messages non lus").getByText("Tout est lu.")).toBeVisible();
    for (const text of ["Jamais vu A", "Jamais vu B", "Jamais vu C"]) {
      await sendMessage(member, text);
    }

    // The owner follows the (stale) history link. A history window renders
    // older messages; it must not claim the newer ones were read.
    await page.goto(olderHref as string);
    await expect(
      page.locator("li[data-message-id]", { hasText: "Premier message" }),
    ).toHaveCount(1);

    await page.goto("/");
    const unread = card(page, "Messages non lus");
    for (const text of ["Jamais vu A", "Jamais vu B", "Jamais vu C"]) {
      await expect(unread.getByText(text)).toBeVisible();
    }
    await expect(unread.locator("li")).toHaveCount(3);

    await member.context().close();
  });
});

test.describe("Accueil — l'assignation d'un événement", () => {
  test("les assignés choisis sont rendus sur l'accueil et sur la fiche", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-homeassign", "Assign Owner");
    await createGroup(page, FAMILY);
    const href = await createInvitationLink(page, FAMILY);
    const member = await joinAsMember(browser, href, "e2e-homeassignmem", "Assign Member");

    const tomorrow = isoDay(1);
    await page.goto("/agenda/new");
    await page.getByLabel("Titre").fill("Sortie scolaire");
    await page.getByLabel("Début").fill(`${tomorrow}T09:00`);
    await page.getByLabel("Fin").fill(`${tomorrow}T17:00`);
    // Both members checked: "Assigné à" is a checkbox per member.
    await page.locator('input[name="assignee_ids"]').first().check();
    await page.locator('input[name="assignee_ids"]').nth(1).check();
    await page.getByRole("button", { name: "Créer l'événement" }).click();
    await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);

    // The dashboard names both.
    await page.goto("/");
    const agenda = card(page, "Prochains événements");
    await expect(agenda.getByText("Sortie scolaire")).toBeVisible();
    await expect(agenda.getByText(/Assign Owner/)).toBeVisible();
    await expect(agenda.getByText(/Assign Member/)).toBeVisible();

    // And so does the event's own page — assignment used to be visible
    // nowhere but the dashboard.
    await page.goto("/agenda");
    await page.getByRole("link", { name: /Sortie scolaire/ }).first().click();
    await expect(page.getByRole("heading", { name: "Sortie scolaire" })).toBeVisible();
    const assignees = page.locator("p", { hasText: "Assigné à" });
    await expect(assignees).toHaveCount(1);
    await expect(assignees).toContainText("Assign Owner");
    await expect(assignees).toContainText("Assign Member");

    await member.context().close();
  });
});
