import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken, seedSchoolHoliday } from "../lib/db";

// Issue #565 — the national days on the agenda: public holidays, clock
// changes and observances as a head line of their day, the family's school
// holidays as a band named in words, and the zone and Alsace-Moselle in the
// family settings. The Rust tests check the markup and the failing API;
// only a browser says the line is on screen, and that a phone's list keeps
// a day that carries nothing but a national day. Since #836 the month draws
// them as ocre bars — a school holiday one bar per week it covers — and the
// head line, the band and the phone's list are the week's.

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

/** The head line of a day of the week's grid: its `.nat` paragraphs. */
function nationalLine(page: Page, date: string) {
  return dayCell(page, date).locator(".nat");
}

/** The month's bars carrying `text` (#836): ocre, and not links. */
function bars(page: Page, text: string) {
  return page.locator(".month span.bar.gen", { hasText: text });
}

/** That `bar` starts on `date`'s column of the month and sits in its week. */
async function startsOn(page: Page, bar: ReturnType<Page["locator"]>, date: string): Promise<void> {
  const box = await bar.boundingBox();
  const day = await page.locator(`.month a.day[href="/agenda?view=day&date=${date}"]`).boundingBox();
  if (!box || !day) throw new Error(`no box for ${date}`);
  expect(Math.abs(box.x - day.x), date).toBeLessThanOrEqual(6);
  expect(box.y).toBeGreaterThan(day.y);
  expect(box.y + box.height).toBeLessThanOrEqual(day.y + day.height);
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
    await startsOn(page, bars(page, "Ascension"), "2027-05-06");
    await startsOn(page, bars(page, "Fête des mères"), "2027-05-30");
    // An ocre bar, not an event: not a link.
    await expect(page.getByRole("link", { name: /Ascension/ })).toHaveCount(0);

    await page.goto("/agenda?view=month&date=2026-10-15");
    await startsOn(page, bars(page, "Passage à l'heure d'hiver"), "2026-10-25");

    // The week view: a head line of its day.
    await page.goto("/agenda?view=week&date=2026-10-25");
    await expect(nationalLine(page, "2026-10-25")).toHaveText("Passage à l'heure d'hiver");
    await expect(nationalLine(page, "2026-10-25").getByRole("link")).toHaveCount(0);
  });

  test("a phone's list keeps a day that carries only a national day", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await registerAndLogin(page, "e2e-natphone", "Jours Téléphone");
    await createGroup(page, "Famille Liste");

    // The month's grid draws it on its day, on a phone too (#836).
    await page.goto("/agenda?view=month&date=2026-10-15");
    await startsOn(page, bars(page, "Passage à l'heure d'hiver"), "2026-10-25");

    // The week's list keeps it: 19 to 25 October.
    await page.goto("/agenda?view=week&date=2026-10-25");
    const entry = dayCell(page, "2026-10-25");
    await expect(entry).toBeVisible();
    await expect(entry).toContainText("Passage à l'heure d'hiver");
    // A day with nothing on it stays out of the list.
    await expect(dayCell(page, "2026-10-21")).toBeHidden();
    // The mini grid points at it, by name.
    const link = page
      .locator("table.mini")
      .getByRole("link", { name: "25 octobre, Passage à l'heure d'hiver" });
    await expect(link).toHaveAttribute("href", "/agenda?view=day&date=2026-10-25");
    // In bold, where an empty day, a link too, is not.
    const empty = page.locator('table.mini a[href="/agenda?view=day&date=2026-10-21"]');
    const weight = (l: typeof link) => l.evaluate((e) => getComputedStyle(e).fontWeight);
    expect(await weight(link)).toBe("600");
    expect(await weight(empty)).toBe("400");
    // A list that shows a day is not empty: no « Rien de prévu » under it.
    await expect(page.getByText("Rien de prévu cette semaine.")).toBeHidden();
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

    await openSettings(page, "Famille Vacances");
    const card = page.locator("section#calendrier");
    await expect(card.getByRole("radio", { name: "Aucune" })).toBeChecked();
    await card.getByRole("radio", { name: "Zone B" }).check();
    await card.getByRole("button", { name: "Enregistrer le calendrier" }).click();
    await expect(page).toHaveURL(/\/settings\?notice=calendar_changed$/);
    await expect(page.getByText("Calendrier de la famille enregistré.")).toBeVisible();
    await expect(card.getByRole("radio", { name: "Zone B" })).toBeChecked();

    await page.goto("/agenda?view=month&date=2026-10-15");
    // One bar per week it covers (#836): Saturday 17 and Sunday 18, then
    // the two weeks from Monday 19 to Sunday 1 November.
    const holiday = bars(page, "Vacances de la Toussaint");
    await expect(holiday).toHaveCount(3);
    await startsOn(page, holiday.nth(0), "2026-10-17");
    await startsOn(page, holiday.nth(1), "2026-10-19");
    await startsOn(page, holiday.nth(2), "2026-10-26");

    // The week of 12 October: written on its first day, a band on the days
    // it covers, each named for a screen reader; 16 October is not one.
    await page.goto("/agenda?view=week&date=2026-10-12");
    await expect(nationalLine(page, "2026-10-17")).toHaveText("Vacances de la Toussaint");
    await expect(page.locator("table.cal td.hol")).toHaveCount(2);
    await expect(dayCell(page, "2026-10-18")).toHaveClass(/\bhol\b/);
    await expect(dayCell(page, "2026-10-18")).toContainText("Vacances de la Toussaint");
    const before = dayCell(page, "2026-10-16");
    await expect(before).not.toHaveClass(/\bhol\b/);
    // Forced colours drop the band's shadow: a top border stands in for it.
    await page.emulateMedia({ forcedColors: "active" });
    await expect(dayCell(page, "2026-10-17")).toHaveCSS("border-top-width", "4px");
    await expect(before).not.toHaveCSS("border-top-width", "4px");
    await page.emulateMedia({ forcedColors: "none" });

    // Alsace-Moselle adds Good Friday (26 March 2027).
    await page.goto("/agenda?view=month&date=2027-03-15");
    await expect(page.getByText("Vendredi saint")).toHaveCount(0);
    await openSettings(page, "Famille Vacances");
    await page.getByRole("checkbox", { name: "Alsace-Moselle" }).check();
    await page.getByRole("button", { name: "Enregistrer le calendrier" }).click();
    await expect(page).toHaveURL(/\/settings\?notice=calendar_changed$/);
    await page.goto("/agenda?view=month&date=2027-03-15");
    await startsOn(page, bars(page, "Vendredi saint"), "2027-03-26");
    // The zone was kept.
    await page.goto("/agenda?view=month&date=2026-10-15");
    await expect(bars(page, "Vacances de la Toussaint")).toHaveCount(3);
  });
});
