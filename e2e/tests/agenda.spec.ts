import { expect, Page, test } from "@playwright/test";
import { crc32, deflateSync } from "node:zlib";
import { fetchEventBounds, fetchVerificationToken } from "../lib/db";
import { parisDay, parisDayOfMonth } from "../lib/dates";
import { chooseTheme, resolvedTheme } from "../lib/theme";

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

/** A fixed date in the current month, avoiding month boundaries. */
function dayThisMonth(day: number): string {
  return parisDayOfMonth(day);
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
    // The settings open under « Événement récurrent » (#698).
    await page.getByRole("checkbox", { name: "Événement récurrent" }).check();
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

  test("a malformed rid or offset reads as absent: the detail page renders (#580)", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-agbadquery", "Agenda Query");
    await createGroup(page, "Famille Paramètres");
    const date = dayThisMonth(16);
    await createEvent(page, { title: "Dentiste", start: `${date}T09:00`, end: `${date}T10:00` });
    await openEventDetail(page, "Dentiste");
    const detail = new URL(page.url()).pathname;
    const rid = "00000000-0000-4000-8000-000000000000";

    // rid unconvertible: no reminder banner, the page itself unchanged.
    let response = await page.goto(`${detail}?rid=x`);
    expect(response?.status()).toBe(200);
    await expect(page.getByRole("heading", { name: "Dentiste" })).toBeVisible();
    await expect(page.getByText("Rappel actif")).toHaveCount(0);

    // offset unconvertible: the banner shows, read as offset 0.
    response = await page.goto(`${detail}?rid=${rid}&offset=abc`);
    expect(response?.status()).toBe(200);
    await expect(page.getByText("Rappel actif : à l'heure de l'événement.")).toBeVisible();

    // A valid pair keeps its effect.
    await page.goto(`${detail}?rid=${rid}&offset=30`);
    await expect(page.getByText("Rappel actif : 30 min avant.")).toBeVisible();

    // A repeated parameter keeps its first occurrence (#582).
    response = await page.goto(`${detail}?rid=${rid}&rid=x&offset=30&offset=abc`);
    expect(response?.status()).toBe(200);
    await expect(page.getByText("Rappel actif : 30 min avant.")).toBeVisible();
    response = await page.goto(`${detail}?rid=x&rid=${rid}`);
    expect(response?.status()).toBe(200);
    await expect(page.getByRole("heading", { name: "Dentiste" })).toBeVisible();
    await expect(page.getByText("Rappel actif")).toHaveCount(0);
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
  test("upload a PNG, it lists and downloads from this origin; wrong type is rejected", async ({
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

    // The file comes from this origin (#775), never from a redirect onto
    // the storage: its bytes, its sniffed type, as a download. Its tile
    // leads to it (#776), the viewer's « Télécharger » adds `?download=1`.
    const tileHref = await link.getAttribute("href");
    if (!tileHref) throw new Error("no file link");
    expect(tileHref).toMatch(/\/file$/);
    const href = `${tileHref}?download=1`;
    const resp = await page.request.get(href, { maxRedirects: 0 });
    expect(resp.status()).toBe(200);
    expect(resp.headers()["location"]).toBeUndefined();
    expect(resp.headers()["content-type"]).toBe("image/png");
    expect(resp.headers()["x-content-type-options"]).toBe("nosniff");
    expect(resp.headers()["content-disposition"]).toContain('attachment; filename="photo.png"');
    expect(Buffer.compare(await resp.body(), PNG_1X1)).toBe(0);

    // A range of it, as a video player asks.
    const ranged = await page.request.get(href, { headers: { Range: "bytes=0-7" } });
    expect(ranged.status()).toBe(206);
    expect(ranged.headers()["content-range"]).toBe(`bytes 0-7/${PNG_1X1.length}`);
    expect(Buffer.compare(await ranged.body(), PNG_1X1.subarray(0, 8))).toBe(0);

    // Delete it from its viewer (#776), once confirmed — the event's own
    // "Supprimer" button is also on this page.
    await link.click();
    page.once("dialog", (d) => d.accept());
    await page.getByRole("dialog", { name: "photo.png" }).getByRole("button", { name: "Supprimer" }).click();
    await expect(page.getByText("Pièce jointe supprimée.")).toBeVisible();
    await expect(page.getByRole("link", { name: "photo.png" })).toHaveCount(0);
  });

  // apps/web's upload page had axum's 2 MiB default body limit while
  // apps/api and Caddy take 20 MiB: a 3 MB file came back `upload_failed`
  // through the page, and was accepted posted straight to the API (#243).
  test("a 3 MB file goes through the web page to the API", async ({ page }) => {
    await registerAndLogin(page, "e2e-agbig", "Big File User");
    await createGroup(page, "Famille Volumes");
    const date = dayThisMonth(21);
    await createEvent(page, {
      title: "Événement volumineux",
      start: `${date}T09:00`,
      end: `${date}T10:00`,
    });
    await openEventDetail(page, "Événement volumineux");

    // A PNG signature is all apps/api sniffs; the rest is padding.
    const big = Buffer.alloc(3 * 1024 * 1024);
    PNG_1X1.copy(big);
    await page.locator('input[name="file"]').setInputFiles({
      name: "gros.png",
      mimeType: "image/png",
      buffer: big,
    });
    await page.getByRole("button", { name: "Envoyer" }).click();
    await expect(page.getByText("Pièce jointe ajoutée.")).toBeVisible();
    await expect(page.getByRole("link", { name: "gros.png" })).toBeVisible();
  });
});

/** One PNG chunk: length, type, data, CRC of type and data. */
function pngChunk(type: string, data: Buffer): Buffer {
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const typed = Buffer.concat([Buffer.from(type, "latin1"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(typed));
  return Buffer.concat([length, typed, crc]);
}

/** An RGB PNG of `width` × `height`, every pixel `rgb`: one apps/api decodes into a thumbnail. */
function png(width: number, height: number, rgb: [number, number, number]): Buffer {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8; // bit depth
  header[9] = 2; // RGB
  const row = Buffer.alloc(1 + width * 3);
  for (let x = 0; x < width; x++) row.set(rgb, 1 + x * 3);
  const rows = Buffer.concat(Array.from({ length: height }, () => row));
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    pngChunk("IHDR", header),
    pngChunk("IDAT", deflateSync(rows)),
    pngChunk("IEND", Buffer.alloc(0)),
  ]);
}

/** A PDF as apps/api sniffs one: its signature. */
const PDF = Buffer.from("%PDF-1.4\n1 0 obj << >> endobj\ntrailer << >>\n%%EOF\n", "latin1");

/** `#rrggbb` as the `rgb(r, g, b)` a computed colour reads. */
function rgbOf(hex: string): string {
  const n = parseInt(hex.trim().slice(1), 16);
  return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
}

async function attach(page: Page, name: string, mimeType: string, buffer: Buffer): Promise<void> {
  await page.locator('input[name="file"]').setInputFiles({ name, mimeType, buffer });
  await page.getByRole("button", { name: "Envoyer" }).click();
  await expect(page.getByText("Pièce jointe ajoutée.")).toBeVisible();
}

/** A fresh user's event carrying two pictures and a PDF, its detail open; returns its path. */
async function eventWithFiles(page: Page, prefix: string, family: string): Promise<string> {
  await registerAndLogin(page, prefix, "Galerie Pièces");
  await createGroup(page, family);
  const date = dayThisMonth(22);
  await createEvent(page, { title: "Sortie au musée", start: `${date}T09:00`, end: `${date}T10:00` });
  await openEventDetail(page, "Sortie au musée");
  await attach(page, "rouge.png", "image/png", png(40, 30, [200, 40, 40]));
  await attach(page, "bleu.png", "image/png", png(30, 40, [40, 40, 200]));
  await attach(page, "notice.pdf", "application/pdf", PDF);
  return new URL(page.url()).pathname;
}

// #776: the attachments as a gallery of tiles, each opening a viewer.
test.describe("Agenda — galerie des pièces jointes (#776)", () => {
  test("a tile opens its viewer, which steps, downloads, closes on Escape and deletes", async ({ page }) => {
    await eventWithFiles(page, "e2e-aggal", "Famille Galerie");
    await expect(page.locator(".gallery a")).toHaveCount(3);
    const red = page.getByRole("link", { name: "rouge.png" });
    await expect(red.locator("img")).toHaveAttribute("src", /\/thumb$/);
    await expect(red.locator("img")).toHaveAttribute("loading", "lazy");
    await expect(page.getByRole("link", { name: "notice.pdf" })).toContainText("PDF");

    await red.click();
    const first = page.getByRole("dialog", { name: "rouge.png" });
    await expect(first).toBeVisible();
    await expect(first.getByText("1 sur 3")).toBeVisible();
    // The whole file, loaded once the viewer opened.
    const full = first.locator("img");
    await expect(full).toHaveAttribute("src", /\/file$/);
    await expect.poll(() => full.evaluate((img: HTMLImageElement) => img.naturalWidth)).toBe(40);
    await expect(first.getByRole("button", { name: "Pièce précédente" })).toBeDisabled();

    // → and « Pièce suivante » step forward, ← back; the last has no next.
    await page.keyboard.press("ArrowRight");
    const second = page.getByRole("dialog", { name: "bleu.png" });
    await expect(second.getByText("2 sur 3")).toBeVisible();
    await expect(first).toBeHidden();
    await second.getByRole("button", { name: "Pièce suivante" }).click();
    const third = page.getByRole("dialog", { name: "notice.pdf" });
    await expect(third.getByText("3 sur 3")).toBeVisible();
    await expect(third.getByRole("link", { name: "Ouvrir le PDF" })).toHaveAttribute("target", "_blank");
    await expect(third.getByRole("button", { name: "Pièce suivante" })).toBeDisabled();
    await page.keyboard.press("ArrowLeft");
    await expect(second).toBeVisible();

    // « Télécharger » is a download, under the file's name.
    const [download] = await Promise.all([
      page.waitForEvent("download"),
      second.getByRole("link", { name: "Télécharger" }).click(),
    ]);
    expect(download.suggestedFilename()).toBe("bleu.png");
    await expect(second.getByRole("link", { name: "Ouvrir dans un onglet" })).toHaveAttribute("target", "_blank");

    // Escape closes it; the focus goes back to the tile of the file shown.
    await page.keyboard.press("Escape");
    await expect(second).toBeHidden();
    await expect(page.getByRole("link", { name: "bleu.png" })).toBeFocused();

    // « Supprimer » asks first: refused, nothing goes; accepted, it goes.
    await page.getByRole("link", { name: "bleu.png" }).click();
    page.once("dialog", (d) => d.dismiss());
    await second.getByRole("button", { name: "Supprimer" }).click();
    await expect(second).toBeVisible();
    let asked = "";
    page.once("dialog", (d) => {
      asked = d.message();
      void d.accept();
    });
    await second.getByRole("button", { name: "Supprimer" }).click();
    await expect(page.getByText("Pièce jointe supprimée.")).toBeVisible();
    expect(asked).toBe("Supprimer « bleu.png » ?");
    await expect(page.getByRole("link", { name: "bleu.png" })).toHaveCount(0);
    await expect(page.locator(".gallery a")).toHaveCount(2);
  });

  test("the gallery and its viewer at 390 and 1440 px, light and dark", async ({ page }) => {
    const detail = await eventWithFiles(page, "e2e-aggalvp", "Famille Galerie Écrans");
    for (const [theme, dataTheme] of [
      ["Clair", "light"],
      ["Sombre", "dark"],
    ] as const) {
      await chooseTheme(page, theme);
      for (const [width, height, columns] of [
        [390, 844, 3],
        [1440, 900, 4],
      ] as const) {
        const at = `${theme} ${width}px`;
        await page.setViewportSize({ width, height });
        await page.goto(detail);
        expect((await resolvedTheme(page)).dataTheme, at).toBe(dataTheme);
        const grid = await page
          .locator(".gallery")
          .evaluate((g) => getComputedStyle(g).gridTemplateColumns.split(" ").length);
        expect(grid, at).toBe(columns);
        const tile = await page.getByRole("link", { name: "rouge.png" }).boundingBox();
        expect(tile!.height, at).toBeGreaterThanOrEqual(44);

        await page.getByRole("link", { name: "rouge.png" }).click();
        const viewer = page.getByRole("dialog", { name: "rouge.png" });
        await expect(viewer).toBeVisible();
        const box = await viewer.boundingBox();
        expect(box!.x, at).toBeGreaterThanOrEqual(0);
        expect(box!.x + box!.width, at).toBeLessThanOrEqual(width);
        // Every control of the viewer is a 44 px target.
        for (const control of await viewer.locator("button, a").all()) {
          const b = await control.boundingBox();
          expect(b!.height, `${at} ${await control.textContent()}`).toBeGreaterThanOrEqual(44);
        }
        // The window's card, on the theme's own tokens.
        const [ground, text, surface, fg] = await viewer.evaluate((d) => {
          const root = getComputedStyle(document.documentElement);
          return [
            getComputedStyle(d).backgroundColor,
            getComputedStyle(d).color,
            root.getPropertyValue("--surface"),
            root.getPropertyValue("--fg"),
          ];
        });
        expect(ground, at).toBe(rgbOf(surface));
        expect(text, at).toBe(rgbOf(fg));
        await page.keyboard.press("Escape");
        await expect(viewer).toBeHidden();
      }
    }
  });
});

test.describe("Agenda — galerie sans JavaScript (#776)", () => {
  test.use({ javaScriptEnabled: false });

  test("a tile leads to the file, and the list under the gallery deletes", async ({ page }) => {
    await eventWithFiles(page, "e2e-aggalnojs", "Famille Galerie Sans Script");
    const red = page.getByRole("link", { name: "rouge.png" });
    const href = await red.getAttribute("href");
    expect(href).toMatch(/\/file$/);
    await red.click();
    await expect(page).toHaveURL(/\/attachments\/[0-9a-f-]+\/file$/);
    const shown = await page.request.get(href!);
    expect(shown.status()).toBe(200);
    expect(shown.headers()["content-type"]).toBe("image/png");

    await page.goBack();
    // No script, no confirmation: the form posts as it is.
    await page.locator("li", { hasText: "notice.pdf" }).getByRole("button", { name: "Supprimer" }).click();
    await expect(page.getByText("Pièce jointe supprimée.")).toBeVisible();
    await expect(page.getByRole("link", { name: "notice.pdf" })).toHaveCount(0);
  });
});

// The two write forms of this epic (`/agenda/new`, `/agenda/:id/edit`) read
// their body as raw bytes rather than through `axum::Form`, because a
// checkbox group submits the same `assignee_ids` key several times and
// `Form`'s deserializer cannot express that. Taking the raw body dropped two
// things `Form` did for free, both flagged by #98's round-2 verification:
// the media-type check (a `text/plain` POST created an event where the 31
// other handlers in this folder answer 415), and reporting a body that does
// not parse (it answered 200 with an unrelated validation message).
test.describe("Agenda — type de média et corps illisible", () => {
  test("un POST hors formulaire est refusé, un corps incomplet est un 422", async ({ page }) => {
    await registerAndLogin(page, "e2e-agmedia", "Media User");
    await createGroup(page, "Famille Media");

    const date = dayThisMonth(12);
    const body = `title=Injecte&starts_at=${date}T10:00&ends_at=${date}T11:00`;

    const plain = await page.request.post("/agenda/new", {
      headers: { "content-type": "text/plain" },
      data: body,
    });
    expect(plain.status()).toBe(415);

    const json = await page.request.post("/agenda/new", {
      headers: { "content-type": "application/json" },
      data: body,
    });
    expect(json.status()).toBe(415);

    // Right media type, body missing the required dates: the answer says the
    // form was unreadable instead of claiming the end precedes the start.
    const incomplete = await page.request.post("/agenda/new", {
      headers: { "content-type": "application/x-www-form-urlencoded" },
      data: "title=Sans+dates",
    });
    expect(incomplete.status()).toBe(422);
    expect(await incomplete.text()).toContain("Formulaire incomplet");

    // None of the three created anything.
    await page.goto("/agenda");
    await expect(page.getByRole("link", { name: /Injecte/ })).toHaveCount(0);
    await expect(page.getByRole("link", { name: /Sans dates/ })).toHaveCount(0);
  });

  // The sheet is multipart since it carries a file (#525): the route takes
  // 20 MiB for the file, but its text fields together keep the plain
  // form's 2 MiB.
  test("des champs texte multipart au-delà de 2 Mio sont un 413", async ({ page }) => {
    await registerAndLogin(page, "e2e-agmpbig", "Multipart User");
    await createGroup(page, "Famille Multipart");

    const date = dayThisMonth(13);
    const fields = {
      title: "Trop long",
      starts_at: `${date}T10:00`,
      ends_at: `${date}T11:00`,
    };
    const tooLong = await page.request.post("/agenda/new", {
      multipart: { ...fields, description: "a".repeat(2 * 1024 * 1024 + 1) },
    });
    expect(tooLong.status()).toBe(413);

    await page.goto("/agenda");
    await expect(page.getByRole("link", { name: /Trop long/ })).toHaveCount(0);
  });
});

// #117. An all-day event is stored with an exclusive end — the Paris
// midnight opening the day after its last one (#101). The edit form used to
// show that midnight in a `datetime-local` field: an event covering the 5th
// to the 7th read "Fin : 08 00:00", and a user "correcting" it to 07 00:00
// lost the 7th without a word. "Journée entière" now turns Début/Fin into
// two date fields, Fin being the last day covered; the web layer adds the
// day back on submit. Stored bounds are read in Postgres: the date fields
// can no longer show them.
test.describe("Agenda — journée entière", () => {
  test("un événement de plusieurs jours se rouvre sur son dernier jour et s'enregistre sans en perdre un", async ({
    page,
  }) => {
    const email = await registerAndLogin(page, "e2e-agallday", "AllDay User");
    await createGroup(page, "Famille Vacances");
    const [first, last, dayAfter] = [dayThisMonth(5), dayThisMonth(7), dayThisMonth(8)];
    const stored = { starts: `${first}T00:00`, ends: `${dayAfter}T00:00` };

    await page.goto("/agenda/new");
    await page.getByLabel("Titre").fill("Séjour au ski");
    await page.locator('input[name="all_day"]').check();
    await expect(page.getByLabel("Début")).toHaveAttribute("type", "date");
    await expect(page.getByLabel("Fin")).toHaveAttribute("type", "date");
    await page.getByLabel("Début").fill(first);
    await page.getByLabel("Fin").fill(last);
    await page.getByRole("button", { name: "Créer l'événement" }).click();
    await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);
    expect(await fetchEventBounds(email, "Séjour au ski")).toEqual(stored);

    // The edit form shows the 7th, not the exclusive 8th.
    await openEventDetail(page, "Séjour au ski");
    await page.getByRole("link", { name: "Modifier" }).click();
    await expect(page.locator('input[name="all_day"]')).toBeChecked();
    await expect(page.getByLabel("Début")).toHaveAttribute("type", "date");
    await expect(page.getByLabel("Début")).toHaveValue(first);
    await expect(page.getByLabel("Fin")).toHaveValue(last);

    // A correction that leaves the dates alone keeps every day.
    await page.getByLabel("Lieu").fill("Chamonix");
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page.getByText("Événement mis à jour.")).toBeVisible();
    expect(await fetchEventBounds(email, "Séjour au ski")).toEqual(stored);

    await page.getByRole("link", { name: "Modifier" }).click();
    await expect(page.getByLabel("Début")).toHaveValue(first);
    await expect(page.getByLabel("Fin")).toHaveValue(last);
  });

  test("cocher la case ramène un créneau à ses journées, décocher puis recocher ne déplace aucune borne", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-agtoggle", "Toggle User");
    await createGroup(page, "Famille Bascule");
    const [first, last, dayAfter] = [dayThisMonth(5), dayThisMonth(7), dayThisMonth(8)];
    const box = page.locator('input[name="all_day"]');
    const start = page.getByLabel("Début");
    const end = page.getByLabel("Fin");

    await page.goto("/agenda/new");
    await start.fill(`${first}T10:00`);
    // An end on midnight is the exclusive bound: the last day is the 7th.
    await end.fill(`${dayAfter}T00:00`);

    await box.check();
    await expect(start).toHaveAttribute("type", "date");
    await expect(start).toHaveValue(first);
    await expect(end).toHaveValue(last);

    // Unticking gives back the instants the dates stand for.
    await box.uncheck();
    await expect(start).toHaveAttribute("type", "datetime-local");
    await expect(start).toHaveValue(`${first}T00:00`);
    await expect(end).toHaveValue(`${dayAfter}T00:00`);

    // And ticking again does not eat a day.
    await box.check();
    await expect(start).toHaveValue(first);
    await expect(end).toHaveValue(last);
  });

  test("sans JS : dates lues fin incluse, créneau coché normalisé, dates inversées refusées", async ({
    page,
  }) => {
    const email = await registerAndLogin(page, "e2e-agnojs", "NoJS User");
    await createGroup(page, "Famille Formulaire");
    const [d5, d6, d7, d8] = [5, 6, 7, 8].map(dayThisMonth);
    const post = (body: string) =>
      page.request.post("/agenda/new", {
        headers: { "content-type": "application/x-www-form-urlencoded" },
        data: body,
        maxRedirects: 0,
      });

    // Date fields, whatever rendered them: the end is the last day covered.
    const dates = await post(`title=Dates+seules&all_day=on&starts_at=${d5}&ends_at=${d7}`);
    expect(dates.status()).toBe(303);
    expect(await fetchEventBounds(email, "Dates seules")).toEqual({
      starts: `${d5}T00:00`,
      ends: `${d8}T00:00`,
    });

    // The box ticked without JS leaves the fields timed; the API still
    // normalizes onto the whole Paris day (#101).
    const timed = await post(
      `title=Creneau+coche&all_day=on&starts_at=${d5}T08:00&ends_at=${d5}T09:00`,
    );
    expect(timed.status()).toBe(303);
    expect(await fetchEventBounds(email, "Creneau coche")).toEqual({
      starts: `${d5}T00:00`,
      ends: `${d6}T00:00`,
    });

    // Fin the day before Début would convert to an end equal to the start,
    // which the shared validation accepts: it is refused, and the form comes
    // back in date mode with what was typed.
    const reversed = await post(
      `title=Dates+inversees&all_day=on&starts_at=${d7}&ends_at=${d6}`,
    );
    expect(reversed.status()).toBe(200);
    const html = await reversed.text();
    expect(html).toContain("La fin doit être après le début.");
    expect(html).toContain(`type="date" name="ends_at" value="${d6}"`);
    await expect(fetchEventBounds(email, "Dates inversees")).rejects.toThrow();
  });

  // #697: with JS, Fin follows Début — its `min` is the start, and a start
  // moved past the end takes the end along. An end still after the start is
  // left alone. Without JS nothing changes (the test above).
  test("la fin suit le début : min posé, fin dépassée ramenée au début, en heures comme en jours", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-agfollow", "Follow User");
    await createGroup(page, "Famille Suivi");
    const [d12, d14, d15] = [12, 14, 15].map(dayThisMonth);
    const box = page.locator('input[name="all_day"]');
    const start = page.getByLabel("Début");
    const end = page.getByLabel("Fin");

    await page.goto("/agenda/new");
    await start.fill(`${d12}T09:00`);
    await end.fill(`${d12}T10:00`);
    await expect(end).toHaveAttribute("min", `${d12}T09:00`);

    // The start passes the end: the end moves to the start.
    await start.fill(`${d12}T18:00`);
    await expect(end).toHaveValue(`${d12}T18:00`);
    await expect(end).toHaveAttribute("min", `${d12}T18:00`);

    // The end is still after the start: it stays where it is.
    await end.fill(`${d14}T12:00`);
    await start.fill(`${d12}T08:00`);
    await expect(end).toHaveValue(`${d14}T12:00`);
    await expect(end).toHaveAttribute("min", `${d12}T08:00`);

    // "Journée entière": the min takes the date shape, and still follows.
    await box.check();
    await expect(end).toHaveAttribute("type", "date");
    await expect(end).toHaveAttribute("min", d12);
    await start.fill(d15);
    await expect(end).toHaveValue(d15);
    await expect(end).toHaveAttribute("min", d15);

    // And back to timed slots.
    await box.uncheck();
    await expect(end).toHaveAttribute("type", "datetime-local");
    await expect(end).toHaveAttribute("min", `${d15}T00:00`);
    await page.getByLabel("Titre").fill("Fin suivie");
    await page.getByRole("button", { name: "Créer l'événement" }).click();
    await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);
  });
});

