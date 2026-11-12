import { expect, Page, Request, Route, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";
import { parisDayOfMonth } from "../lib/dates";

// Issue #778: a form being sent puts its button in waiting (`enhance.js`):
// `aria-busy`, the label of its `data-busy-label` or « Envoi en cours »,
// disabled, a wheel drawn by the sheet, and no second sending. A page the
// back button restores from the browser's cache gets its button back
// (`pageshow`).
//
// Each post under test is held by `page.route` for a while, then let
// through. While it is held, the page has a navigation pending, and
// Playwright answers nothing about it — locators, `page.evaluate`, even a
// DevTools session wait for the navigation to end. The page still runs, so
// it watches itself (`watch`): once its form is sent, it writes down what
// its button looks like and tries to send the form again; the next page
// reads that note from sessionStorage.
//
// The back button has to restore a page from the back/forward cache for
// `pageshow` to have anything to do, and Playwright launches Chromium with
// `--disable-back-forward-cache`: this file leaves that argument out, on
// the full Chromium build, as page-transition.spec.ts does (the headless
// shell keeps no such cache). Top-level, because both options start a
// browser of their own. The suite's `reducedMotion: "reduce"` stays, so no
// page transition is asked for.
test.use({
  channel: "chromium",
  launchOptions: { ignoreDefaultArgs: ["--disable-back-forward-cache"] },
});

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-busy-password-1";

/** Registers and verifies a fresh account, without logging in; returns its email. */
async function register(page: Page): Promise<string> {
  const email = uniqueEmail("e2e-busy");
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill("Attente Envoi");
  await page.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
  await page.getByRole("textbox", { name: "Confirmation" }).fill(PASSWORD);
  await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
  await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
  await page.getByRole("button", { name: "Créer mon compte" }).click();
  await expect(page).toHaveURL(/\/register\/check-email$/);
  const token = await fetchVerificationToken(email);
  await page.goto(`/verify-email?token=${token}`);
  return email;
}

/**
 * Holds every POST whose URL matches until `release` is called, and lets
 * the later ones through; `posts` counts them.
 */
async function holdPosts(
  page: Page,
  url: RegExp,
): Promise<{ posts: () => number; release: () => Promise<void> }> {
  let count = 0;
  let open!: () => void;
  const gate = new Promise<void>((resolve) => (open = resolve));
  const handler = async (route: Route, request: Request) => {
    if (request.method() !== "POST") return route.fallback();
    count += 1;
    await gate;
    await route.continue();
  };
  await page.route(url, handler);
  return {
    posts: () => count,
    // The route stays: unrouting would drop the post it still holds.
    release: async () => open(),
  };
}

interface Seen {
  busy: string | null;
  disabled: boolean;
  label: string;
  named: boolean;
}

const SEEN = "busy-submit-seen";

/**
 * From the next page on: once a form holding `selector` is sent, the page
 * notes its button 300 ms later, then sends the form again.
 */
async function watch(page: Page, selector: string): Promise<void> {
  await page.addInitScript(
    ([sel, key]) => {
      addEventListener("submit", (event) => {
        const form = event.target as HTMLFormElement;
        const button = form.querySelector(sel) as HTMLButtonElement | null;
        if (!button || sessionStorage.getItem(key)) return;
        setTimeout(() => {
          const seen = {
            busy: button.getAttribute("aria-busy"),
            disabled: button.disabled,
            label: button.textContent,
            named: button.hasAttribute("aria-label") || button.hasAttribute("aria-labelledby"),
          };
          sessionStorage.setItem(key, JSON.stringify(seen));
          form.requestSubmit();
        }, 300);
      });
    },
    [selector, SEEN],
  );
}

async function seen(page: Page): Promise<Seen | null> {
  return page.evaluate((key) => JSON.parse(sessionStorage.getItem(key) ?? "null"), SEEN);
}

/** WCAG contrast ratio of two computed `rgb(…)` colours. */
function contrast(a: string, b: string): number {
  const luminance = (css: string) => {
    const [r, g, bl] = (css.match(/[\d.]+/g) ?? []).slice(0, 3).map(Number);
    const lin = (c: number) => {
      const s = c / 255;
      return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
    };
    return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(bl);
  };
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

const LOGIN_BUTTON = 'form[action="/login"] button[type="submit"]';

test("logging in: the button waits, the form is not sent twice, then the home page", async ({ page }) => {
  const email = await register(page);
  await watch(page, LOGIN_BUTTON);
  await page.goto("/login");
  await page.getByLabel("Email").fill(email);
  await page.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
  const held = await holdPosts(page, /\/login$/);
  await expect(page.locator(LOGIN_BUTTON)).not.toHaveAttribute("aria-busy");
  // Not awaited: the click waits for the navigation, which waits for us.
  const click = page.locator(LOGIN_BUTTON).click();
  await expect.poll(() => held.posts()).toBe(1);
  await page.waitForTimeout(1000);
  await held.release();
  await click;
  await expect(page).toHaveURL("/");

  // Its own label, from `data-busy-label`, which is also its name (no
  // `aria-label` over it); disabled; and the second sending stopped.
  expect(await seen(page)).toEqual({
    busy: "true",
    disabled: true,
    label: "Connexion en cours",
    named: false,
  });
  expect(held.posts()).toBe(1);
});

test("the wheel: the button's own colour, both themes, frozen under reduced motion", async ({ page }) => {
  await page.goto("/login");
  const button = page.locator(LOGIN_BUTTON);
  await button.evaluate((node) => node.setAttribute("aria-busy", "true"));
  const wheel = () =>
    button.evaluate((node) => {
      const before = getComputedStyle(node, "::before");
      const own = getComputedStyle(node);
      return {
        content: before.content,
        animation: before.animationName,
        ring: before.borderTopColor,
        text: own.color,
        ground: own.backgroundColor,
      };
    });
  const grounds = new Set<string>();
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme, reducedMotion: "reduce" });
    const frozen = await wheel();
    expect(frozen.content).toBe('""');
    expect(frozen.animation).toBe("none");
    expect(frozen.ring).toBe(frozen.text);
    // WCAG 1.4.11 asks 3:1 of a graphic; it is the label's own pair.
    expect(contrast(frozen.ring, frozen.ground)).toBeGreaterThanOrEqual(4.5);
    grounds.add(frozen.ground);
    await page.emulateMedia({ colorScheme: scheme, reducedMotion: "no-preference" });
    expect((await wheel()).animation).toBe("busy-spin");
  }
  // Both themes were painted.
  expect(grounds.size).toBe(2);
});

