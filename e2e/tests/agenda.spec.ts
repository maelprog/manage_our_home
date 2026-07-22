import { Browser, expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Front epic #3 — Agenda (issue #18): every user journey the epic
// introduces, happy paths plus the documented error states from
// apps/api/src/agenda/'s error tables (403 permission bar, 404 unknown
// event, 422 unsupported_file_type/file_too_large on upload) and the RRULE
// v1 / per-occurrence-completion behaviour that's the crux of the epic.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-agenda-password-1";

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

/** A fixed date in the current month, avoiding month boundaries. */
function dayThisMonth(day: number): string {
  const now = new Date();
  const y = now.getFullYear();
  const m = String(now.getMonth() + 1).padStart(2, "0");
  return `${y}-${m}-${String(day).padStart(2, "0")}`;
}

interface EventOpts {
  title: string;
  start: string; // datetime-local, e.g. 2026-07-15T10:00
  end: string;
  isTask?: boolean;
  freq?: "none" | "daily" | "weekly" | "monthly" | "yearly";
}

/** Fill and submit /agenda/new; asserts the redirect + notice. */
async function createEvent(page: Page, opts: EventOpts): Promise<void> {
  await page.goto("/agenda/new");
  await page.getByLabel("Titre").fill(opts.title);
  await page.getByLabel("Début").fill(opts.start);
  await page.getByLabel("Fin").fill(opts.end);
  if (opts.isTask) await page.locator('input[name="is_task"]').check();
  if (opts.freq && opts.freq !== "none") {
    await page.getByLabel("Fréquence").selectOption(opts.freq);
  }
  await page.getByRole("button", { name: "Créer l'événement" }).click();
  await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);
  await expect(page.getByText("Événement créé.")).toBeVisible();
}

/** Opens an event's detail by clicking its chip in the calendar. */
async function openEventDetail(page: Page, title: string): Promise<void> {
  await page.goto("/agenda");
  await page.getByRole("link", { name: new RegExp(title) }).first().click();
  await expect(page.getByRole("heading", { name: title })).toBeVisible();
}

test.describe("Agenda — calendar & events", () => {
  test("create an event, see it in month and week views", async ({ page }) => {
    await registerAndLogin(page, "e2e-agevent", "Agenda Creator");
    await createGroup(page, "Famille Agenda");

    const date = dayThisMonth(15);
    await createEvent(page, {
      title: "Réunion parents",
      start: `${date}T10:00`,
      end: `${date}T11:00`,
    });

    // Month view (default) shows the chip in the day cell.
    await page.goto("/agenda");
    await expect(page.getByRole("link", { name: /Réunion parents/ })).toBeVisible();

    // Week view of that week shows it too.
    await page.goto(`/agenda?view=week&date=${date}`);
    await expect(page.getByRole("link", { name: /Réunion parents/ })).toBeVisible();
  });

  test("unknown event id shows the not-found page", async ({ page }) => {
    await registerAndLogin(page, "e2e-ag404", "Agenda 404");
    await createGroup(page, "Famille 404");
    await page.goto("/agenda/00000000-0000-4000-8000-000000000000");
    await expect(page.getByRole("heading", { name: "Événement introuvable" })).toBeVisible();
  });
});

test.describe("Agenda — tasks & completion", () => {
  test("recurring task: completing one occurrence leaves the others to do", async ({ page }) => {
    await registerAndLogin(page, "e2e-agrec", "Recurring Task User");
    await createGroup(page, "Famille Récurrence");

    const date = dayThisMonth(15);
    await createEvent(page, {
      title: "Sortir les poubelles",
      start: `${date}T08:00`,
      end: `${date}T08:15`,
      isTask: true,
      freq: "weekly",
    });

    await openEventDetail(page, "Sortir les poubelles");
    await expect(page.getByText("indépendante par occurrence")).toBeVisible();

    const occurrences = page.locator("li", { has: page.locator('input[name="occurrence_at"]') });
    await expect(occurrences.first()).toBeVisible();
    const total = await occurrences.count();
    expect(total).toBeGreaterThan(1);

    // Complete the first occurrence only.
    await occurrences.first().getByRole("button", { name: "Marquer faite" }).click();
    await expect(page.getByText("Complétion mise à jour.")).toBeVisible();

    // Exactly one occurrence is now done (✅ + "Marquer à faire"); the rest
    // are still to do — completion is per-occurrence, not on the series row.
    await expect(page.getByRole("button", { name: "Marquer à faire" })).toHaveCount(1);
    await expect(page.getByRole("button", { name: "Marquer faite" })).toHaveCount(total - 1);
  });

  test("one-off task completes and un-completes", async ({ page }) => {
    await registerAndLogin(page, "e2e-agoneoff", "One-off Task User");
    await createGroup(page, "Famille Tâche");

    const date = dayThisMonth(16);
    await createEvent(page, {
      title: "Appeler le plombier",
      start: `${date}T14:00`,
      end: `${date}T14:30`,
      isTask: true,
    });

    await openEventDetail(page, "Appeler le plombier");
    await expect(page.getByText("⬜ À faire")).toBeVisible();
    await page.getByRole("button", { name: "Marquer faite" }).click();
    await expect(page.getByText("✅ Fait")).toBeVisible();
    await page.getByRole("button", { name: "Marquer à faire" }).click();
    await expect(page.getByText("⬜ À faire")).toBeVisible();
  });
});