// #525 — the head of the agenda, from the mock-ups: ‹ · « Aujourd'hui » · ›
// stepping by the view on screen, and the Semaine / Mois toggle, on a phone
// as on a PC. The arrows keep their accessible names.
const AGENDA_LAYOUTS = [
  ["téléphone", { width: 390, height: 844 }],
  ["PC", { width: 1440, height: 900 }],
] as const;

const VIEW_ARROWS = [
  ["day", "Jour précédent", "Jour suivant"],
  ["month", "Mois précédent", "Mois suivant"],
  ["week", "Semaine précédente", "Semaine suivante"],
] as const;

for (const [layout, viewport] of AGENDA_LAYOUTS) {
  test.describe(`Agenda — en-tête, ${layout} (#525)`, () => {
    test.use({ viewport });

    test("› then ‹ come back to the same period, and « Aujourd'hui » to today's", async ({ page }) => {
      await registerAndLogin(page, "e2e-agnav", "Agenda Nav");
      await createGroup(page, "Famille Navigation");
      const heading = page.getByRole("heading", { level: 1 });
      const today = parisDay();

      for (const [view, previous, next] of VIEW_ARROWS) {
        await page.goto(`/agenda?view=${view}`);
        const current = (await heading.innerText()).trim();

        await page.getByRole("link", { name: next, exact: true }).click();
        await expect(heading).not.toHaveText(current);
        await page.getByRole("link", { name: previous, exact: true }).click();
        await expect(heading).toHaveText(current);

        // Two steps away, then back in one.
        await page.getByRole("link", { name: previous, exact: true }).click();
        await page.getByRole("link", { name: previous, exact: true }).click();
        await expect(heading).not.toHaveText(current);
        await page.getByRole("link", { name: "Aujourd'hui", exact: true }).click();
        await expect(page).toHaveURL(`/agenda?view=${view}&date=${today}`);
        await expect(heading).toHaveText(current);
      }
    });

    test("Semaine and Mois switch the view, and say which one is on screen", async ({ page }) => {
      await registerAndLogin(page, "e2e-agview", "Agenda Vue");
      await createGroup(page, "Famille Affichage");
      await page.goto("/agenda");
      const toggle = page.getByRole("navigation", { name: "Affichage" });
      await expect(toggle).toBeVisible();
      const month = toggle.getByRole("link", { name: "Mois", exact: true });
      const week = toggle.getByRole("link", { name: "Semaine", exact: true });
      await expect(month).toHaveAttribute("aria-current", "true");
      await expect(week).not.toHaveAttribute("aria-current");

      await week.click();
      await expect(page).toHaveURL(/\/agenda\?view=week&date=/);
      await expect(page.getByRole("heading", { level: 1 })).toHaveText(/^Semaine du /);
      await expect(week).toHaveAttribute("aria-current", "true");
      await expect(page.getByRole("link", { name: "Semaine précédente", exact: true })).toBeVisible();

      await month.click();
      await expect(page).toHaveURL(/\/agenda\?view=month&date=/);
      await expect(month).toHaveAttribute("aria-current", "true");
    });
  });
}

