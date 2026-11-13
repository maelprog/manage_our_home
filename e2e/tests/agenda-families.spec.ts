import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";
import { parisDayOfMonth } from "../lib/dates";

// Issue #622 — the agenda of several families. The Rust tests cover the
// selection's parsing, its check against the visitor's families, the merge
// of their occurrences and the page when one family fails; only a browser
// says that the selector, the cookie and the links of the event pages work
// together, and that the active family is left where it was.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-families-password-1";

// Neither name holds a word of a label on the pages under test (getByLabel
// matches substrings, the family's name included: see agenda.spec.ts).
const F1 = "Foyer Un";
const F2 = "Foyer Deux";

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

/** Creates a family, which becomes the active one. */
async function createGroup(page: Page, name: string): Promise<void> {
  await page.goto("/groups/new");
  await page.getByLabel("Nom du groupe").fill(name);
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
}

/** Fills and submits the sheet already open on `page`. */
async function submitSheet(page: Page, title: string, day: number): Promise<void> {
  const date = parisDayOfMonth(day);
  await page.getByLabel("Titre").fill(title);
  await page.getByLabel("Début").fill(`${date}T10:00`);
  await page.getByLabel("Fin").fill(`${date}T11:00`);
  await page.getByRole("button", { name: "Créer l'événement" }).click();
  await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);
}

/** The active family, as the sidebar's profile block names it (#523). */
function activeFamily(page: Page) {
  return page.locator('header a[href="/account"] .muted');
}

function selector(page: Page) {
  return page.getByRole("form", { name: "Familles affichées" });
}

function box(page: Page, name: string) {
  return selector(page).getByRole("checkbox", { name, exact: true });
}

/** Clicks a box of the selector, which sends the form at once (#750). */
async function toggle(page: Page, name: string): Promise<void> {
  const before = page.url();
  await box(page, name).click();
  await page.waitForURL((url) => url.href !== before);
}

/**
 * Ticks exactly `families` in the selector, one box at a time, each one
 * reloading the agenda (#750): the wanted ones first, so that the last
 * family ticked is never the one unticked.
 */
async function show(page: Page, families: string[]): Promise<void> {
  for (const name of [F1, F2]) {
    if (families.includes(name) && !(await box(page, name).isChecked())) await toggle(page, name);
  }
  for (const name of [F1, F2]) {
    if (!families.includes(name) && (await box(page, name).isChecked())) await toggle(page, name);
  }
  await expect(page).toHaveURL(/familles=/);
}

/** The chip of `title`, whose name ends with its family's in a view of several. */
function chip(page: Page, title: string) {
  return page.locator("a.chip", { hasText: title });
}

