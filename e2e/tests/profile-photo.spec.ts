import { expect, Locator, Page, test } from "@playwright/test";
import { writeFile } from "node:fs/promises";
import { crc32, deflateSync } from "node:zlib";
import { fetchVerificationToken } from "../lib/db";

// Profile photo (#468): uploaded, replaced and removed from /account, and
// shown in place of the member's initial wherever their `.avatar` appears —
// the messagerie and their profile (#466) here. apps/api holds and judges
// the photo (#467, #588); these journeys check what the page makes of its
// answers, refusals included.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-profile-photo-password-1";

/** Register + verify + login a fresh user on the given page. */
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

/** One PNG chunk: length, type, data, CRC of type and data. */
function chunk(type: string, data: Buffer): Buffer {
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const typed = Buffer.concat([Buffer.from(type, "latin1"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(typed));
  return Buffer.concat([length, typed, crc]);
}

type Rgb = [number, number, number];

/** An RGB PNG of `width` × `height`, every pixel `rgb` — or, given
 * `right`, its left half `rgb` and its right half `right`. */
function png(width: number, height: number, rgb: Rgb, right: Rgb = rgb): Buffer {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8; // bit depth
  header[9] = 2; // RGB
  const row = Buffer.alloc(1 + width * 3); // filter byte 0, then the pixels
  for (let x = 0; x < width; x++) row.set(x < width / 2 ? rgb : right, 1 + x * 3);
  const rows = Buffer.concat(Array.from({ length: height }, () => row));
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(rows)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

/** The photo input of /account. */
function photoInput(page: Page): Locator {
  return page.getByLabel("Photo (JPEG ou PNG");
}

async function uploadPhoto(page: Page, name: string, mimeType: string, buffer: Buffer): Promise<void> {
  await uploadFile(page, { name, mimeType, buffer });
}

/** Sends the /account photo form with `file`: a payload, or the path of
 * a file on disk. */
async function uploadFile(page: Page, file: string | { name: string; mimeType: string; buffer: Buffer }): Promise<void> {
  await page.goto("/account");
  await photoInput(page).setInputFiles(file);
  await page.locator("form[action='/account/avatar'] button[type=submit]").click();
}

/** The natural width of an `<img>` once loaded, 0 if it did not load. */
async function loadedWidth(img: Locator): Promise<number> {
  return img.evaluate(async (el: HTMLImageElement) => {
    if (!el.complete) await new Promise((done) => el.addEventListener("load", done, { once: true }));
    return el.naturalWidth;
  });
}

// Without JavaScript, the native field and apps/api's centred square, as
// before #860: these journeys send the files as they are, so that each
// refusal of apps/api is reached. With JavaScript the photo is framed and
// re-encoded on the device first (see "Framing the profile photo" below).
test.describe("Profile photo (#468)", () => {
  test.use({ javaScriptEnabled: false });

  test("an uploaded photo shows in the messagerie and on the profile, and its removal brings the initial back", async ({
    page,
    browser,
  }) => {
    await registerAndLogin(page, "e2e-photo-owner", "Photo Auteur");
    await page.goto("/groups/new");
    await page.getByLabel("Nom du groupe").fill("Famille Portrait");
    await page.getByRole("button", { name: "Créer le groupe" }).click();
    await page.goto("/groups");
    await page.locator("li", { hasText: "Famille Portrait" }).getByRole("link", { name: "Membres" }).click();
    await page.getByRole("button", { name: "Créer une invitation" }).click();
    const href = await page.locator(".notice.success a").getAttribute("href");
    if (!href) throw new Error("no invitation link");
    await page.goto("/messagerie");
    await page.getByLabel("Votre message").fill("Voici ma photo");
    await page.getByRole("button", { name: "Envoyer" }).click();

    const context = await browser.newContext();
    const member = await context.newPage();
    await registerAndLogin(member, "e2e-photo-reader", "Photo Lecteur");
    await member.goto(href);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    // Before: the initial, and a picker for JPEG and PNG only.
    await page.goto("/account");
    const section = page.locator("section", { hasText: "Photo de profil" });
    await expect(section.locator(".avatar")).toHaveText("P");
    await expect(photoInput(page)).toHaveAttribute("accept", "image/jpeg,image/png");
    await expect(page.getByRole("button", { name: "Retirer la photo" })).toHaveCount(0);

    await uploadPhoto(page, "moi.png", "image/png", png(400, 300, [200, 40, 40]));
    await expect(page).toHaveURL(/\/account\?notice=avatar_updated$/);
    await expect(page.getByText("Photo de profil enregistrée.")).toBeVisible();
    const own = section.locator(".avatar img");
    await expect(own).toHaveAttribute("src", "/account/avatar");
    await expect(own).toHaveAttribute("alt", "");
    // apps/api's stored square, 256 px, shown at the avatar's fixed size.
    expect(await loadedWidth(own)).toBe(256);
    await expect(page.getByRole("button", { name: "Remplacer la photo" })).toBeVisible();

    // Another member sees it on the author's message, in the link to the
    // author's profile, and on that profile.
    await member.goto("/messagerie");
    const row = member.locator("li[data-message-id]", { hasText: "Voici ma photo" });
    const link = row.getByRole("link", { name: "Photo Auteur", exact: true });
    const photo = link.locator(".avatar img");
    await expect(photo).toHaveAttribute("src", /^\/groups\/[0-9a-f-]{36}\/members\/[0-9a-f-]{36}\/avatar$/);
    expect(await loadedWidth(photo)).toBe(256);
    await link.click();
    await expect(member).toHaveURL(/\/groups\/[0-9a-f-]{36}\/members\/[0-9a-f-]{36}$/);
    const heading = member.getByRole("heading", { level: 1 });
    await expect(heading).toHaveText(/Photo Auteur/);
    expect(await loadedWidth(heading.locator(".avatar img"))).toBe(256);

    // Removed: the initial comes back everywhere.
    await page.goto("/account");
    await page.getByRole("button", { name: "Retirer la photo" }).click();
    await expect(page).toHaveURL(/\/account\?notice=avatar_removed$/);
    await expect(section.locator(".avatar img")).toHaveCount(0);
    await expect(section.locator(".avatar")).toHaveText("P");

    await member.reload();
    await expect(heading.locator(".avatar img")).toHaveCount(0);
    await expect(heading.locator(".avatar")).toHaveText("P");
    await member.goto("/messagerie");
    await expect(link.locator(".avatar img")).toHaveCount(0);
    await expect(link.locator(".avatar")).toHaveText("P");

    await context.close();
  });

  test("each refusal of the photo is explained in French", async ({ page }, testInfo) => {
    await registerAndLogin(page, "e2e-photo-refused", "Photo Refusée");

    // WebP is no longer taken (arbitrage of 2026-10-24); the picker does
    // not offer it, a file handed to the form anyway is refused by apps/api.
    const webp = Buffer.concat([
      Buffer.from("RIFF", "latin1"),
      Buffer.from([0x24, 0, 0, 0]),
      Buffer.from("WEBPVP8 ", "latin1"),
      Buffer.alloc(28),
    ]);
    await uploadPhoto(page, "moi.webp", "image/webp", webp);
    await expect(page).toHaveURL(/\/account\?error=unsupported_file_type$/);
    await expect(
      page.getByText("Format non pris en charge : la photo doit être un fichier JPEG ou PNG."),
    ).toBeVisible();

    // A PNG whose frame passes the bound of a decode (#588): refused with
    // its own code, not "unreadable".
    await uploadPhoto(page, "immense.png", "image/png", png(4000, 4000, [0, 0, 0]));
    await expect(page).toHaveURL(/\/account\?error=image_too_large$/);
    await expect(page.getByText("Cette image a trop de pixels pour être traitée.")).toBeVisible();

    // Past apps/api's 32 MiB, and its form past the route's limit: refused
    // by apps/web before relaying it, once read to its end (#976). Handed
    // over as a file on disk: as a payload, its 33 MiB cross the protocol
    // to the page in base64, and with several workers at once the page
    // stalled or crashed in `setInputFiles` before anything was sent.
    const heavy = testInfo.outputPath("lourde.png");
    await writeFile(heavy, Buffer.concat([png(8, 8, [0, 0, 0]), Buffer.alloc(33 * 1024 * 1024)]));
    await uploadFile(page, heavy);
    await expect(page).toHaveURL(/\/account\?error=file_too_large$/);
    await expect(page.getByText("La photo dépasse 32 Mo.")).toBeVisible();

    // Not a picture at all.
    await uploadPhoto(page, "moi.jpg", "image/jpeg", Buffer.from([0xff, 0xd8, 0xff, 0xe0, 0, 0, 0, 0]));
    await expect(page).toHaveURL(/\/account\?error=unreadable_image$/);
    await expect(page.getByText("Cette image n'a pas pu être lue")).toBeVisible();

    // Nothing was stored: the initial is still there.
    await expect(page.locator("section", { hasText: "Photo de profil" }).locator(".avatar")).toHaveText("P");
  });
});

const RED: Rgb = [200, 30, 30];
const BLUE: Rgb = [30, 30, 200];

/** The colour at the centre of the caller's stored photo, read back from
 * the `<img>` of /account (same origin: the canvas is not tainted). */
async function storedPhotoCentre(page: Page): Promise<Rgb> {
  const img = page.locator("section", { hasText: "Photo de profil" }).locator(".avatar img").first();
  await expect(img).toHaveAttribute("src", "/account/avatar");
  return img.evaluate(async (el: HTMLImageElement) => {
    if (!el.complete) await new Promise((done) => el.addEventListener("load", done, { once: true }));
    const canvas = document.createElement("canvas");
    canvas.width = el.naturalWidth;
    canvas.height = el.naturalHeight;
    const ctx = canvas.getContext("2d")!;
    ctx.drawImage(el, 0, 0);
    const [r, g, b] = ctx.getImageData(canvas.width / 2, canvas.height / 2, 1, 1).data;
    return [r, g, b] as Rgb;
  });
}

/** Near enough to `want` for a colour gone through two JPEG encodings. */
function expectColour(got: Rgb, want: Rgb): void {
  for (let i = 0; i < 3; i++) expect(Math.abs(got[i] - want[i]), `${got} vs ${want}`).toBeLessThan(40);
}

/** The framing square, as assistive technologies name it. */
function square(page: Page): Locator {
  return page.getByRole("group", { name: /^Carré de découpage/ });
}

// #860: with JavaScript, the photo is shown whole and the square on it is
// what is sent. The test photo is 400 × 200, its left half red and its
// right half blue: apps/api's centred square would be half and half, so
// the colour of the stored photo says which square was sent.
test.describe("Framing the profile photo (#860)", () => {
  test("the square dragged and resized with the pointer is the photo stored", async ({ page }) => {
    await registerAndLogin(page, "e2e-photo-frame", "Cadre Souris");
    await page.goto("/account");
    // The native field gives way to the button.
    await expect(photoInput(page)).toBeHidden();
    await expect(page.getByRole("button", { name: "Ajouter une photo" })).toBeHidden();
    await expect(page.getByRole("button", { name: "Choisir une photo" })).toBeVisible();

    await photoInput(page).setInputFiles({ name: "moi.png", mimeType: "image/png", buffer: png(400, 200, RED, BLUE) });
    const box = square(page);
    await expect(box).toBeVisible();
    await expect(page.getByText("Aperçu de votre icône")).toBeVisible();
    const live = page.locator("[data-crop-live]");

    // It starts centred, at 80 % of the photo's height. Its bottom right
    // corner, pulled down and right: the square grows, up to the photo's
    // bottom edge.
    // The mouse works in the viewport: the whole photo in view first.
    await page.locator("[data-crop-stage]").scrollIntoViewIfNeeded();
    let at = (await box.boundingBox())!;
    await page.mouse.move(at.x + at.width - 4, at.y + at.height - 4);
    await page.mouse.down();
    await page.mouse.move(at.x + at.width + 200, at.y + at.height + 200, { steps: 4 });
    await page.mouse.up();
    await expect(live).toHaveText("Carré à la position choisie, côté 90 % de la photo.");
    const grown = (await box.boundingBox())!;
    expect(grown.width).toBeGreaterThan(at.width + 4);

    // The square, dragged far to the right: it stops at the photo's edge.
    at = grown;
    await page.mouse.move(at.x + at.width / 2, at.y + at.height / 2);
    await page.mouse.down();
    await page.mouse.move(at.x + at.width / 2 + 1000, at.y + at.height / 2, { steps: 5 });
    await page.mouse.up();
    const stage = (await page.locator("[data-crop-stage]").boundingBox())!;
    const moved = (await box.boundingBox())!;
    expect(Math.abs(moved.x + moved.width - (stage.x + stage.width))).toBeLessThan(4);

    await page.getByRole("button", { name: "Enregistrer la photo" }).click();
    await expect(page).toHaveURL(/\/account\?notice=avatar_updated$/);
    await expect(page.getByText("Photo de profil enregistrée.")).toBeVisible();
    expectColour(await storedPhotoCentre(page), BLUE);
  });

  test("the keyboard alone chooses, moves, resizes and sends the square", async ({ page }) => {
    await registerAndLogin(page, "e2e-photo-keys", "Cadre Clavier");
    await page.goto("/account");

    const choose = page.getByRole("button", { name: "Choisir une photo" });
    await choose.focus();
    const chooser = page.waitForEvent("filechooser");
    await page.keyboard.press("Enter");
    await (await chooser).setFiles({ name: "moi.png", mimeType: "image/png", buffer: png(400, 200, RED, BLUE) });

    // The square takes the focus once the photo is read.
    const box = square(page);
    await expect(box).toBeFocused();
    const live = page.locator("[data-crop-live]");

    // It starts at 160 px, 80 % of the photo's height and its least side.
    // A step is 8 px, 2 % of the photo's width.
    await page.keyboard.press("-");
    await expect(live).toHaveText("Carré réduit, côté 80 % de la photo.");
    await page.keyboard.press("Shift+ArrowRight");
    await expect(live).toHaveText("Carré agrandi, côté 84 % de la photo.");
    await page.keyboard.press("+");
    await expect(live).toHaveText("Carré agrandi, côté 88 % de la photo.");
    await page.keyboard.press("Shift+ArrowUp");
    await expect(live).toHaveText("Carré réduit, côté 84 % de la photo.");
    await page.keyboard.press("Home");
    await expect(live).toHaveText("Carré recentré.");
    // Then all the way left: the square holds red only.
    for (let i = 0; i < 20; i++) await page.keyboard.press("ArrowLeft");
    await expect(live).toHaveText("Carré déplacé vers la gauche.");
    const stage = (await page.locator("[data-crop-stage]").boundingBox())!;
    expect(Math.abs((await box.boundingBox())!.x - stage.x)).toBeLessThan(4);

    // Tab on to « Enregistrer la photo », past the shortcuts' summary.
    const save = page.getByRole("button", { name: "Enregistrer la photo" });
    for (let i = 0; i < 4 && !(await save.evaluate((el) => el === document.activeElement)); i++) {
      await page.keyboard.press("Tab");
    }
    await expect(save).toBeFocused();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/account\?notice=avatar_updated$/);
    expectColour(await storedPhotoCentre(page), RED);
  });

  // A refusal replaces the page's own response banners (`p.notice`), not
  // the panel of a scheduled deletion (`section.notice.error`), which says
  // the account is about to go and holds the button that cancels it.
  test("a failed send is told under the profile head, and a scheduled deletion's panel stays", async ({ page }) => {
    await registerAndLogin(page, "e2e-photo-pending", "Cadre Suppression");
    await page.goto("/account/delete");
    page.once("dialog", (d) => d.accept());
    await page.getByRole("textbox", { name: "Mot de passe actuel" }).fill(PASSWORD);
    await page.getByRole("checkbox").check();
    await page.getByRole("button", { name: "Demander la suppression de mon compte" }).click();
    await expect(page).toHaveURL("/account?notice=deletion_requested");
    const panel = page.locator("section.notice.error");
    await expect(panel).toContainText("Suppression de compte programmée");
    const done = page.locator("p.notice.success");
    await expect(done).toBeVisible();

    // The send fails before apps/api: a gateway with no answer.
    await page.route("**/account/avatar", (route) =>
      route.request().method() === "POST" ? route.fulfill({ status: 502, body: "" }) : route.continue(),
    );
    await photoInput(page).setInputFiles({ name: "moi.png", mimeType: "image/png", buffer: png(400, 200, RED, BLUE) });
    await expect(square(page)).toBeVisible();
    await page.getByRole("button", { name: "Enregistrer la photo" }).click();

    await expect(page.getByRole("alert").filter({ hasText: "La photo n'a pas pu être envoyée. Merci de réessayer." })).toBeVisible();
    await expect(panel).toContainText("Suppression de compte programmée");
    await expect(panel.getByRole("button", { name: "Annuler la demande de suppression" })).toBeVisible();
    // The banner of the previous answer is gone; the framing stays, to retry.
    await expect(done).toHaveCount(0);
    await expect(square(page)).toBeVisible();
    await expect(page.getByRole("button", { name: "Enregistrer la photo" })).toBeEnabled();
  });

  test("a file the browser cannot read is said so, and another can be chosen", async ({ page }) => {
    await registerAndLogin(page, "e2e-photo-unreadable", "Cadre Illisible");
    await page.goto("/account");
    await photoInput(page).setInputFiles({
      name: "moi.jpg",
      mimeType: "image/jpeg",
      buffer: Buffer.from([0xff, 0xd8, 0xff, 0xe0, 0, 0, 0, 0]),
    });
    await expect(page.getByText("Ce fichier n'est pas une image lisible.")).toBeVisible();
    await expect(square(page)).toBeHidden();
    await expect(page.getByRole("button", { name: "Choisir une photo" })).toBeVisible();
    // Nothing was sent.
    await expect(page).toHaveURL(/\/account$/);
  });
});