test.describe("Agenda — mise en page (#525)", () => {
  test("a phone gets the floating + and no day panel", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await registerAndLogin(page, "e2e-agphone", "Agenda Phone");
    await createGroup(page, "Famille Téléphone");
    await page.goto("/agenda");
    await expect(page.getByRole("link", { name: "Ajouter un événement" })).toBeVisible();
    await expect(page.getByRole("link", { name: "Nouvel événement" })).toBeHidden();
    await expect(page.getByText("Rien de prévu ce jour-là.")).toBeHidden();
  });

  test("a PC gets the day panel, named for the day in focus", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await registerAndLogin(page, "e2e-agpanel", "Agenda Panneau");
    await createGroup(page, "Famille Panneau");
    const date = dayThisMonth(16);
    await createEvent(page, { title: "Cours de judo", start: `${date}T17:00`, end: `${date}T18:00` });

    await page.goto(`/agenda?view=month&date=${date}`);
    const [y, m, d] = date.split("-").map(Number);
    const long = new Intl.DateTimeFormat("fr-FR", {
      weekday: "long",
      day: "numeric",
      month: "long",
      timeZone: "UTC",
    }).format(new Date(Date.UTC(y, m - 1, d)));
    const panel = page.locator("section.card").filter({
      has: page.getByRole("heading", { level: 2, name: new RegExp(`^${long}$`, "i") }),
    });
    await expect(panel).toBeVisible();
    await expect(panel.getByText("Cours de judo")).toBeVisible();
    await expect(panel.getByText("17:00")).toBeVisible();
    // The chip in the grid stays the one link to the event.
    await expect(page.getByRole("link", { name: /Cours de judo/ })).toHaveCount(1);
    await expect(page.getByRole("link", { name: "Ajouter un événement" })).toBeHidden();
  });
});

