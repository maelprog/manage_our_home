import { expect, Page, test } from "@playwright/test";
import { cacheOffProduct, fetchVerificationToken } from "../lib/db";

// Issue #402 — scanning an article's barcode on /stocks/new. Open Food Facts
// is never reached: each code a test scans is either put in apps/api's cache
// first (`cacheOffProduct`), a store's weighing label (never sent), already
// in the family's stock (no product needed), or not a code at all.
//
// The camera itself cannot be driven from here: headless Chromium has no
// camera to point at a barcode. What is tested instead is everything around
// it — the `scan` parameter the button navigates to, the no-JavaScript field
// that sends the same parameter, and the vendored decoder, fed an EAN-13
// drawn on a canvas.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-scan-password-1";

const KNOWN = "3017620422003";
const UNKNOWN = "4006381333931";
const WEIGHED = "2123456012347";

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
  test("a known code pre-fills the form, and scanning it again adds one", async ({ page }) => {
    await cacheOffProduct(KNOWN, "Pâte à tartiner", "400 g");
    await registerAndLogin(page, "e2e-scan-known");
    await createGroup(page, "Famille Scan");

    await page.goto(`/stocks/new?scan=${KNOWN}`);
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

  test("a store's weighing label gets the manual form with its code", async ({ page }) => {
    await registerAndLogin(page, "e2e-scan-weighed");
    await createGroup(page, "Famille Pesée");

    await page.goto(`/stocks/new?scan=${WEIGHED}`);
    await expect(page.getByText("Étiquette de pesée du magasin")).toBeVisible();
    await expect(page.getByLabel("Nom")).toHaveValue("");
    await expect(page.getByText(`Code-barres associé : ${WEIGHED}`)).toBeVisible();
    await expect(page.getByText("Données produits")).toHaveCount(0);
  });

  test("a string that is not a barcode gets the manual form and a message", async ({ page }) => {
    await registerAndLogin(page, "e2e-scan-invalid");
    await createGroup(page, "Famille Invalide");

    await page.goto("/stocks/new?scan=12345");
    await expect(page.getByText("Ce code-barres n'est pas reconnu")).toBeVisible();
    await expect(page.getByLabel("Code-barres")).toHaveValue("12345");
    await expect(page.getByText("Code-barres associé")).toHaveCount(0);
  });

  test("the scanner's decoder reads an EAN-13, from files the site serves", async ({ page }) => {
    await registerAndLogin(page, "e2e-scan-decoder");
    await createGroup(page, "Famille Décodeur");
    await page.goto("/stocks/new");
    // With JavaScript and a camera API, the button is revealed. Browsers
    // expose the camera API to secure contexts only: `localhost` (CI) is
    // one, a plain-HTTP host name is not, and there the button stays hidden.
    const secure = await page.evaluate(() => window.isSecureContext);
    await expect(page.getByRole("button", { name: "Scanner" })).toHaveCount(secure ? 1 : 0);
    if (secure) await expect(page.getByRole("button", { name: "Scanner" })).toBeVisible();

    const origin = new URL(page.url()).origin;
    const requested: string[] = [];
    page.on("request", (request) => requested.push(request.url()));

    const decoded = await page.evaluate(async (digits: string) => {
      const root = document.querySelector("[data-scan]");
      if (!root) throw new Error("no scan block");
      await new Promise((resolve, reject) => {
        const script = document.createElement("script");
        script.src = root.getAttribute("data-scan-polyfill") ?? "";
        script.addEventListener("load", resolve);
        script.addEventListener("error", reject);
        document.head.appendChild(script);
      });
      const api = (window as unknown as { BarcodeDetectionAPI: any }).BarcodeDetectionAPI;
      const wasm = root.getAttribute("data-scan-wasm");
      api.setZXingModuleOverrides({ locateFile: () => wasm });

      // An EAN-13 drawn by hand: start guard, six digits in L/G parity set
      // by the first digit, centre guard, six in R, end guard.
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
      const quiet = 12 * module;
      const canvas = document.createElement("canvas");
      canvas.width = bits.length * module + 2 * quiet;
      canvas.height = 200;
      const ctx = canvas.getContext("2d");
      if (!ctx) throw new Error("no 2d context");
      ctx.fillStyle = "#fff";
      ctx.fillRect(0, 0, canvas.width, canvas.height);
      ctx.fillStyle = "#000";
      for (let i = 0; i < bits.length; i++) {
        if (bits[i] === "1") ctx.fillRect(quiet + i * module, 20, module, 160);
      }
      const detector = new api.BarcodeDetector({ formats: ["ean_13"] });
      const codes = await detector.detect(canvas);
      return codes.map((c: { rawValue: string }) => c.rawValue);
    }, KNOWN);

    expect(decoded).toEqual([KNOWN]);
    expect(requested.some((url) => url.endsWith(".wasm"))).toBe(true);
    expect(requested.filter((url) => !url.startsWith(origin))).toEqual([]);
  });
});

test.describe("Stocks — barcode scan without JavaScript", () => {
  test.use({ javaScriptEnabled: false });

  test("the Code-barres field leads to the manual form, then to the article in stock", async ({
    page,
  }) => {
    await cacheOffProduct(UNKNOWN, null, null);
    await registerAndLogin(page, "e2e-scan-nojs");
    await createGroup(page, "Famille Sans JS");

    await page.goto("/stocks/new");
    // No script, no camera button: the field is the way in.
    await expect(page.getByRole("button", { name: "Scanner" })).toHaveCount(0);
    await page.getByLabel("Code-barres").fill(UNKNOWN);
    await page.getByRole("button", { name: "Chercher le produit" }).click();
    await expect(page).toHaveURL(new RegExp(`/stocks/new\\?scan=${UNKNOWN}$`));
    await expect(page.getByText("Aucune fiche produit pour ce code")).toBeVisible();
    await expect(page.getByText(`Code-barres associé : ${UNKNOWN}`)).toBeVisible();
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
