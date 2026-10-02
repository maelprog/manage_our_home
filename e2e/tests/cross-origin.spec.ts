import { expect, test } from "@playwright/test";

// Issue #223: a page of another site submitting apps/web's login form (a
// login CSRF) is refused, in a real browser. The Rust tests set the
// headers by hand; only a browser shows which ones it really sends. On
// plain http to a host that is not localhost (the local docker recipe)
// Chromium sends no Sec-Fetch-Site and the refusal rests on Origin; on
// http://localhost (CI) it sends `Sec-Fetch-Site: cross-site`.

const ORIGIN = process.env.WEB_BASE_URL ?? "http://localhost:3000";
const FOREIGN = "http://attacker.invalid";

test.describe("Cross-origin requests (#223)", () => {
  test("a login form posted from another site is refused", async ({ page, context }) => {
    await page.route(`${FOREIGN}/**`, (route) =>
      route.fulfill({
        contentType: "text/html",
        body: `<!doctype html><form method="post" action="${ORIGIN}/login">
          <input name="email" value="attacker@example.test">
          <input name="password" value="attacker-password">
        </form><script>document.forms[0].submit()</script>`,
      }),
    );

    const answer = page.waitForResponse(
      (r) => r.url() === `${ORIGIN}/login` && r.request().method() === "POST",
    );
    await page.goto(`${FOREIGN}/`);
    const response = await answer;

    expect(response.status()).toBe(403);
    await expect(page.getByRole("heading", { name: "Envoi refusé" })).toBeVisible();
    expect(await context.cookies(ORIGIN)).toEqual([]);
  });
});