// #675 — a phone's list shows only the days that carry something, so an
// empty month showed no date at all. A mini grid of the period now heads it.
test.describe("Agenda — mini-grille sur téléphone (#675)", () => {
  test("an empty month shows its days, today marked, and says nothing is planned", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await registerAndLogin(page, "e2e-agmini", "Agenda Mini");
    await createGroup(page, "Famille Grille");
    await page.goto("/agenda");

    const mini = page.locator("table.mini");
    await expect(mini).toBeVisible();
    const cells = mini.locator("td");
    const count = await cells.count();
    expect(count).toBeGreaterThanOrEqual(28);
    expect(count).toBeLessThanOrEqual(42);
    // A day number, then — on a national day (#565), a link — its name in
    // words: « 11 novembre, Armistice 1918 ».
    for (const text of await cells.allInnerTexts()) expect(text.trim()).toMatch(/^\d{1,2}(\s|$)/);

    const today = mini.locator('td[aria-current="date"]');
    await expect(today).toHaveCount(1);
    await expect(today).toHaveClass(/\bcurrent\b/);
    await expect(today).toHaveText(new RegExp(`^${Number(parisDay().slice(8))}(\\s|$)`));

    // A national day written out puts its day in the list (#565): the
    // empty state is read on a month and a week that have none — February
    // 2027's grid runs from 1 February to 14 March.
    await page.goto("/agenda?view=month&date=2027-02-10");
    await expect(page.getByText("Rien de prévu ce mois-ci.")).toBeVisible();
    await page.goto("/agenda?view=week&date=2027-02-10");
    await expect(mini.locator("td")).toHaveCount(7);
    await expect(page.getByText("Rien de prévu cette semaine.")).toBeVisible();
  });

  test("a day with an event says so, and leads to its day view (#793)", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await registerAndLogin(page, "e2e-agminilink", "Agenda Lien");
    await createGroup(page, "Famille Ancre");
    const date = dayThisMonth(15);
    await createEvent(page, { title: "Réunion parents", start: `${date}T10:00`, end: `${date}T11:00` });

    await page.goto(`/agenda?view=month&date=${date}`);
    // The 15th may also be a national day (15 August, #565): its name
    // comes before the count.
    const link = page
      .locator("table.mini")
      .getByRole("link", { name: /^15 \S+, (.+, )?1 événement$/ });
    await expect(link).toHaveAttribute("href", `/agenda?view=day&date=${date}`);
    await expect(page.getByText("Rien de prévu ce mois-ci.")).toBeHidden();
    await link.click();
    await expect(page).toHaveURL(`/agenda?view=day&date=${date}`);
    await expect(page.locator("a.blk", { hasText: "Réunion parents" })).toBeVisible();
    // A day with nothing leads there too.
    await page.goto(`/agenda?view=month&date=${date}`);
    const other = dayThisMonth(16);
    await page.locator("table.mini").getByRole("link", { name: "16", exact: true }).click();
    await expect(page).toHaveURL(`/agenda?view=day&date=${other}`);
  });

  test("no horizontal scroll at 360px", async ({ page }) => {
    await page.setViewportSize({ width: 360, height: 780 });
    await registerAndLogin(page, "e2e-agmini360", "Agenda Étroit");
    await createGroup(page, "Famille Étroite");
    for (const path of ["/agenda", "/agenda?view=week"]) {
      await page.goto(path);
      await expect(page.locator("table.mini")).toBeVisible();
      const overflow = await page.evaluate(
        () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
      );
      expect(overflow, path).toBeLessThanOrEqual(0);
    }
  });

  test("a PC keeps its grid and gets neither the mini grid nor the empty state", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    await registerAndLogin(page, "e2e-agminipc", "Agenda Bureau");
    await createGroup(page, "Famille Bureau");
    await page.goto("/agenda");
    await expect(page.locator("table.cal")).toBeVisible();
    await expect(page.locator("table.mini")).toBeHidden();
    await expect(page.getByText("Rien de prévu ce mois-ci.")).toBeHidden();
  });
});