test.describe("Agenda — several families (#622)", () => {
  test("one family, the other, both; the selection survives › and a reload; the active family stays", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-agfam", "Agenda Familles");
    // F2 first, then F1: F1 is the active family from here on.
    await createGroup(page, F2);
    await page.goto("/agenda/new");
    await submitSheet(page, "Piscine Deux", 15);
    await createGroup(page, F1);
    await page.goto("/agenda/new");
    await submitSheet(page, "Dentiste Un", 16);

    // Without a selection: the active family alone, as before.
    await page.goto("/agenda");
    await expect(activeFamily(page)).toHaveText(F1);
    await expect(chip(page, "Dentiste Un")).toBeVisible();
    await expect(chip(page, "Piscine Deux")).toHaveCount(0);

    // F2 alone.
    await show(page, [F2]);
    await expect(chip(page, "Piscine Deux")).toBeVisible();
    await expect(chip(page, "Dentiste Un")).toHaveCount(0);

    // F1 alone again.
    await show(page, [F1]);
    await expect(chip(page, "Dentiste Un")).toBeVisible();
    await expect(chip(page, "Piscine Deux")).toHaveCount(0);

    // Both: each event with its family's name.
    await show(page, [F1, F2]);
    await expect(page.getByRole("link", { name: `Piscine Deux (${F2})` })).toBeVisible();
    await expect(page.getByRole("link", { name: `Dentiste Un (${F1})` })).toBeVisible();

    // The next month keeps the selection, in the URL.
    await page.getByRole("link", { name: "Mois suivant" }).click();
    await expect(page).toHaveURL(/familles=/);
    await expect(box(page, F1)).toBeChecked();
    await expect(box(page, F2)).toBeChecked();

    // And /agenda with no parameter at all, through the cookie.
    await page.goto("/agenda");
    await expect(page.getByRole("link", { name: `Piscine Deux (${F2})` })).toBeVisible();
    await expect(page.getByRole("link", { name: `Dentiste Un (${F1})` })).toBeVisible();

    // The active family has not moved.
    await expect(activeFamily(page)).toHaveText(F1);
  });

  test("from the combined view, open then edit an event of the other family; create in it; open it from that family alone", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-agfamedit", "Agenda Familles Edit");
    await createGroup(page, F2);
    await page.goto("/agenda/new");
    await submitSheet(page, "Piscine Deux", 15);
    await createGroup(page, F1);
    await page.goto("/agenda");
    await show(page, [F1, F2]);

    // Open F2's event from the view of both, F1 being active.
    await chip(page, "Piscine Deux").click();
    await expect(page.getByRole("heading", { level: 1, name: "Piscine Deux" })).toBeVisible();
    await expect(page).toHaveURL(/famille=/);
    await expect(page.getByText(`Famille : ${F2}`)).toBeVisible();

    // Edit it.
    await page.getByRole("link", { name: "Modifier" }).click();
    await page.getByLabel("Titre").fill("Piscine Deux déplacée");
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page.getByText("Événement mis à jour.")).toBeVisible();
    await expect(page.getByRole("heading", { level: 1, name: "Piscine Deux déplacée" })).toBeVisible();
    await expect(activeFamily(page)).toHaveText(F1);

    // Create from the view of both, choosing F2.
    await page.goto("/agenda/new");
    const choice = page.getByRole("navigation", { name: "Famille" });
    await expect(choice.getByRole("link", { name: F1 })).toHaveAttribute("aria-current", "true");
    await choice.getByRole("link", { name: F2 }).click();
    await expect(page.locator(".sheet .eyebrow")).toHaveText(F2);
    await submitSheet(page, "Goûter Deux", 17);

    // It is F2's: shown with F2 alone, not with F1 alone.
    await page.goto("/agenda");
    await show(page, [F2]);
    await expect(chip(page, "Goûter Deux")).toBeVisible();
    await expect(chip(page, "Piscine Deux déplacée")).toBeVisible();
    await show(page, [F1]);
    await expect(chip(page, "Goûter Deux")).toHaveCount(0);
    await expect(activeFamily(page)).toHaveText(F1);

    // From F2 alone, F1 still active: its event opens in F2.
    await show(page, [F2]);
    await chip(page, "Goûter Deux").click();
    await expect(page.getByRole("heading", { level: 1, name: "Goûter Deux" })).toBeVisible();
    await expect(page).toHaveURL(/famille=/);
    await expect(page.getByText(`Famille : ${F2}`)).toBeVisible();
    await expect(activeFamily(page)).toHaveText(F1);
  });
});

