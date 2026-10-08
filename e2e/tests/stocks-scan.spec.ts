import { expect, Page, test } from "@playwright/test";
import { crc32, deflateSync } from "node:zlib";
import { fetchVerificationToken } from "../lib/db";

// Issue #402 — scanning an article's barcode on /stocks/new. The photo is a
// plain multipart form, decoded by apps/web; the code goes to apps/api,
// which asks Open Food Facts — here the stack's stand-in
// (e2e/scripts/off-stub.ts, `OPENFOODFACTS_BASE_URL`), which knows KNOWN
// and answers 404 for anything else. No test reaches the real service.
//
// The camera itself is the browser's: what is tested is the photo it would
// hand over, drawn here as a PNG. Issue #403 adds the GS1 2D codes, whose
// decoding apps/web's unit tests cover: here, the string one decodes to.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-scan-password-1";

/** In the stand-in, as "Pâte à tartiner", 400 g. */
const KNOWN = "3017620422003";
/** Unknown to the stand-in. */
const UNKNOWN = "4006381333931";
const WEIGHED = "2123456012347";

/** An 8-bit greyscale PNG of `rows` (0 = black, 255 = white). */
function png(width: number, height: number, pixel: (x: number, y: number) => number): Buffer {
  const raw = Buffer.alloc((width + 1) * height);
  for (let y = 0; y < height; y++) {
    raw[y * (width + 1)] = 0; // filter: none
    for (let x = 0; x < width; x++) raw[y * (width + 1) + 1 + x] = pixel(x, y);
  }
  const chunk = (type: string, data: Buffer): Buffer => {
    const length = Buffer.alloc(4);
    length.writeUInt32BE(data.length);
    const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(body));
    return Buffer.concat([length, body, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 0; // greyscale
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

/** A photo of an EAN-13: start guard, six digits in the L/G parity the
 *  first digit sets, centre guard, six in R, end guard, quiet zones. */
function ean13Photo(digits: string): Buffer {
  const L = ["0001101", "0011001", "0010011", "0111101", "0100011", "0110001", "0101111", "0111011", "0110111", "0001011"];
  const G = ["0100111", "0110011", "0011011", "0100001", "0011101", "0111001", "0000101", "0010001", "0001001", "0010111"];
  const R = ["1110010", "1100110", "1101100", "1000010", "1011100", "1001110", "1010000", "1000100", "1001000", "1110100"];
  const PARITY = ["LLLLLL", "LLGLGG", "LLGGLG", "LLGGGL", "LGLLGG", "LGGLLG", "LGGGLL", "LGLGLG", "LGLGGL", "LGGLGL"];
  const d = digits.split("").map(Number);
  let bits = "101";
  for (let i = 1; i <= 6; i++) bits += (PARITY[d[0]][i - 1] === "L" ? L : G)[d[i]];
  bits += "01010";
  for (let i = 7; i <= 12; i++) bits += R[d[i]];
  bits += "101";
  const module = 4;
  const quiet = 15 * module;
  return png(bits.length * module + 2 * quiet, 240, (x, y) => {
    const bar = Math.floor((x - quiet) / module);
    const dark = y >= 20 && y < 220 && x >= quiet && bar < bits.length && bits[bar] === "1";
    return dark ? 0 : 255;
  });
}

const BLANK_PHOTO = png(640, 480, () => 255);

async function registerAndLogin(page: Page, prefix: string): Promise<void> {
  const email = uniqueEmail(prefix);
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill("Scan User");
  await page.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
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

test.describe("Stocks — barcode scan", () => {
  test("a photo of a known code pre-fills the form, and scanning it again adds one", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-scan-known");
    await createGroup(page, "Famille Scan");

    // With JavaScript, choosing the photo sends it (`data-submit-on-change`).
    await page.goto("/stocks/new");
    await page.getByLabel("Photographier le code").setInputFiles({
      name: "code.png",
      mimeType: "image/png",
      buffer: ean13Photo(KNOWN),
    });
    await expect(page).toHaveURL(new RegExp(`/stocks/new\\?scan=${KNOWN}$`));
    await expect(page.getByText("Produit trouvé")).toBeVisible();
    await expect(page.getByLabel("Nom")).toHaveValue("Pâte à tartiner (400 g)");
    await expect(page.getByLabel("Quantité")).toHaveValue("1");
    await expect(page.getByLabel("Unité")).toHaveValue("unité");
    await expect(page.getByText(`Code-barres associé : ${KNOWN}`)).toBeVisible();
    await expect(page.getByText("Données produits : Open Food Facts, ODbL.")).toBeVisible();
    await page.getByRole("button", { name: "Ajouter l'article" }).click();
    await expect(page).toHaveURL(/\/stocks\?notice=item_created$/);

    // The same code again: the article already in stock, and "+1".
    await page.goto(`/stocks/new?scan=${KNOWN}`);
    await expect(page.getByRole("heading", { name: "Déjà en stock" })).toBeVisible();
    await expect(page.getByText("Déjà en stock : Pâte à tartiner (400 g) (1 unité)")).toBeVisible();
    await page.getByRole("button", { name: "+1" }).click();
    await expect(page).toHaveURL(/\/stocks\/[0-9a-f-]+\?notice=quantity_adjusted$/);
    await expect(page.getByText("Quantité : 2 unité")).toBeVisible();

    // "Créer quand même un autre article": the form, without the code.
    await page.goto(`/stocks/new?scan=${KNOWN}`);
    await page.getByRole("link", { name: "Créer quand même un autre article" }).click();
    await expect(page.getByLabel("Nom")).toHaveValue("Pâte à tartiner (400 g)");
    await expect(page.getByText("Cet article ne portera pas le code-barres")).toBeVisible();
    await expect(page.getByText("Code-barres associé")).toHaveCount(0);
  });

  test("a photo with no readable code offers to scan again, not the article form", async ({
    page,
  }) => {
    await registerAndLogin(page, "e2e-scan-blurred");
    await createGroup(page, "Famille Floue");

    await page.goto("/stocks/new");
    await page.getByLabel("Photographier le code").setInputFiles({
      name: "flou.png",
      mimeType: "image/png",
      buffer: BLANK_PHOTO,
    });
    await expect(page).toHaveURL(/\/stocks\/new\?photo=unreadable$/);
    await expect(page.getByText("Aucun code-barres n'a pu être lu sur la photo")).toBeVisible();
    await expect(page.getByLabel("Nom")).toHaveCount(0);
    await expect(page.getByRole("link", { name: "Saisir l'article sans code-barres" })).toBeVisible();

    // Taking the photo again, readable this time, relaunches the scan.
    await page.getByLabel("Photographier le code").setInputFiles({
      name: "net.png",
      mimeType: "image/png",
      buffer: ean13Photo(KNOWN),
    });
    await expect(page).toHaveURL(new RegExp(`/stocks/new\\?scan=${KNOWN}$`));
    await expect(page.getByLabel("Nom")).toHaveValue("Pâte à tartiner (400 g)");

    // Or the digits, typed: the scan runs on them.
    await page.goto("/stocks/new?photo=unreadable");
    await page.getByLabel("Code-barres").fill(UNKNOWN);
    await page.getByRole("button", { name: "Chercher le produit" }).click();
    await expect(page.getByText("Aucune fiche produit pour ce code")).toBeVisible();
    await expect(page.getByText(`Code-barres associé : ${UNKNOWN}`)).toBeVisible();
  });

  test("a store's weighing label gets the manual form with its code", async ({ page }) => {
    await registerAndLogin(page, "e2e-scan-weighed");
    await createGroup(page, "Famille Pesée");

    await page.goto(`/stocks/new?scan=${WEIGHED}`);
    await expect(page.getByText("Étiquette de pesée du magasin")).toBeVisible();
    await expect(page.getByLabel("Nom")).toHaveValue("");
    await expect(page.getByText(`Code-barres associé : ${WEIGHED}`)).toBeVisible();
    await expect(page.getByText("Données produits")).toHaveCount(0);
  });

  test("a GS1 code's date pre-fills the form, then a sooner one is proposed", async ({ page }) => {
    await registerAndLogin(page, "e2e-scan-gs1");
    await createGroup(page, "Famille Datamatrix");

    // (01) KNOWN as a GTIN-14, (10) a lot ended by FNC1 (ASCII 29), (17)
    // 2027-01-31: what a GS1 DataMatrix decodes to (#403).
    const gs1 = (yymmdd: string) => `010301762042200310LOT-42\u001d17${yymmdd}`;
    await page.goto(`/stocks/new?scan=${encodeURIComponent(gs1("270131"))}`);
    expect(page.url()).toContain("%1D17270131");
    await expect(page.getByText("Produit trouvé")).toBeVisible();
    await expect(page.getByLabel("Nom")).toHaveValue("Pâte à tartiner (400 g)");
    await expect(page.getByText(`Code-barres associé : ${KNOWN}`)).toBeVisible();
    await expect(page.getByLabel("Date de péremption")).toHaveValue("2027-01-31");
    await expect(page.getByText("Lue sur le code")).toBeVisible();
    // The member may correct it before saving.
    await page.getByLabel("Date de péremption").fill("2027-01-30");
    await page.getByRole("button", { name: "Ajouter l'article" }).click();
    await expect(page).toHaveURL(/\/stocks\?notice=item_created$/);

    // The same product, a sooner date: proposed, not written.
    await page.goto(`/stocks/new?scan=${encodeURIComponent(gs1("270115"))}`);
    await expect(page.getByRole("heading", { name: "Déjà en stock" })).toBeVisible();
    await expect(
      page.getByText(
        "Date de péremption lue sur le code : 15/01/2027, plus proche que celle de l'article (30/01/2027).",
      ),
    ).toBeVisible();
    await page.getByRole("link", { name: "Mettre cette date sur l'article" }).click();
    await expect(page).toHaveURL(/\/stocks\/[0-9a-f-]+\/edit\?expires_on=2027-01-15$/);
    await expect(page.getByLabel("Date de péremption")).toHaveValue("2027-01-15");
    await expect(page.getByText("Lue sur le code")).toBeVisible();
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page.getByText("Article mis à jour.")).toBeVisible();
    await expect(page.getByText("Péremption : 15/01/2027")).toBeVisible();
  });

  test("a string that is not a barcode gets the manual form and a message", async ({ page }) => {
    await registerAndLogin(page, "e2e-scan-invalid");
    await createGroup(page, "Famille Invalide");

    await page.goto("/stocks/new?scan=12345");
    await expect(page.getByText("Ce code-barres n'est pas reconnu")).toBeVisible();
    await expect(page.getByLabel("Code-barres")).toHaveValue("12345");
    await expect(page.getByText("Code-barres associé")).toHaveCount(0);
  });
});

test.describe("Stocks — barcode scan without JavaScript", () => {
  test.use({ javaScriptEnabled: false });

  test("the photo form and the Code-barres field both work, up to the +1", async ({ page }) => {
    await registerAndLogin(page, "e2e-scan-nojs");
    await createGroup(page, "Famille Sans JS");

    // The photo, sent by its button.
    await page.goto("/stocks/new");
    await page.getByLabel("Photographier le code").setInputFiles({
      name: "code.png",
      mimeType: "image/png",
      buffer: ean13Photo(KNOWN),
    });
    await page.getByRole("button", { name: "Scanner" }).click();
    await expect(page.getByLabel("Nom")).toHaveValue("Pâte à tartiner (400 g)");

    // The digits, typed, for a product the stand-in does not know.
    await page.goto("/stocks/new");
    await page.getByLabel("Code-barres").fill(UNKNOWN);
    await page.getByRole("button", { name: "Chercher le produit" }).click();
    await expect(page).toHaveURL(new RegExp(`/stocks/new\\?scan=${UNKNOWN}$`));
    await expect(page.getByText("Aucune fiche produit pour ce code")).toBeVisible();
    await page.getByLabel("Nom").fill("Surligneurs");
    await page.getByRole("button", { name: "Ajouter l'article" }).click();
    await expect(page).toHaveURL(/\/stocks\?notice=item_created$/);

    await page.goto("/stocks/new");
    await page.getByLabel("Code-barres").fill(UNKNOWN);
    await page.getByRole("button", { name: "Chercher le produit" }).click();
    await expect(page.getByText("Déjà en stock : Surligneurs (1 unité)")).toBeVisible();
    await page.getByRole("button", { name: "+1" }).click();
    await expect(page.getByText("Quantité : 2 unité")).toBeVisible();
  });
});