// #792 — a day of a phone's list says its weekday, « jeudi 16 »; a PC's
// grid, whose column headers already do, keeps the number alone, and its
// screen reader does not hear the weekday twice.
test.describe("Agenda — jour de la semaine dans la liste (#792)", () => {
  test("a phone's list writes it out, a PC's grid neither draws nor reads it", async ({ page }) => {
    await registerAndLogin(page, "e2e-agweekday", "Agenda Semaine");
    await createGroup(page, "Famille Semaine");
    const date = dayThisMonth(16);
    await createEvent(page, { title: "Cours de piano", start: `${date}T17:00`, end: `${date}T18:00` });
    const [y, m, d] = date.split("-").map(Number);
    const weekday = new Intl.DateTimeFormat("fr-FR", { weekday: "long", timeZone: "UTC" }).format(
      new Date(Date.UTC(y, m - 1, d)),
    );
    // The cell is found by its number's link to its day view (#793).
    const number = page
      .locator("table.cal .cal-day")
      .filter({ has: page.locator(`a[href="/agenda?view=day&date=${date}"]`) });

    for (const [layout, viewport] of AGENDA_LAYOUTS) {
      await page.setViewportSize(viewport);
      for (const view of ["month", "week"]) {
        await page.goto(`/agenda?view=${view}&date=${date}`);
        await expect(number, `${layout} ${view}`).toBeVisible();
        const snapshot = await number.ariaSnapshot();
        if (layout === "téléphone") {
          await expect(number, `${layout} ${view}`).toHaveText(`${weekday} ${d}`, { useInnerText: true });
          expect(snapshot, `${layout} ${view}`).toContain(`${weekday} ${d}`);
        } else {
          await expect(number, `${layout} ${view}`).toHaveText(`${d}`, { useInnerText: true });
          expect(snapshot, `${layout} ${view}`).not.toContain(weekday);
        }
      }
    }
  });
});

// #793 — one day hour by hour: the « Journée » banner, then the hours, a
// block per event placed at its hour and as tall as it lasts. Read with
// JavaScript off, at both layouts: the page and its opening on 08:00 owe
// nothing to a script. A fixed day, far from today and from any national
// day: 10 March 2027 is a Wednesday.
const DAY = "2027-03-10";
const NEXT = "2027-03-11";

/** Posts the event form as a browser without JavaScript would. */
async function postEvent(page: Page, body: string): Promise<void> {
  const res = await page.request.post("/agenda/new", {
    headers: { "content-type": "application/x-www-form-urlencoded" },
    data: body,
    maxRedirects: 0,
  });
  expect(res.status(), body).toBe(303);
}

/** « mercredi 10 mars 2027 »: the day view's heading for `date`. */
function longDate(date: string): string {
  const [y, m, d] = date.split("-").map(Number);
  return new Intl.DateTimeFormat("fr-FR", {
    weekday: "long",
    day: "numeric",
    month: "long",
    year: "numeric",
    timeZone: "UTC",
  }).format(new Date(Date.UTC(y, m - 1, d)));
}

