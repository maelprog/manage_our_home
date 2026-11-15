import { expect, Page, test } from "@playwright/test";
import { fetchVerificationToken, seedMessageNotification } from "../lib/db";

// Issue #861: a notification opened is read at once, and the back button
// shows it so, without a manual reload. `/notifications` is sent
// `no-store`, so the browser fetches it again rather than take it from its
// HTTP cache; a page the back/forward cache restores, shown before the
// opening, is fetched again by `enhance.js` (`pageshow`), whatever its
// path, since every page wears the count.
//
// Playwright launches Chromium with `--disable-back-forward-cache`: this
// file leaves that argument out, on the full Chromium build, as
// busy-submit.spec.ts does (the headless shell keeps no such cache), so the
// second test does go through that cache. Top-level, because both options
// start a browser of their own.
test.use({
  viewport: { width: 1440, height: 900 },
  channel: "chromium",
  launchOptions: { ignoreDefaultArgs: ["--disable-back-forward-cache"] },
});

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-back-password-1";

/**
 * Avoids every form label of the pages under test (`getByLabel` matches
 * substrings, the family switcher included).
 */
const FAMILY = "Foyer Retour";

/** A verified account, logged in, in a family of its own; returns its email. */
async function signUpWithFamily(page: Page): Promise<string> {
  const email = uniqueEmail("e2e-back");
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill("Camille");
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
  await page.goto("/groups/new");
  await page.getByLabel("Nom du groupe").fill(FAMILY);
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
  return email;
}

/** The sidebar's Notifications entry, by its accessible name and count. */
const entry = (page: Page, name: string) =>
  page.getByRole("navigation", { name: "Navigation principale" }).getByRole("link", { name, exact: true });

async function twoUnread(page: Page): Promise<void> {
  const email = await signUpWithFamily(page);
  await seedMessageNotification(email, FAMILY, "Sam a écrit dans la messagerie.");
  await seedMessageNotification(email, FAMILY, "Lou a écrit dans la messagerie.");
}

test.describe("Back after opening a notification", () => {
  test("the list comes back with the entry read and the count down", async ({ page }) => {
    await twoUnread(page);
    const res = await page.goto("/notifications");
    expect(res!.headers()["cache-control"]).toBe("no-store");
    await expect(entry(page, "Notifications (2 non lues)")).toBeVisible();
    const sam = page.getByRole("button", { name: /Sam a écrit/ });
    await expect(sam).toContainText("Non lue");

    await sam.click();
    await expect(page).toHaveURL("/messagerie");
    await expect(entry(page, "Notifications (1 non lue)")).toBeVisible();

    // `commit`: a page restored from the back/forward cache fires no
    // `load` again.
    await page.goBack({ waitUntil: "commit" });
    await expect(page).toHaveURL("/notifications");
    await expect(page.locator("hgroup .eyebrow")).toHaveText("1 non lue");
    await expect(page.getByRole("button", { name: /Sam a écrit/ })).not.toContainText("Non lue");
    await expect(page.getByRole("button", { name: /Lou a écrit/ })).toContainText("Non lue");
    await expect(entry(page, "Notifications (1 non lue)")).toBeVisible();
  });

  test("a page the back/forward cache restores wears the new count", async ({ page }) => {
    await twoUnread(page);
    await page.goto("/stocks");
    await expect(entry(page, "Notifications (2 non lues)")).toBeVisible();
    // The proof that the back button below does restore this document
    // from the back/forward cache, kept where the reload leaves it.
    await page.evaluate(() =>
      window.addEventListener("pageshow", (event) => {
        if (event.persisted) sessionStorage.setItem("e2e-restored", "stocks");
      }),
    );

    await page.goto("/notifications");
    await page.getByRole("button", { name: /Sam a écrit/ }).click();
    await expect(page).toHaveURL("/messagerie");

    // Back past the list to the stocks, shown before the opening.
    await page.goBack({ waitUntil: "commit" });
    await expect(page).toHaveURL("/notifications");
    await page.goBack({ waitUntil: "commit" });
    await expect(page).toHaveURL("/stocks");
    await expect(entry(page, "Notifications (1 non lue)")).toBeVisible();
    expect(await page.evaluate(() => sessionStorage.getItem("e2e-restored"))).toBe("stocks");
  });

  test("a page shown after the last opening comes back from the cache untouched", async ({ page }) => {
    await twoUnread(page);
    await page.goto("/notifications");
    await page.getByRole("button", { name: /Sam a écrit/ }).click();
    await expect(page).toHaveURL("/messagerie");

    // Shown after the opening: nothing of it is stale.
    await page.goto("/stocks");
    await expect(entry(page, "Notifications (1 non lue)")).toBeVisible();
    await page.evaluate(() => {
      const w = window as unknown as { kept?: boolean; restored?: boolean };
      w.kept = true;
      window.addEventListener("pageshow", (event) => {
        if (event.persisted) w.restored = true;
      });
    });

    await page.goto("/agenda");
    await page.goBack({ waitUntil: "commit" });
    await expect(page).toHaveURL("/stocks");
    // Restored from the back/forward cache, and not reloaded: the
    // document is the one left, its `window` with it.
    await expect
      .poll(() => page.evaluate(() => (window as unknown as { restored?: boolean }).restored))
      .toBe(true);
    // A reload asked at `pageshow` would have come by now and taken both.
    await page.waitForTimeout(1000);
    expect(
      await page.evaluate(() => {
        const w = window as unknown as { kept?: boolean; restored?: boolean };
        return w.kept === true && w.restored === true;
      }),
    ).toBe(true);
  });
});