const PNG_1X1 = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
  "base64",
);

const UPLOAD_BUTTON = 'form[enctype="multipart/form-data"] button[type="submit"]';

test("an attachment: the button waits, the next page comes, the back button gives it back", async ({ page }) => {
  const email = await register(page);
  await page.goto("/login");
  await page.getByLabel("Email").fill(email);
  await page.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
  await page.getByRole("button", { name: "Se connecter" }).click();
  await expect(page).toHaveURL("/");
  await page.goto("/groups/new");
  await page.getByLabel("Nom du groupe").fill("Famille Attente");
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
  const date = parisDayOfMonth(21);
  await page.goto("/agenda/new");
  await page.getByLabel("Titre").fill("Envoi lent");
  await page.getByLabel("Début").fill(`${date}T09:00`);
  await page.getByLabel("Fin").fill(`${date}T10:00`);
  await page.getByRole("button", { name: "Créer l'événement" }).click();
  await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);
  await page.getByRole("link", { name: /Envoi lent/ }).first().click();
  await expect(page.getByRole("heading", { name: "Envoi lent" })).toBeVisible();
  const detail = page.url();

  // Whether the page in view came out of the back/forward cache.
  await page.addInitScript(() => {
    addEventListener("pageshow", (event) => {
      (window as unknown as { restored: boolean }).restored = event.persisted;
    });
  });
  await watch(page, UPLOAD_BUTTON);
  await page.reload();

  await page.locator('input[name="file"]').setInputFiles({
    name: "lent.png",
    mimeType: "image/png",
    buffer: PNG_1X1,
  });
  const held = await holdPosts(page, /\/attachments(\?|$)/);
  const click = page.locator(UPLOAD_BUTTON).click();
  await expect.poll(() => held.posts()).toBe(1);
  await page.waitForTimeout(1000);
  await held.release();
  await click;
  await expect(page.getByText("Pièce jointe ajoutée.")).toBeVisible();
  // No `data-busy-label` here: the default.
  expect(await seen(page)).toEqual({ busy: "true", disabled: true, label: "Envoi en cours", named: false });
  expect(held.posts()).toBe(1);

  // Back: the page left waiting comes out of the cache as it was left,
  // and `pageshow` gives its button back.
  // `commit`: a page restored from the cache fires no `load` again.
  await page.goBack({ waitUntil: "commit" });
  await expect(page).toHaveURL(detail);
  await expect
    .poll(() => page.evaluate(() => (window as unknown as { restored?: boolean }).restored))
    .toBe(true);
  const button = page.locator(UPLOAD_BUTTON);
  await expect(button).toBeEnabled();
  await expect(button).not.toHaveAttribute("aria-busy");
  await expect(button).toHaveText("Envoyer");

  // A refused `data-confirm` leaves the viewer's « Supprimer » as it was;
  // the viewer's « Fermer », a `<form method="dialog">`, never waits.
  await page.reload();
  await page.getByRole("link", { name: "lent.png" }).click();
  const viewer = page.getByRole("dialog", { name: "lent.png" });
  await viewer.getByRole("button", { name: "Fermer" }).click();
  await expect(viewer).toBeHidden();
  await page.getByRole("link", { name: "lent.png" }).click();
  const close = viewer.getByRole("button", { name: "Fermer" });
  await expect(close).toBeEnabled();
  await expect(close).not.toHaveAttribute("aria-busy");
  page.once("dialog", (dialog) => dialog.dismiss());
  const remove = viewer.getByRole("button", { name: "Supprimer" });
  await remove.click();
  await expect(remove).toBeEnabled();
  await expect(remove).not.toHaveAttribute("aria-busy");
  page.once("dialog", (dialog) => dialog.accept());
  await remove.click();
  await expect(page.getByRole("link", { name: "lent.png" })).toHaveCount(0);
});
