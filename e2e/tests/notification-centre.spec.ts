import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// The notification centre (#535), over the API of #534: a reminder that
// comes due is listed unread, opening it marks it read, and the count of
// the sidebar's entry and of the home's bell goes down with it.
//
// The reminders are the real ones: an event that starts in a few minutes,
// its reminder set ten minutes before, is due at once, and apps/api's
// worker writes the notification on its next pass — every 60 s
// (`SEND_POLL_INTERVAL_SECS`). Hence the long poll below, and the two
// events created up front, which come due on the same pass.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-centre-password-1";

/**
 * Avoids every form label of the pages under test (`getByLabel` matches
 * substrings, the family switcher included): Titre, Début, Fin, Lieu,
 * Description, Rappel, Fréquence.
 */
const FAMILY = "Foyer Pastille";

async function registerAndLogin(page: Page): Promise<void> {
  const email = uniqueEmail("centre");
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill("Dominique");
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
}

/** `YYYY-MM-DDTHH:MM` in Paris, `minutes` from now: a `datetime-local`. */
function parisLocal(minutes: number): string {
  const parts = new Intl.DateTimeFormat("en-CA", {
    timeZone: "Europe/Paris",
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }).formatToParts(new Date(Date.now() + minutes * 60_000));
  const part = (type: string) => parts.find((p) => p.type === type)?.value ?? "";
  return `${part("year")}-${part("month")}-${part("day")}T${part("hour")}:${part("minute")}`;
}

/** An event starting in five minutes, reminded ten minutes before: due now. */
async function createDueReminder(page: Page, title: string): Promise<void> {
  await page.goto("/agenda/new");
  await page.getByLabel("Titre").fill(title);
  await page.getByLabel("Début").fill(parisLocal(5));
  await page.getByLabel("Fin").fill(parisLocal(65));
  await page.getByLabel("Rappel").selectOption("10");
  await page.getByRole("button", { name: "Créer l'événement" }).click();
  await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);
}

const sidebar = (page: Page) =>
  page.getByRole("navigation", { name: "Navigation principale" });

test.describe("Notification centre", () => {
  test.use({ viewport: { width: 1440, height: 900 } });

  test("a due reminder is listed unread; opening it marks it read and the count goes down", async ({
    page,
  }) => {
    test.setTimeout(240_000);
    await registerAndLogin(page);
    await page.goto("/groups/new");
    await page.getByLabel("Nom du groupe").fill(FAMILY);
    await page.getByRole("button", { name: "Créer le groupe" }).click();
    await expect(page).toHaveURL(/\/groups\?notice=group_created$/);

    // Nothing yet: the entry has no count, the page says so.
    await page.goto("/notifications");
    await expect(page.getByRole("heading", { name: "Notifications", level: 1 })).toBeVisible();
    await expect(sidebar(page).getByRole("link", { name: "Notifications", exact: true })).toBeVisible();
    await expect(page.getByText("Aucune notification pour le moment.")).toBeVisible();
    await expect(page.getByRole("button", { name: "Tout marquer comme lu" })).toHaveCount(0);

    await createDueReminder(page, "Piscine du soir");
    await createDueReminder(page, "Dentiste");

    // The worker's next pass writes both.
    await expect(async () => {
      await page.goto("/notifications");
      await expect(
        sidebar(page).getByRole("link", { name: "Notifications, 2 non lues", exact: true }),
      ).toBeVisible({ timeout: 1_000 });
    }).toPass({ timeout: 150_000, intervals: [5_000] });

    await expect(page.locator("hgroup .eyebrow")).toHaveText("2 non lues");
    await expect(page.getByRole("heading", { name: "Aujourd’hui" })).toBeVisible();
    const piscine = page.getByRole("button", { name: /Rappel : « Piscine du soir »/ });
    await expect(piscine).toContainText("Agenda ·");
    await expect(piscine).toContainText("Non lue");

    // The bell of the home page, on a phone, says the same count.
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto("/");
    const bell = page.getByRole("link", { name: "Notifications, 2 non lues", exact: true });
    await expect(bell).toBeVisible();
    await expect(bell).toHaveAttribute("href", "/notifications");
    await page.setViewportSize({ width: 1440, height: 900 });

    // The filter keeps the reminders and says which one is on.
    await page.goto("/notifications");
    const filters = page.getByRole("navigation", { name: "Filtres" });
    await filters.getByRole("link", { name: "Agenda" }).click();
    await expect(page).toHaveURL("/notifications?kind=agenda_reminder");
    await expect(filters.getByRole("link", { name: "Agenda" })).toHaveAttribute("aria-current", "true");
    await expect(page.getByRole("button", { name: /Rappel : « Dentiste »/ })).toBeVisible();
    await filters.getByRole("link", { name: "Messages" }).click();
    await expect(page.getByText("Aucune notification ici.")).toBeVisible();

    // Opening one leads to its event and marks it read: the count goes
    // down by one.
    await page.goto("/notifications");
    await page.getByRole("button", { name: /Rappel : « Piscine du soir »/ }).click();
    await expect(page).toHaveURL(/\/agenda\/[0-9a-f-]{36}$/);
    await expect(page.getByRole("heading", { name: "Piscine du soir" })).toBeVisible();
    await expect(
      sidebar(page).getByRole("link", { name: "Notifications, 1 non lue", exact: true }),
    ).toBeVisible();

    await page.goto("/notifications");
    await expect(page.locator("hgroup .eyebrow")).toHaveText("1 non lue");
    const opened = page.getByRole("button", { name: /Rappel : « Piscine du soir »/ });
    await expect(opened).not.toContainText("Non lue");
    await expect(page.getByRole("button", { name: /Rappel : « Dentiste »/ })).toContainText("Non lue");

    // « Tout marquer comme lu » clears the rest.
    await page.getByRole("button", { name: "Tout marquer comme lu" }).click();
    await expect(page).toHaveURL("/notifications");
    await expect(page.locator("hgroup .eyebrow")).toHaveText("Tout est lu");
    await expect(sidebar(page).getByRole("link", { name: "Notifications", exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: /Rappel : « Dentiste »/ })).not.toContainText("Non lue");

    // The settings panel leads to the push preferences.
    await page.getByRole("link", { name: "Régler les notifications" }).click();
    await expect(page).toHaveURL("/account/notifications");
  });
});