for (const [layout, viewport] of AGENDA_LAYOUTS) {
  test.describe(`Agenda — vue jour, ${layout}, sans JavaScript (#793)`, () => {
    test.use({ viewport, javaScriptEnabled: false });

    test("the banner, the hours side by side, cut at midnight, open on 08:00", async ({ page }) => {
      await registerAndLogin(page, "e2e-agday", "Agenda Jour");
      await createGroup(page, "Famille Journée");
      await postEvent(page, `title=R%C3%A9union+%C3%A9cole&starts_at=${DAY}T10:00&ends_at=${DAY}T11:30`);
      await postEvent(page, `title=Appel+plombier&starts_at=${DAY}T10:30&ends_at=${DAY}T11:00`);
      await postEvent(page, `title=Soir%C3%A9e+tardive&starts_at=${DAY}T23:00&ends_at=${NEXT}T01:00`);
      await postEvent(page, `title=Voyage+scolaire&all_day=on&starts_at=2027-03-09&ends_at=${NEXT}`);

      await page.goto(`/agenda?view=day&date=${DAY}`);
      await expect(page.getByRole("heading", { level: 1 })).toHaveText(longDate(DAY));

      // The banner: the trip of three days, on its second.
      const banner = page.locator(".allday");
      await expect(banner.getByRole("heading", { name: "Journée" })).toBeVisible();
      const trip = banner.getByRole("link", { name: /Voyage scolaire/ });
      await expect(trip).toBeVisible();
      await expect(trip).toContainText("jour 2 sur 3");

      // 08:00 at the top of the hours, the night a scroll away.
      const hours = page.locator(".hours-scroll");
      const opening = await hours.evaluate((el) => {
        const eight = el.querySelector("#h8") as HTMLElement;
        return {
          scrolled: el.scrollTop,
          offset: eight.getBoundingClientRect().top - el.getBoundingClientRect().top,
        };
      });
      expect(opening.scrolled).toBeGreaterThan(0);
      expect(Math.abs(opening.offset)).toBeLessThanOrEqual(1);
      await expect(hours.locator("#h8")).toBeInViewport();

      // Two blocks that overlap sit side by side, neither hiding the other.
      const meeting = page.locator("a.blk", { hasText: "Réunion école" });
      const plumber = page.locator("a.blk", { hasText: "Appel plombier" });
      await expect(meeting).toContainText("10:00–11:30");
      await expect(plumber).toBeVisible();
      const [a, b] = [await meeting.boundingBox(), await plumber.boundingBox()];
      if (!a || !b) throw new Error("the blocks have no box");
      expect(b.y).toBeGreaterThan(a.y);
      expect(b.y).toBeLessThan(a.y + a.height);
      expect(b.x).toBeGreaterThanOrEqual(a.x + a.width - 1);
      // The longer is about three times the shorter: 1 h 30 against 30 min.
      expect(a.height / b.height).toBeGreaterThan(2.5);

      // The evening event, cut at midnight, says its real hours.
      const evening = page.locator("a.blk", { hasText: "Soirée tardive" });
      await expect(evening).toContainText("23:00–01:00");
      await expect(evening).toHaveClass(/\bcut-e\b/);
      await expect(page.getByText("Rien de prévu à l'heure ce jour-là.")).toHaveCount(0);

      // The next day: the end of the evening, cut at its start; the trip's
      // last day.
      await page.getByRole("link", { name: "Jour suivant", exact: true }).click();
      await expect(page).toHaveURL(`/agenda?view=day&date=${NEXT}`);
      const after = page.locator("a.blk", { hasText: "Soirée tardive" });
      await expect(after).toHaveClass(/\bcut-s\b/);
      await expect(after).toContainText("23:00–01:00");
      await expect(page.locator(".allday").getByRole("link", { name: /Voyage scolaire/ })).toContainText(
        "jour 3 sur 3",
      );

      // The day after: nothing at all.
      await page.goto("/agenda?view=day&date=2027-03-12");
      await expect(page.locator(".allday")).toHaveCount(0);
      await expect(page.getByText("Rien de prévu à l'heure ce jour-là.")).toBeVisible();

      // No date, or one that cannot be read: today, as the other views do.
      for (const path of ["/agenda?view=day", "/agenda?view=day&date=demain"]) {
        await page.goto(path);
        await expect(page.getByRole("heading", { level: 1 }), path).toHaveText(longDate(parisDay()));
      }
    });

    test("a day's number, « +N autres » and the toggle lead to the day, which keeps its date", async ({
      page,
    }) => {
      await registerAndLogin(page, "e2e-agdaynav", "Agenda Jour Nav");
      await createGroup(page, "Famille Accès");
      for (const hour of [8, 10, 12, 14]) {
        const h = String(hour).padStart(2, "0");
        await postEvent(page, `title=Rendez-vous+${hour}h&starts_at=${DAY}T${h}:00&ends_at=${DAY}T${h}:30`);
      }

      // The day's number, in the grid on a PC, in the mini grid on a phone.
      // A month grid shows a 10 of two months: the grid's is found by its
      // link.
      await page.goto(`/agenda?view=month&date=${DAY}`);
      const dayLink = page.locator(`table.cal .cal-day a[href="/agenda?view=day&date=${DAY}"]`);
      const number =
        layout === "PC" ? dayLink : page.locator("table.mini").getByRole("link", { name: /^10 mars, / });
      await number.click();
      await expect(page).toHaveURL(`/agenda?view=day&date=${DAY}`);
      for (const hour of [8, 10, 12, 14]) {
        await expect(page.locator("a.blk", { hasText: `Rendez-vous ${hour}h` })).toHaveCount(1);
      }

      // The toggle keeps the day, and says which view is on screen.
      const toggle = page.getByRole("navigation", { name: "Affichage" });
      await expect(toggle.getByRole("link", { name: "Jour", exact: true })).toHaveAttribute("aria-current", "true");
      await toggle.getByRole("link", { name: "Semaine", exact: true }).click();
      await expect(page).toHaveURL(`/agenda?view=week&date=${DAY}`);
      // In the week, the day's number leads to it too.
      await expect(dayLink).toHaveText(layout === "PC" ? "10" : "mercredi 10", { useInnerText: true });
      await dayLink.click();
      await expect(page).toHaveURL(`/agenda?view=day&date=${DAY}`);
      await toggle.getByRole("link", { name: "Mois", exact: true }).click();
      await expect(page).toHaveURL(`/agenda?view=month&date=${DAY}`);

      // A crowded day of the month counts the rest into « +N autres »,
      // which leads to the day view of that day.
      await page.locator("table.cal").getByRole("link", { name: "+2 autres" }).click();
      await expect(page).toHaveURL(`/agenda?view=day&date=${DAY}`);
    });
  });
}

test.describe("Agenda — feuille de création sans JavaScript (#525)", () => {
  test.use({ javaScriptEnabled: false });

  test("the sheet creates an event with JavaScript off, and its cross goes back", async ({ page }) => {
    await registerAndLogin(page, "e2e-agnojs", "Agenda Sans JS");
    await createGroup(page, "Famille Sans Script");
    await page.goto("/agenda/new");
    await expect(page.getByRole("heading", { level: 1, name: "Nouvel événement" })).toBeVisible();
    await expect(page.locator(".sheet .eyebrow")).toHaveText("Famille Sans Script");

    const date = dayThisMonth(17);
    await page.getByLabel("Titre").fill("Atelier sans script");
    await page.getByLabel("Début").fill(`${date}T09:00`);
    await page.getByLabel("Fin").fill(`${date}T10:00`);
    await page.getByRole("checkbox", { name: "Agenda Sans JS" }).check();
    await page.getByRole("button", { name: "Créer l'événement" }).click();
    await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);
    await expect(page.getByText("Événement créé.")).toBeVisible();
    await page.goto(`/agenda?view=month&date=${date}`);
    await expect(page.getByRole("link", { name: /Atelier sans script/ })).toBeVisible();

    await page.goto("/agenda/new");
    await page.getByRole("link", { name: "Fermer" }).click();
    await expect(page).toHaveURL("/agenda");
  });

  // The sheet carries the attachment the mock-up draws: sent as multipart,
  // the file goes to the event once it exists.
  test("the sheet creates an event with its attachment, JavaScript off", async ({ page }) => {
    await registerAndLogin(page, "e2e-agnewatt", "Agenda Justificatif");
    await createGroup(page, "Famille Justificatifs");
    await page.goto("/agenda/new");

    const date = dayThisMonth(18);
    await page.getByLabel("Titre").fill("Rendez-vous avec justificatif");
    await page.getByLabel("Début").fill(`${date}T14:00`);
    await page.getByLabel("Fin").fill(`${date}T15:00`);
    await page.getByLabel("Pièce jointe").setInputFiles({
      name: "convocation.png",
      mimeType: "image/png",
      buffer: PNG_1X1,
    });
    await page.getByRole("button", { name: "Créer l'événement" }).click();
    await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);
    await expect(page.getByText("Événement créé.")).toBeVisible();

    await openEventDetail(page, "Rendez-vous avec justificatif");
    const link = page.getByRole("link", { name: "convocation.png" });
    await expect(link).toBeVisible();
    const href = await link.getAttribute("href");
    if (!href) throw new Error("no download link");
    const resp = await page.request.get(href, { maxRedirects: 0 });
    expect(resp.status()).toBe(200);
    expect(Buffer.compare(await resp.body(), PNG_1X1)).toBe(0);
  });

  // A refused file is caught before the event is written: the form comes
  // back under the reason, and the agenda holds nothing.
  test("a refused file brings the sheet back and creates nothing", async ({ page }) => {
    await registerAndLogin(page, "e2e-agnewbad", "Agenda Fichier Refusé");
    await createGroup(page, "Famille Refus");
    await page.goto("/agenda/new");

    const date = dayThisMonth(19);
    await page.getByLabel("Titre").fill("Événement au mauvais fichier");
    await page.getByLabel("Début").fill(`${date}T09:00`);
    await page.getByLabel("Fin").fill(`${date}T10:00`);
    await page.getByLabel("Pièce jointe").setInputFiles({
      name: "notes.txt",
      mimeType: "text/plain",
      buffer: Buffer.from("bonjour"),
    });
    await page.getByRole("button", { name: "Créer l'événement" }).click();
    await expect(page).toHaveURL("/agenda/new");
    await expect(page.getByText("Type de fichier non autorisé (png, jpg, webp, pdf, mp4, webm).")).toBeVisible();

    await page.goto(`/agenda?view=month&date=${date}`);
    await expect(page.getByRole("link", { name: /Événement au mauvais fichier/ })).toHaveCount(0);
  });
});

