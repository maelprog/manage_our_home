import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken, seedSchoolHoliday } from "../lib/db";

// Issue #565 — the national days on the agenda: public holidays, clock
// changes and observances as a head line of their day, the family's school
// holidays as a band named in words, and the zone and Alsace-Moselle in the
// family settings. The Rust tests check the markup and the failing API;
// only a browser says the line is on screen, and that a phone's list keeps
// a day that carries nothing but a national day.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-national-password-1";

async function registerAndLogin(page: Page, prefix: string, displayName: string): Promise<void> {
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
}

async function createGroup(page: Page, name: string): Promise<void> {
  await page.goto("/groups/new");
  await page.getByLabel("Nom du groupe").fill(name);
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
}

async function openSettings(page: Page, family: string): Promise<void> {
  await page.goto("/groups");
  await page.locator("li", { hasText: family }).getByRole("link", { name: "Paramètres" }).click();
  await expect(page.getByRole("heading", { level: 1, name: family })).toBeVisible();
}

/** A day's cell of the grid, found by its number's link to its day view
 *  (#793; by an anchor `#jAAAA-MM-JJ` before). */
function dayCell(page: Page, date: string) {
  return page
    .locator("table.cal td.cal-cell")
    .filter({ has: page.locator(`.cal-day a[href="/agenda?view=day&date=${date}"]`) });
}

/** The head line of a day of the grid: its `.nat` paragraphs. */
function nationalLine(page: Page, date: string) {
  return dayCell(page, date).locator(".nat");
}

test.describe("Agenda — jours nationaux (#565)", () => {
  test("May 2027 shows Ascension and Mother's Day, October 2026 the clock change on the 25th", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    await registerAndLogin(page, "e2e-natdays", "Jours Nationaux");
    await createGroup(page, "Famille Calendrier");

    await page.goto("/agenda?view=month&date=2027-05-15");
    // Easter 2027 is 28 March: Ascension on 6 May. Pentecost is 16 May, so
    // Mother's Day stays on the last Sunday, the 30th.
    await expect(nationalLine(page, "2027-05-06")).toHaveText("Ascension");
    await expect(nationalLine(page, "2027-05-30")).toHaveText("Fête des mères");
    // A head line, not an event: no link in it.
    await expect(nationalLine(page, "2027-05-06").getByRole("link")).toHaveCount(0);
    await expect(page.getByRole("link", { name: /Ascension/ })).toHaveCount(0);

    await page.goto("/agenda?view=month&date=2026-10-15");
    await expect(nationalLine(page, "2026-10-25")).toHaveText("Passage à l'heure d'hiver");

    // The week view is the same grid.
    await page.goto("/agenda?view=week&date=2026-10-25");
    await expect(nationalLine(page, "2026-10-25")).toHaveText("Passage à l'heure d'hiver");
  });

  test("a phone's list keeps a day that carries only a national day", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await registerAndLogin(page, "e2e-natphone", "Jours Téléphone");
    await createGroup(page, "Famille Liste");

    await page.goto("/agenda?view=month&date=2026-10-15");
    const entry = dayCell(page, "2026-10-25");
    await expect(entry).toBeVisible();
    await expect(entry).toContainText("Passage à l'heure d'hiver");
    // A day with nothing on it stays out of the list.
    await expect(page.locator("table.cal td.cal-cell").filter({ hasText: /^\s*14\s*$/ })).toBeHidden();
    // The mini grid points at it, by name.
    const link = page
      .locator("table.mini")
      .getByRole("link", { name: "25 octobre, Passage à l'heure d'hiver" });
    await expect(link).toHaveAttribute("href", "/agenda?view=day&date=2026-10-25");
    // A list that shows a day is not empty: no « Rien de prévu » under it.
    await expect(page.getByText("Rien de prévu ce mois-ci.")).toBeHidden();
  });

  test("zone B set in the settings brings its Toussaint holidays to the agenda", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    // The cache `jobs::school_holidays` fills, pre-filled: the zone B
    // Toussaint of the recorded answer (#563), 17 October to 1 November.
    await seedSchoolHoliday({
      zone: "B",
      label: "Vacances de la Toussaint",
      startsOn: "2026-10-17",
      endsOn: "2026-11-01",
      schoolYear: "2026-2027",
    });
    await registerAndLogin(page, "e2e-natzone", "Jours Zone");
    await createGroup(page, "Famille Vacances");

    // No zone yet: no school holidays.
    await page.goto("/agenda?view=month&date=2026-10-15");
    await expect(page.getByText("Vacances de la Toussaint")).toHaveCount(0);
    await expect(page.locator("table.cal td.hol")).toHaveCount(0);

    await openSettings(page, "Famille Vacances");
    const card = page.locator("section#calendrier");
    await expect(card.getByRole("radio", { name: "Aucune" })).toBeChecked();
    await card.getByRole("radio", { name: "Zone B" }).check();
    await card.getByRole("button", { name: "Enregistrer le calendrier" }).click();
    await expect(page).toHaveURL(/\/settings\?notice=calendar_changed$/);
    await expect(page.getByText("Calendrier de la famille enregistré.")).toBeVisible();
    await expect(card.getByRole("radio", { name: "Zone B" })).toBeChecked();

    await page.goto("/agenda?view=month&date=2026-10-15");
    // Written on its first day and on each Monday it covers, and a band on
    // all sixteen days, each named for a screen reader.
    await expect(nationalLine(page, "2026-10-17")).toHaveText("Vacances de la Toussaint");
    await expect(nationalLine(page, "2026-10-19")).toHaveText("Vacances de la Toussaint");
    await expect(page.locator("table.cal td.hol")).toHaveCount(16);
    await expect(dayCell(page, "2026-10-17")).toHaveClass(/\bhol\b/);
    await expect(page.locator("table.cal td.hol").nth(4)).toContainText("Vacances de la Toussaint");
    // The day after the holidays is not one.
    const after = page.locator("table.cal td.cal-cell").nth(
      // October 2026's grid opens on Monday 28 September: 2 November is
      // the 36th day.
      35,
    );
    await expect(after).not.toHaveClass(/\bhol\b/);
    // Forced colours drop the band's shadow: a top border stands in for it.
    await page.emulateMedia({ forcedColors: "active" });
    await expect(dayCell(page, "2026-10-17")).toHaveCSS("border-top-width", "4px");
    await expect(after).not.toHaveCSS("border-top-width", "4px");
    await page.emulateMedia({ forcedColors: "none" });

    // Alsace-Moselle adds Good Friday (26 March 2027).
    await page.goto("/agenda?view=month&date=2027-03-15");
    await expect(page.getByText("Vendredi saint")).toHaveCount(0);
    await openSettings(page, "Famille Vacances");
    await page.getByRole("checkbox", { name: "Alsace-Moselle" }).check();
    await page.getByRole("button", { name: "Enregistrer le calendrier" }).click();
    await expect(page).toHaveURL(/\/settings\?notice=calendar_changed$/);
    await page.goto("/agenda?view=month&date=2027-03-15");
    await expect(nationalLine(page, "2027-03-26")).toHaveText("Vendredi saint");
    // The zone was kept.
    await page.goto("/agenda?view=month&date=2026-10-15");
    await expect(page.locator("table.cal td.hol")).toHaveCount(16);
  });
});
