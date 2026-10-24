import { expect, Locator, Page, test } from "@playwright/test";
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

/** An RGB PNG of `width` × `height`, every pixel `rgb`. */
function png(width: number, height: number, rgb: [number, number, number]): Buffer {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8; // bit depth
  header[9] = 2; // RGB
  const row = Buffer.alloc(1 + width * 3); // filter byte 0, then the pixels
  for (let x = 0; x < width; x++) row.set(rgb, 1 + x * 3);
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
  await page.goto("/account");
  await photoInput(page).setInputFiles({ name, mimeType, buffer });
  await page.locator("form[action='/account/avatar'] button[type=submit]").click();
}

/** The natural width of an `<img>` once loaded, 0 if it did not load. */
async function loadedWidth(img: Locator): Promise<number> {
  return img.evaluate(async (el: HTMLImageElement) => {
    if (!el.complete) await new Promise((done) => el.addEventListener("load", done, { once: true }));
    return el.naturalWidth;
  });
}

test.describe("Profile photo (#468)", () => {
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

  test("each refusal of the photo is explained in French", async ({ page }) => {
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

    // Past apps/api's 32 MiB.
    const heavy = Buffer.concat([png(8, 8, [0, 0, 0]), Buffer.alloc(33 * 1024 * 1024)]);
    await uploadPhoto(page, "lourde.png", "image/png", heavy);
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