// #793 — the day view of several families: each block names its family,
// and the arrows keep the selection.
test.describe("Agenda — the day view of several families (#793)", () => {
  test("both families' events side by side, each named; › keeps the selection; the other's opens in it", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-agfamday", "Agenda Familles Jour");
    await createGroup(page, F2);
    await page.goto("/agenda/new");
    await submitSheet(page, "Piscine Deux", 15);
    await createGroup(page, F1);
    await page.goto("/agenda/new");
    await submitSheet(page, "Dentiste Un", 15);
    const date = parisDayOfMonth(15);
    // A quarter of an hour: a short block, on one line.
    await page.goto("/agenda/new");
    await page.getByLabel("Titre").fill("Pharmacie");
    await page.getByLabel("Début").fill(`${date}T12:15`);
    await page.getByLabel("Fin").fill(`${date}T12:30`);
    await page.getByRole("button", { name: "Créer l'événement" }).click();
    await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);
    await page.goto("/agenda");
    await show(page, [F1, F2]);

    await page.getByRole("navigation", { name: "Affichage" }).getByRole("link", { name: "Jour", exact: true }).click();
    await expect(page).toHaveURL(/view=day/);
    await expect(page).toHaveURL(/familles=/);
    await page.goto(`/agenda?view=day&date=${date}&familles=toutes`);
    const pool = page.locator("a.blk", { hasText: "Piscine Deux" });
    const dentist = page.locator("a.blk", { hasText: "Dentiste Un" });
    await expect(pool).toContainText(F2);
    await expect(dentist).toContainText(F1);
    // Both 10:00–11:00: side by side.
    const [a, b] = [await dentist.boundingBox(), await pool.boundingBox()];
    if (!a || !b) throw new Error("the blocks have no box");
    expect(Math.abs(a.y - b.y)).toBeLessThanOrEqual(1);
    expect(Math.abs(a.x - b.x)).toBeGreaterThanOrEqual(Math.min(a.width, b.width) - 1);
    // The short block keeps its title whole, its family on the same line.
    const pharmacy = page.locator("a.blk.short", { hasText: "Pharmacie" });
    await expect(pharmacy).toContainText(F1);
    const title = pharmacy.locator("strong");
    const fam = pharmacy.locator(".fam");
    expect(await title.evaluate((e) => e.scrollWidth <= e.clientWidth)).toBe(true);
    const [t, f] = [await title.boundingBox(), await fam.boundingBox()];
    if (!t || !f) throw new Error("the short block's parts have no box");
    expect(Math.abs(t.y + t.height / 2 - (f.y + f.height / 2))).toBeLessThanOrEqual(2);
    expect(f.x).toBeGreaterThan(t.x + t.width);

    await page.getByRole("link", { name: "Jour suivant", exact: true }).click();
    await expect(page).toHaveURL(/view=day&date=.*familles=/);
    await expect(box(page, F1)).toBeChecked();
    await expect(box(page, F2)).toBeChecked();

    // F2's event, F1 being active, opens in F2.
    await page.goto(`/agenda?view=day&date=${date}&familles=toutes`);
    await pool.click();
    await expect(page.getByRole("heading", { level: 1, name: "Piscine Deux" })).toBeVisible();
    await expect(page).toHaveURL(/famille=/);
    await expect(activeFamily(page)).toHaveText(F1);
  });

  // A block of 45 minutes is two lines tall: its family, on a third line,
  // was cut through by the block's edge, half its glyphs showing.
  test("a block under an hour keeps its family whole, on a phone and on a PC", async ({ page }) => {
    await registerAndLogin(page, "e2e-agfammid", "Agenda Familles Mid");
    await createGroup(page, F2);
    await createGroup(page, F1);
    const date = parisDayOfMonth(15);
    await page.goto("/agenda/new");
    await page.getByLabel("Titre").fill("Cours de piano");
    await page.getByLabel("Début").fill(`${date}T14:00`);
    await page.getByLabel("Fin").fill(`${date}T14:45`);
    await page.getByRole("button", { name: "Créer l'événement" }).click();
    await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);

    for (const viewport of [
      { width: 390, height: 844 },
      { width: 1280, height: 800 },
    ]) {
      await page.setViewportSize(viewport);
      await page.goto(`/agenda?view=day&date=${date}&familles=toutes`);
      const block = page.locator("a.blk", { hasText: "Cours de piano" });
      await expect(block).toContainText(F1);
      const fit = await block.evaluate((blk) => {
        const b = blk.getBoundingClientRect();
        const s = getComputedStyle(blk);
        const bottom = b.bottom - parseFloat(s.borderBottomWidth) - parseFloat(s.paddingBottom);
        const fam = blk.querySelector(".fam")!;
        const f = fam.getBoundingClientRect();
        return { famBottom: f.bottom, bottom, famRight: f.right, right: b.right, famWidth: f.width };
      });
      expect(fit.famBottom, `${viewport.width}px`).toBeLessThanOrEqual(fit.bottom + 0.5);
      expect(fit.famRight, `${viewport.width}px`).toBeLessThanOrEqual(fit.right + 0.5);
      expect(fit.famWidth, `${viewport.width}px`).toBeGreaterThan(0);
    }
  });
});