test.describe("Agenda — edit & delete", () => {
  test("edit an event's title", async ({ page }) => {
    await registerAndLogin(page, "e2e-agedit", "Edit User");
    await createGroup(page, "Famille Édition");
    const date = dayThisMonth(17);
    await createEvent(page, {
      title: "Titre avant",
      start: `${date}T09:00`,
      end: `${date}T10:00`,
    });

    await openEventDetail(page, "Titre avant");
    await page.getByRole("link", { name: "Modifier" }).click();
    await page.getByLabel("Titre").fill("Titre après");
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page.getByText("Événement mis à jour.")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Titre après" })).toBeVisible();
  });

  test("delete an event removes it from the calendar", async ({ page }) => {
    await registerAndLogin(page, "e2e-agdel", "Delete User");
    await createGroup(page, "Famille Suppression");
    const date = dayThisMonth(18);
    await createEvent(page, {
      title: "À supprimer",
      start: `${date}T09:00`,
      end: `${date}T10:00`,
    });

    await openEventDetail(page, "À supprimer");
    await page.getByRole("button", { name: "Supprimer" }).click();
    await expect(page).toHaveURL(/\/agenda\?notice=event_deleted$/);
    await expect(page.getByText("Événement supprimé.")).toBeVisible();
    await page.goto("/agenda");
    await expect(page.getByRole("link", { name: /À supprimer/ })).toHaveCount(0);
  });
});

test.describe("Agenda — permission bar", () => {
  test("a standard member cannot edit or delete another member's event", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-agpermowner", "Perm Owner");
    await createGroup(page, "Famille Droits");

    // Invite a second user into the family.
    await page.goto("/groups");
    await page.locator("li", { hasText: "Famille Droits" }).getByRole("link", { name: "Membres" }).click();
    await page.getByRole("button", { name: "Créer une invitation" }).click();
    const href = await page.locator(".notice.success a").getAttribute("href");
    if (!href) throw new Error("no invitation link");

    const date = dayThisMonth(19);
    await createEvent(page, {
      title: "Événement du proprio",
      start: `${date}T09:00`,
      end: `${date}T10:00`,
    });

    const context = await browser.newContext();
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-agpermmember", "Perm Member");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    // The member sees the event but gets no edit/delete controls.
    await openEventDetail(member, "Événement du proprio");
    await expect(member.getByText("Seul le créateur ou un administrateur")).toBeVisible();
    await expect(member.getByRole("link", { name: "Modifier" })).toHaveCount(0);
    await expect(member.getByRole("button", { name: "Supprimer" })).toHaveCount(0);
    await context.close();
  });
});

// A 1x1 transparent PNG (valid signature so the backend's MIME sniff passes).
const PNG_1X1 = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
  "base64",
);

test.describe("Agenda — attachments", () => {
  test("upload a PNG, it lists and downloads via a presigned redirect; wrong type is rejected", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-agatt", "Attachment User");
    await createGroup(page, "Famille Fichiers");
    const date = dayThisMonth(20);
    await createEvent(page, {
      title: "Événement avec fichier",
      start: `${date}T09:00`,
      end: `${date}T10:00`,
    });
    await openEventDetail(page, "Événement avec fichier");

    // Reject a disallowed extension client-side (no API call).
    await page.locator('input[name="file"]').setInputFiles({
      name: "notes.txt",
      mimeType: "text/plain",
      buffer: Buffer.from("hello"),
    });
    await page.getByRole("button", { name: "Envoyer" }).click();
    await expect(page.getByText("Type de fichier non autorisé")).toBeVisible();

    // Upload a valid PNG.
    await page.locator('input[name="file"]').setInputFiles({
      name: "photo.png",
      mimeType: "image/png",
      buffer: PNG_1X1,
    });
    await page.getByRole("button", { name: "Envoyer" }).click();
    await expect(page.getByText("Pièce jointe ajoutée.")).toBeVisible();
    const link = page.getByRole("link", { name: "photo.png" });
    await expect(link).toBeVisible();

    // The download endpoint 302s onto a short-lived presigned MinIO URL.
    const href = await link.getAttribute("href");
    if (!href) throw new Error("no download link");
    const resp = await page.request.get(href, { maxRedirects: 0 });
    expect(resp.status()).toBeGreaterThanOrEqual(300);
    expect(resp.status()).toBeLessThan(400);
    const location = resp.headers()["location"] ?? "";
    expect(location).toContain("X-Amz-"); // presigned query params

    // Delete it.
    await page.getByRole("button", { name: "Supprimer" }).click();
    await expect(page.getByText("Pièce jointe supprimée.")).toBeVisible();
    await expect(page.getByRole("link", { name: "photo.png" })).toHaveCount(0);
  });
});