// #621: the « Privé » box of the forms and the mark it leaves. A private
// event is seen by its creator and the members of its « pour qui » only
// (apps/api's policy, #620); everyone else in the family gets nothing — not
// on the agenda, not on the home page, and the "not found" page for its URL.
test.describe("Agenda — événements privés (#621)", () => {
  /** A fresh invitation link into the family `page`'s user owns. */
  async function invitation(page: Page, family: string): Promise<string> {
    await page.goto("/groups");
    await page.locator("li", { hasText: family }).getByRole("link", { name: "Membres" }).click();
    await page.getByRole("button", { name: "Créer une invitation" }).click();
    const href = await page.locator(".notice.success a").getAttribute("href");
    if (!href) throw new Error("no invitation link");
    return href;
  }

  async function join(page: Page, href: string): Promise<void> {
    await page.goto(href);
    await page.getByRole("button", { name: "Rejoindre le groupe" }).click();
  }

  /** The event's id, read off its chip on the agenda of `date`. */
  async function eventId(page: Page, date: string, title: string): Promise<string> {
    await page.goto(`/agenda?date=${date}`);
    const href = await page.getByRole("link", { name: new RegExp(title) }).first().getAttribute("href");
    const id = href?.match(/^\/agenda\/([0-9a-f-]{36})/)?.[1];
    if (!id) throw new Error(`no chip for ${title}: ${href}`);
    return id;
  }

  const privateBox = (page: Page) => page.getByRole("checkbox", { name: /^Privé — visible seulement/ });

  test("seuls le créateur et « pour qui » le voient ; décoché, toute la famille le voit", async ({
    page,
    browser,
  }) => {
    test.slow();
    const family = "Famille Confidences";
    await registerAndLogin(page, "e2e-agpriva", "Alix Créatrice");
    await createGroup(page, family);
    const toB = await invitation(page, family);
    const toC = await invitation(page, family);

    const bContext = await browser.newContext({ reducedMotion: "reduce" });
    const b = await bContext.newPage();
    await registerAndLogin(b, "e2e-agprivb", "Basile Absent");
    await join(b, toB);
    const cContext = await browser.newContext({ reducedMotion: "reduce" });
    const c = await cContext.newPage();
    await registerAndLogin(c, "e2e-agprivc", "Côme Invité");
    await join(c, toC);

    // Tomorrow, so that the home page lists it whatever the hour.
    const date = parisDay(1);

    // A, for themselves only: the box sits under « pour qui », unchecked.
    await page.goto("/agenda/new");
    await expect(privateBox(page)).not.toBeChecked();
    await page.getByLabel("Titre").fill("Rendez-vous secret");
    await page.getByLabel("Début").fill(`${date}T10:00`);
    await page.getByLabel("Fin").fill(`${date}T11:00`);
    await privateBox(page).check();
    await page.getByRole("button", { name: "Créer l'événement" }).click();
    await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);

    // A sees it, marked « Privé »: on the agenda, on the home page, on its page.
    const secret = await eventId(page, date, "Rendez-vous secret");
    await expect(page.getByRole("link", { name: /^Privé 10:00 Rendez-vous secret$/ })).toBeVisible();
    await page.goto("/");
    await expect(page.getByText(/^Privé Rendez-vous secret$/).first()).toBeVisible();
    await page.goto(`/agenda/${secret}`);
    await expect(page.getByText("Privé — visible seulement par son créateur")).toBeVisible();

    // B, in the same family, sees nothing of it, and its URL is not found.
    await b.goto(`/agenda?date=${date}`);
    await expect(b.getByRole("link", { name: /Rendez-vous secret/ })).toHaveCount(0);
    await b.goto("/");
    await expect(b.getByText("Rendez-vous secret")).toHaveCount(0);
    await b.goto(`/agenda/${secret}`);
    await expect(b.getByRole("heading", { name: "Événement introuvable" })).toBeVisible();

    // A checks C in « pour qui »: C sees that one, B still does not.
    await page.goto("/agenda/new");
    await page.getByLabel("Titre").fill("Surprise pour Côme");
    await page.getByLabel("Début").fill(`${date}T14:00`);
    await page.getByLabel("Fin").fill(`${date}T15:00`);
    await page.getByRole("checkbox", { name: "Côme Invité" }).check();
    await privateBox(page).check();
    await page.getByRole("button", { name: "Créer l'événement" }).click();
    await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);
    await c.goto(`/agenda?date=${date}`);
    await expect(c.getByRole("link", { name: /^Privé 14:00 Surprise pour Côme$/ })).toBeVisible();
    await expect(c.getByRole("link", { name: /Rendez-vous secret/ })).toHaveCount(0);
    await b.goto(`/agenda?date=${date}`);
    await expect(b.getByRole("link", { name: /Surprise pour Côme/ })).toHaveCount(0);

    // A unchecks « Privé » on the first: B sees it, unmarked.
    await page.goto(`/agenda/${secret}/edit`);
    await expect(privateBox(page)).toBeChecked();
    await privateBox(page).uncheck();
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page.getByText("Événement mis à jour.")).toBeVisible();
    await expect(page.getByText("Privé — visible seulement")).toHaveCount(0);
    await b.goto(`/agenda?date=${date}`);
    await expect(b.getByRole("link", { name: /^10:00 Rendez-vous secret$/ })).toBeVisible();

    await bContext.close();
    await cContext.close();
  });

  test("la case se coche et s'envoie sans JavaScript", async ({ browser }) => {
    const context = await browser.newContext({ javaScriptEnabled: false, reducedMotion: "reduce" });
    const page = await context.newPage();
    await registerAndLogin(page, "e2e-agprivnojs", "Agenda Discret");
    await createGroup(page, "Famille Sans Script Discrète");
    const date = dayThisMonth(20);
    await page.goto("/agenda/new");
    await page.getByLabel("Titre").fill("Note discrète");
    await page.getByLabel("Début").fill(`${date}T09:00`);
    await page.getByLabel("Fin").fill(`${date}T10:00`);
    await privateBox(page).check();
    await page.getByRole("button", { name: "Créer l'événement" }).click();
    await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);
    await page.goto(`/agenda?date=${date}`);
    await expect(page.getByRole("link", { name: /^Privé 09:00 Note discrète$/ })).toBeVisible();
    await context.close();
  });
});

