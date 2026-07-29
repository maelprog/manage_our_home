import { Browser, expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";
import { IcsFixture, icsDayThisMonth, icsFeed, startIcsFixtureServer } from "../lib/ics-server";

// Front epic F11 — Google Calendar import UI (issue #52): connect a private ICS
// feed to the family agenda, pull it on demand, disconnect it. Covers the
// journeys the epic introduces plus the documented error states from
// apps/api/src/google_calendar/'s error table (403 permission bar, 404 unknown
// connection, 422 feed_fetch_failed/invalid_ics) and the two properties that are
// the crux of the epic: UID-keyed idempotence (a re-import of an unchanged feed
// duplicates nothing) and the feed URL never leaking back out.
//
// No Google dependency and no network egress: the feed is served by a local
// fixture server (see ../lib/ics-server.ts for why that works).

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-gcal-password-1";

// Deliberately free of every form label these tests query ("Nom de l'agenda",
// "Adresse secrète au format iCal", "Nom du groupe", …): the family switcher's
// <label> text contains the active group's name, so a colliding name breaks
// strict-mode getByLabel.
const FAMILY = "Famille GCal";

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

/** An owner with a family, ready to connect a calendar. */
async function owner(browser: Browser, prefix: string): Promise<Page> {
  const context = await browser.newContext();
  const page = await context.newPage();
  await registerAndLogin(page, prefix, "GCal Owner");
  await createGroup(page, FAMILY);
  return page;
}

/** Fills and submits the connect form. Does not assert the outcome. */
async function submitConnectForm(page: Page, label: string, feedUrl: string): Promise<void> {
  await page.goto("/agenda/imports/new");
  await page.getByLabel("Nom de l'agenda").fill(label);
  await page.locator('input[name="feed_url"]').fill(feedUrl);
  await page.getByRole("button", { name: "Connecter cet agenda" }).click();
}

/** Connects a calendar and asserts it landed in the list. */
async function connectCalendar(page: Page, label: string, feedUrl: string): Promise<void> {
  await submitConnectForm(page, label, feedUrl);
  await expect(page).toHaveURL(/\/agenda\/imports\?notice=import_created$/);
  await expect(page.locator("tr", { hasText: label })).toHaveCount(1);
}

/** Presses "Importer maintenant" on a connection's row. */
async function runImport(page: Page, label: string): Promise<void> {
  await page.goto("/agenda/imports");
  await page.locator("tr", { hasText: label }).getByRole("button", { name: "Importer maintenant" }).click();
}

/** How many chips in the current month view carry this title. */
async function chipCount(page: Page, title: string): Promise<number> {
  await page.goto("/agenda");
  return page.getByRole("link", { name: new RegExp(title) }).count();
}

let feed: IcsFixture;

test.beforeAll(async () => {
  feed = await startIcsFixtureServer();
});

test.afterAll(async () => {
  await feed.close();
});

test.describe("Google Calendar import — connect and pull", () => {
  test("an admin connects a calendar, imports it, and its events land in the agenda", async ({
    browser,
  }) => {
    const page = await owner(browser, "e2e-gcal-connect");

    // The door is signposted from the agenda and from the family settings.
    await page.goto("/agenda");
    await expect(page.getByRole("link", { name: "Agendas Google" })).toBeVisible();
    await page.goto("/groups");
    await page.locator("li", { hasText: FAMILY }).getByRole("link", { name: "Paramètres" }).click();
    await page.getByRole("link", { name: "Gérer les agendas Google" }).click();
    await expect(page).toHaveURL(/\/agenda\/imports$/);

    // Empty state explains the feature before anything is connected.
    await expect(page.getByRole("heading", { name: "Agendas Google" })).toBeVisible();
    await expect(page.getByText("Aucun agenda Google connecté.")).toBeVisible();
    // The model is stated, not implied: on demand, one-way.
    await expect(page.locator("main")).toContainText("à sens unique");
    await expect(page.locator("main")).toContainText("à la demande");

    const path = "/connect.ics";
    const day = icsDayThisMonth(15);
    feed.serve(
      path,
      icsFeed([
        {
          uid: "gcal-connect-1@example.test",
          summary: "Rendez-vous dentiste",
          day,
          lastModified: "20260101T090000Z",
          location: "12 rue des Lilas",
        },
        {
          uid: "gcal-connect-2@example.test",
          summary: "Cours de piano",
          day,
          startTime: "140000",
          endTime: "150000",
          lastModified: "20260101T090000Z",
        },
      ]),
    );

    await connectCalendar(page, "Agenda partagé", feed.url(path));
    // A brand-new connection has pulled nothing yet, and says so.
    await expect(page.locator("tr", { hasText: "Agenda partagé" })).toContainText("jamais importé");

    await runImport(page, "Agenda partagé");
    await expect(page).toHaveURL(/notice=imported/);
    await expect(
      page.getByText("Import terminé : 2 événements importés, 0 mis à jour, 0 inchangé."),
    ).toBeVisible();
    // The last-import cell is no longer "jamais importé".
    await expect(page.locator("tr", { hasText: "Agenda partagé" })).not.toContainText(
      "jamais importé",
    );

    // The imported VEVENTs are ordinary agenda events now.
    expect(await chipCount(page, "Rendez-vous dentiste")).toBe(1);
    expect(await chipCount(page, "Cours de piano")).toBe(1);
  });

  test("re-importing an unchanged feed changes nothing and duplicates nothing", async ({
    browser,
  }) => {
    const page = await owner(browser, "e2e-gcal-idem");
    const path = "/idempotent.ics";
    feed.serve(
      path,
      icsFeed([
        {
          uid: "gcal-idem-1@example.test",
          summary: "Réunion de copropriété",
          day: icsDayThisMonth(12),
          lastModified: "20260101T090000Z",
        },
      ]),
    );

    await connectCalendar(page, "Agenda stable", feed.url(path));
    await runImport(page, "Agenda stable");
    await expect(
      page.getByText("Import terminé : 1 événement importé, 0 mis à jour, 0 inchangé."),
    ).toBeVisible();

    // Second run against an untouched feed: everything skipped.
    await runImport(page, "Agenda stable");
    await expect(
      page.getByText("Import terminé : 0 événement importé, 0 mis à jour, 1 inchangé."),
    ).toBeVisible();

    expect(await chipCount(page, "Réunion de copropriété")).toBe(1);
  });

  test("a changed event is updated in place, not added a second time", async ({ browser }) => {
    const page = await owner(browser, "e2e-gcal-update");
    const path = "/mutating.ics";
    const day = icsDayThisMonth(9);
    const event = {
      uid: "gcal-update-1@example.test",
      summary: "Conseil de classe",
      day,
      lastModified: "20260101T090000Z",
    };
    feed.serve(path, icsFeed([event]));

    await connectCalendar(page, "Agenda mouvant", feed.url(path));
    await runImport(page, "Agenda mouvant");
    expect(await chipCount(page, "Conseil de classe")).toBe(1);

    // Same UID, new title, bumped LAST-MODIFIED — what Google does when the
    // event is edited upstream.
    feed.serve(
      path,
      icsFeed([{ ...event, summary: "Conseil de classe (reporté)", lastModified: "20260202T090000Z" }]),
    );
    await runImport(page, "Agenda mouvant");
    await expect(
      page.getByText("Import terminé : 0 événement importé, 1 mis à jour, 0 inchangé."),
    ).toBeVisible();

    expect(await chipCount(page, "Conseil de classe \\(reporté\\)")).toBe(1);
    // The old title is gone: the row was rewritten, not duplicated.
    expect(await chipCount(page, "Conseil de classe$")).toBe(0);
  });
});

test.describe("Google Calendar import — permission bar", () => {
  test("a standard member cannot connect or remove, but can trigger an import", async ({
    browser,
  }) => {
    const admin = await owner(browser, "e2e-gcal-bar-owner");
    const path = "/shared.ics";
    feed.serve(
      path,
      icsFeed([
        {
          uid: "gcal-bar-1@example.test",
          summary: "Sortie scolaire",
          day: icsDayThisMonth(18),
          lastModified: "20260101T090000Z",
        },
      ]),
    );
    await connectCalendar(admin, "Agenda familial", feed.url(path));

    const invite = await createInviteLink(admin, FAMILY);
    const { page: member } = await secondUser(browser, "e2e-gcal-bar-member", "GCal Member");
    await member.goto(invite);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();
    await expect(member).toHaveURL(/\/groups\?notice=joined$/);

    await member.goto("/agenda/imports");
    // Reads the list — but neither of the admin/owner controls is rendered.
    await expect(member.locator("tr", { hasText: "Agenda familial" })).toHaveCount(1);
    await expect(member.getByRole("link", { name: "Ajouter un agenda Google" })).toHaveCount(0);
    await expect(member.getByRole("link", { name: "Supprimer" })).toHaveCount(0);

    // Direct navigation is bounced with the permission copy — the hidden
    // controls are UX, the gate is enforced.
    await member.goto("/agenda/imports/new");
    await expect(member).toHaveURL(/\/agenda\/imports\?error=forbidden$/);
    await expect(
      member.getByText(
        "Seul un administrateur ou le propriétaire de la famille peut connecter ou retirer un agenda Google.",
      ),
    ).toBeVisible();

    // Pulling the feed, though, is open to any member.
    await runImport(member, "Agenda familial");
    await expect(
      member.getByText("Import terminé : 1 événement importé, 0 mis à jour, 0 inchangé."),
    ).toBeVisible();
    expect(await chipCount(member, "Sortie scolaire")).toBe(1);
  });
});

test.describe("Google Calendar import — error states", () => {
  test("a non-http(s) address is rejected on the form", async ({ browser }) => {
    const page = await owner(browser, "e2e-gcal-scheme");
    await submitConnectForm(page, "Agenda douteux", "ftp://example.test/agenda.ics");

    await expect(page.getByText(/L'adresse doit commencer par https:\/\//)).toBeVisible();
    // Nothing was created.
    await page.goto("/agenda/imports");
    await expect(page.getByText("Aucun agenda Google connecté.")).toBeVisible();
  });

  test("the feed address is never echoed back after a failed submit", async ({ browser }) => {
    const page = await owner(browser, "e2e-gcal-secret");
    const secret = "ftp://calendar.example.test/ical/e2e-secret-token-42/basic.ics";
    await submitConnectForm(page, "Agenda secret", secret);

    // The error page re-asks for the address rather than putting a credential
    // back into the HTML — and it is not in the URL either.
    await expect(page.locator('input[name="feed_url"]')).toHaveValue("");
    expect(await page.content()).not.toContain("e2e-secret-token-42");
    expect(page.url()).not.toContain("e2e-secret-token-42");
    // The label, which is not a secret, is preserved.
    await expect(page.getByLabel("Nom de l'agenda")).toHaveValue("Agenda secret");
  });

  test("an unreachable feed reports a fetch failure without leaking the address", async ({
    browser,
  }) => {
    const page = await owner(browser, "e2e-gcal-unreachable");
    // Served by nothing: the fixture server 404s an unknown path.
    const missing = feed.url("/missing-e2e-secret-token-99.ics");
    await connectCalendar(page, "Agenda absent", missing);

    await runImport(page, "Agenda absent");
    await expect(page.getByText(/Google n'a pas répondu/)).toBeVisible();
    // apps/api's message interpolates reqwest's error, which can carry the URL;
    // the page maps the code to fixed copy instead.
    expect(await page.content()).not.toContain("missing-e2e-secret-token-99");
    expect(page.url()).not.toContain("missing-e2e-secret-token-99");
  });

  test("a body that is not an iCal feed reports invalid_ics", async ({ browser }) => {
    const page = await owner(browser, "e2e-gcal-notics");
    const path = "/not-a-calendar.txt";
    feed.serve(path, "this is definitely not an ics file", "text/plain");
    await connectCalendar(page, "Agenda cassé", feed.url(path));

    await runImport(page, "Agenda cassé");
    await expect(
      page.getByText("Le contenu récupéré n'est pas un agenda iCal valide."),
    ).toBeVisible();
  });

  test("importing a connection that no longer exists reports not found", async ({ browser }) => {
    const page = await owner(browser, "e2e-gcal-404");
    await page.goto("/agenda/imports");
    // Forge the action the row's form would post to.
    await page.evaluate((id) => {
      const form = document.createElement("form");
      form.method = "post";
      form.action = `/agenda/imports/${id}/import`;
      document.body.appendChild(form);
      form.submit();
    }, crypto.randomUUID());
    await expect(page.getByText("Cet agenda connecté n'existe plus.")).toBeVisible();
  });
});

test.describe("Google Calendar import — disconnect", () => {
  test("removing a connection keeps the events it already imported", async ({ browser }) => {
    const page = await owner(browser, "e2e-gcal-delete");
    const path = "/to-delete.ics";
    feed.serve(
      path,
      icsFeed([
        {
          uid: "gcal-delete-1@example.test",
          summary: "Match de handball",
          day: icsDayThisMonth(21),
          lastModified: "20260101T090000Z",
        },
      ]),
    );
    await connectCalendar(page, "Agenda à retirer", feed.url(path));
    await runImport(page, "Agenda à retirer");
    expect(await chipCount(page, "Match de handball")).toBe(1);

    await page.goto("/agenda/imports");
    await page.locator("tr", { hasText: "Agenda à retirer" }).getByRole("link", { name: "Supprimer" }).click();

    // The confirmation offers the choice, unticked, and still spells out the
    // duplicate-on-reconnect consequence.
    await expect(page.getByRole("heading", { name: /Retirer « Agenda à retirer »/ })).toBeVisible();
    const deleteEvents = page.locator('input[name="delete_events"]');
    await expect(deleteEvents).not.toBeChecked();
    await expect(page.getByText(/restent dans l'agenda/)).toBeVisible();
    await expect(page.getByText(/ré-importés en double/)).toBeVisible();

    // Left untouched: this is the branch that keeps them.
    await page.getByRole("button", { name: "Retirer cet agenda Google" }).click();
    await expect(page).toHaveURL(/\/agenda\/imports\?notice=import_deleted$/);
    await expect(page.getByText(/Les événements déjà importés restent dans l'agenda/)).toBeVisible();
    await expect(page.getByText("Aucun agenda Google connecté.")).toBeVisible();

    // …and the promise the copy just made holds.
    expect(await chipCount(page, "Match de handball")).toBe(1);
  });

  // #55: the other branch of the same confirmation — the bulk cleanup that
  // otherwise means deleting a whole feed's worth of events one at a time.
  test("ticking the box removes the imported events along with the connection", async ({
    browser,
  }) => {
    const page = await owner(browser, "e2e-gcal-delete-events");
    const path = "/to-delete-with-events.ics";
    feed.serve(
      path,
      icsFeed([
        {
          uid: "gcal-delete-events-1@example.test",
          summary: "Cours de piano",
          day: icsDayThisMonth(14),
          lastModified: "20260101T090000Z",
        },
        {
          uid: "gcal-delete-events-2@example.test",
          summary: "Conseil de classe",
          day: icsDayThisMonth(15),
          lastModified: "20260101T090000Z",
        },
      ]),
    );
    await connectCalendar(page, "Agenda à vider", feed.url(path));
    await runImport(page, "Agenda à vider");
    expect(await chipCount(page, "Cours de piano")).toBe(1);
    expect(await chipCount(page, "Conseil de classe")).toBe(1);

    await page.goto("/agenda/imports");
    await page.locator("tr", { hasText: "Agenda à vider" }).getByRole("link", { name: "Supprimer" }).click();
    await page.locator('input[name="delete_events"]').check();
    await page.getByRole("button", { name: "Retirer cet agenda Google" }).click();

    // The banner names the count — the events are gone, so nothing on screen
    // could tell the user how many left.
    await expect(page).toHaveURL(/\/agenda\/imports\?notice=import_deleted&deleted=2$/);
    await expect(page.getByText(/ainsi que 2 événements importés/)).toBeVisible();
    await expect(page.getByText("Aucun agenda Google connecté.")).toBeVisible();

    expect(await chipCount(page, "Cours de piano")).toBe(0);
    expect(await chipCount(page, "Conseil de classe")).toBe(0);
  });
});