// #750 — on a phone, the selector applies each box at once: no « Afficher »
// to press, « Toutes » ticking every family or leaving the active one, and
// the last family kept ticked.
const F3 = "Foyer Trois";

test.describe("Agenda — the selector applies each box (#750)", () => {
  test("Toutes ticks every family, unticked leaves the active one; the last one stays", async ({ page }) => {
    await registerAndLogin(page, "e2e-agfamauto", "Agenda Familles Auto");
    // F3, F2, then F1: F1 is the active family.
    await createGroup(page, F3);
    await page.goto("/agenda/new");
    await submitSheet(page, "Judo Trois", 14);
    await createGroup(page, F2);
    await page.goto("/agenda/new");
    await submitSheet(page, "Piscine Deux", 15);
    await createGroup(page, F1);
    await page.goto("/agenda/new");
    await submitSheet(page, "Dentiste Un", 16);

    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto("/agenda");
    await expect(box(page, F1)).toBeChecked();
    await expect(box(page, "Toutes")).not.toBeChecked();
    await expect(selector(page).getByRole("button", { name: "Afficher" })).toBeHidden();

    // « Toutes »: every family ticked and shown, without « Afficher ».
    await toggle(page, "Toutes");
    await expect(page).toHaveURL(/familles=toutes/);
    for (const name of [F1, F2, F3, "Toutes"]) await expect(box(page, name)).toBeChecked();
    await expect(page.getByRole("link", { name: `Judo Trois (${F3})` })).toBeVisible();
    await expect(page.getByRole("link", { name: `Piscine Deux (${F2})` })).toBeVisible();
    await expect(page.getByRole("link", { name: `Dentiste Un (${F1})` })).toBeVisible();

    // Unticked: the active family alone.
    await toggle(page, "Toutes");
    await expect(page).not.toHaveURL(/familles=toutes/);
    await expect(box(page, F1)).toBeChecked();
    for (const name of [F2, F3, "Toutes"]) await expect(box(page, name)).not.toBeChecked();
    await expect(chip(page, "Dentiste Un")).toBeVisible();
    await expect(chip(page, "Piscine Deux")).toHaveCount(0);
    await expect(chip(page, "Judo Trois")).toHaveCount(0);

    // One family, then the two others one by one: « Toutes » follows.
    await toggle(page, F2);
    await expect(box(page, "Toutes")).not.toBeChecked();
    await expect(page.getByRole("link", { name: `Piscine Deux (${F2})` })).toBeVisible();
    await toggle(page, F3);
    await expect(box(page, "Toutes")).toBeChecked();
    await toggle(page, F2);
    await toggle(page, F1);
    await expect(box(page, F3)).toBeChecked();
    for (const name of [F1, F2, "Toutes"]) await expect(box(page, name)).not.toBeChecked();
    await expect(chip(page, "Judo Trois")).toBeVisible();

    // The last family: a modal, the box still ticked, nothing sent.
    const before = page.url();
    await box(page, F3).click();
    const dialog = page.getByRole("dialog");
    await expect(dialog).toHaveText(/Au moins une famille doit rester affichée\./);
    await expect(box(page, F3)).toBeChecked();
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
    await expect(box(page, F3)).toBeFocused();
    expect(page.url()).toBe(before);
    await expect(chip(page, "Judo Trois")).toBeVisible();

    // The active family has not moved.
    await page.setViewportSize({ width: 1440, height: 900 });
    await expect(activeFamily(page)).toHaveText(F1);
  });
});