// #698: the recurrence settings open under an « Événement récurrent » box.
// Group names avoid every label of the form (`getByLabel` matches
// substrings, the family switcher included).
test.describe("Agenda — case « Événement récurrent » (#698)", () => {
  const recurringBox = (page: Page) => page.getByRole("checkbox", { name: "Événement récurrent" });
  const recurrenceLine = (page: Page) => page.getByText("Récurrence :");

  test("cocher ouvre les réglages, décocher les masque et l'envoi reste ponctuel", async ({ page }) => {
    await registerAndLogin(page, "e2e-agrecbox", "Cycle User");
    await createGroup(page, "Famille Cycle");
    const date = dayThisMonth(19);
    const box = recurringBox(page);
    const settings = page.locator("#recurrence-settings");
    const freq = page.getByLabel("Fréquence");

    await page.goto("/agenda/new");
    await expect(box).not.toBeChecked();
    await expect(box).toHaveAttribute("aria-expanded", "false");
    await expect(box).toHaveAttribute("aria-controls", "recurrence-settings");
    await expect(settings).toBeHidden();
    await expect(freq).toBeHidden();

    await box.check();
    await expect(box).toHaveAttribute("aria-expanded", "true");
    await expect(settings).toBeVisible();
    // No frequency chosen yet: the box picks the weekly one.
    await expect(freq).toHaveValue("weekly");
    await page.getByRole("checkbox", { name: "Lun" }).check();

    // Unchecked, the settings are hidden again and what they hold is not sent.
    await box.uncheck();
    await expect(box).toHaveAttribute("aria-expanded", "false");
    await expect(settings).toBeHidden();
    await page.getByLabel("Titre").fill("Rendez-vous unique");
    await page.getByLabel("Début").fill(`${date}T09:00`);
    await page.getByLabel("Fin").fill(`${date}T10:00`);
    await page.getByRole("button", { name: "Créer l'événement" }).click();
    await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);
    await openEventDetail(page, "Rendez-vous unique");
    await expect(recurrenceLine(page)).toHaveCount(0);
  });

  test("l'édition d'un événement récurrent ouvre la case cochée ; décochée, la règle s'en va", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-agrecedit", "Cycle Editor");
    await createGroup(page, "Famille Cycle Bis");
    const date = dayThisMonth(20);
    await createEvent(page, {
      title: "Cours de piano",
      start: `${date}T17:00`,
      end: `${date}T18:00`,
      freq: "daily",
    });
    await openEventDetail(page, "Cours de piano");
    await expect(page.getByText("Récurrence : Tous les jours")).toBeVisible();

    await page.getByRole("link", { name: "Modifier" }).click();
    const box = recurringBox(page);
    await expect(box).toBeChecked();
    await expect(box).toHaveAttribute("aria-expanded", "true");
    await expect(page.getByLabel("Fréquence")).toHaveValue("daily");
    await box.uncheck();
    await expect(page.locator("#recurrence-settings")).toBeHidden();
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page.getByText("Événement mis à jour.")).toBeVisible();
    await expect(recurrenceLine(page)).toHaveCount(0);
  });

  test("sans JavaScript, pas de case : les réglages restent affichés et « Aucune » reste un choix", async ({
    browser,
  }) => {
    const context = await browser.newContext({ javaScriptEnabled: false, reducedMotion: "reduce" });
    const page = await context.newPage();
    await registerAndLogin(page, "e2e-agrecnojs", "Cycle Sans Script");
    await createGroup(page, "Famille Cycle Ter");
    const date = dayThisMonth(21);

    await page.goto("/agenda/new");
    await expect(recurringBox(page)).toHaveCount(0);
    await expect(page.locator("#recurrence-settings")).toBeVisible();
    await expect(page.getByLabel("Fréquence")).toHaveValue("none");
    await page.getByLabel("Titre").fill("Marché du samedi");
    await page.getByLabel("Début").fill(`${date}T08:00`);
    // An end before the start: the sheet comes back under its error with
    // the frequency chosen, still without a box.
    await page.getByLabel("Fin").fill(`${date}T07:00`);
    await page.getByLabel("Fréquence").selectOption("weekly");
    await page.getByRole("button", { name: "Créer l'événement" }).click();
    await expect(page.getByText("La fin doit être après le début.")).toBeVisible();
    await expect(recurringBox(page)).toHaveCount(0);
    await expect(page.locator("#recurrence-settings")).toBeVisible();
    await expect(page.getByLabel("Fréquence")).toHaveValue("weekly");

    await page.getByLabel("Fin").fill(`${date}T09:00`);
    await page.getByRole("button", { name: "Créer l'événement" }).click();
    await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);
    await page.goto(`/agenda?date=${date}`);
    await page.getByRole("link", { name: /Marché du samedi/ }).first().click();
    await expect(page.getByText("Récurrence : Chaque semaine")).toBeVisible();

    // The edit page of that recurring event: « Aucune » clears the rule.
    await page.getByRole("link", { name: "Modifier" }).click();
    await expect(recurringBox(page)).toHaveCount(0);
    await expect(page.getByLabel("Fréquence")).toHaveValue("weekly");
    await page.getByLabel("Fréquence").selectOption("none");
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page.getByText("Événement mis à jour.")).toBeVisible();
    await expect(recurrenceLine(page)).toHaveCount(0);
    await context.close();
  });
});

// #709: the edit form of a recurring event opened from one of its
// occurrences asks what the change applies to — the whole series (by
// default), this occurrence and the following ones, or this one alone.
test.describe("Agenda — portée d'une modification de série", () => {
  /** The calendar chip of `title`'s occurrence on `day` (17:00 in Paris is
   *  the same UTC day, which the chip's `occ` names). */
  const chipOn = (page: Page, day: string) => page.locator(`a.chip[href*="occ=${day}T"]`);
  const scopeGroup = (page: Page) =>
    page.getByRole("group", { name: "Appliquer la modification à" });

  test("une occurrence seule, puis elle et les suivantes ; le reste de la série ne bouge pas", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-agscope", "Scope Editor");
    await createGroup(page, "Famille Portée");
    const [first, third, fourth, fifth, sixth] = [10, 12, 13, 14, 15].map(dayThisMonth);
    await createEvent(page, {
      title: "Atelier",
      start: `${first}T17:00`,
      end: `${first}T18:00`,
      freq: "daily",
    });

    // The third occurrence, alone, moved to 19:00 and renamed.
    await page.goto(`/agenda?date=${first}`);
    await chipOn(page, third).click();
    await page.getByRole("link", { name: "Modifier" }).click();
    await expect(page.getByLabel("Début")).toHaveValue(`${third}T17:00`);
    const scope = scopeGroup(page);
    const whole = scope.getByRole("radio", { name: "Toute la série" });
    await expect(whole).toBeChecked();
    // Native radios: the arrows move the choice.
    await whole.focus();
    await page.keyboard.press("ArrowDown");
    await expect(scope.getByRole("radio", { name: "Cet événement et les suivants" })).toBeChecked();
    await page.keyboard.press("ArrowDown");
    await expect(scope.getByRole("radio", { name: "Cet événement uniquement" })).toBeChecked();
    await page.getByLabel("Titre").fill("Atelier déplacé");
    await page.getByLabel("Début").fill(`${third}T19:00`);
    await page.getByLabel("Fin").fill(`${third}T20:00`);
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page.getByText("Événement mis à jour.")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Atelier déplacé" })).toBeVisible();
    await expect(page.getByText("Récurrence :")).toHaveCount(0);

    await page.goto(`/agenda?date=${first}`);
    await expect(page.locator("a.chip", { hasText: "Atelier déplacé" })).toHaveCount(1);
    await expect(chipOn(page, third)).toHaveText(/19:00 Atelier déplacé/);
    await expect(chipOn(page, fourth)).toHaveText(/17:00 Atelier$/);

    // From the fifth on, renamed: the fourth keeps its title.
    await chipOn(page, fifth).click();
    await page.getByRole("link", { name: "Modifier" }).click();
    await scopeGroup(page).getByRole("radio", { name: "Cet événement et les suivants" }).check();
    await page.getByLabel("Titre").fill("Atelier du soir");
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page.getByText("Événement mis à jour.")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Atelier du soir" })).toBeVisible();
    await expect(page.getByText("Récurrence : Tous les jours")).toBeVisible();

    await page.goto(`/agenda?date=${first}`);
    await expect(chipOn(page, first)).toHaveText(/17:00 Atelier$/);
    await expect(chipOn(page, fourth)).toHaveText(/17:00 Atelier$/);
    await expect(chipOn(page, fifth)).toHaveText(/17:00 Atelier du soir$/);
    await expect(chipOn(page, sixth)).toHaveText(/17:00 Atelier du soir$/);
  });

  test("sur un événement unique, le formulaire ne demande rien", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-agscopenone", "Scope Plain");
    await createGroup(page, "Famille Portée Bis");
    const date = dayThisMonth(16);
    await createEvent(page, {
      title: "Rendez-vous isolé",
      start: `${date}T09:00`,
      end: `${date}T10:00`,
    });
    await openEventDetail(page, "Rendez-vous isolé");
    await page.getByRole("link", { name: "Modifier" }).click();
    await expect(page.getByRole("heading", { name: /Modifier/ })).toBeVisible();
    await expect(page.getByRole("radio")).toHaveCount(0);
  });
});
