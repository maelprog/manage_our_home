import { expect, test } from "@playwright/test";

// Issue #67's "Vérification": cut the outside world and the fonts must
// still render, with no request leaving for a third-party domain. The Rust
// guards in apps/web assert that the stylesheet says the right thing; only
// a browser can show that the bytes actually arrive and that nothing else
// is fetched on the side.

const ORIGIN = process.env.WEB_BASE_URL ?? "http://localhost:3000";

test.describe("Self-hosted fonts (#67)", () => {
  test("a page loads its fonts from apps/web and contacts nobody else", async ({ page }) => {
    const foreign: string[] = [];
    const fontRequests: string[] = [];
    page.on("request", (request) => {
      const url = request.url();
      if (!url.startsWith(ORIGIN)) {
        foreign.push(url);
      } else if (url.includes("/assets/fonts/")) {
        fontRequests.push(new URL(url).pathname);
      }
    });

    // /login is the first page a visitor sees, and it needs no session.
    await page.goto("/login");
    await page.waitForLoadState("networkidle");

    expect(
      foreign,
      "a page view must not reach a third-party origin — that is the visitor's " +
        "IP handed to someone else, and a processing activity to declare",
    ).toEqual([]);

    // Both faces are on the page: Source Sans 3 on the body, Fraunces on the
    // h1. `document.fonts` reports the real outcome — a 404 with
    // `font-display: swap` renders the fallback and looks perfectly fine.
    const loaded = await page.evaluate(async () => {
      await document.fonts.ready;
      return {
        body: document.fonts.check('1rem "Source Sans 3"'),
        heading: document.fonts.check('1rem "Fraunces"'),
      };
    });
    expect(loaded).toEqual({ body: true, heading: true });
    expect(fontRequests.length).toBeGreaterThanOrEqual(2);
  });

  test("every font the stylesheet names is served, cached for a year", async ({ page }) => {
    await page.goto("/login");
    // Since #89 the sheet is linked, not inlined, so the font URLs are no
    // longer in the document — they are in the sheet the document points at.
    // Following the link is also what checks the link works at all.
    const href = await page.getAttribute('link[rel="stylesheet"]', "href");
    expect(href, "the page must link a stylesheet").toBeTruthy();
    const sheet = await page.request.get(href as string);
    expect(sheet.status(), href as string).toBe(200);
    const css = await sheet.text();
    const files = [...css.matchAll(/\/assets\/fonts\/[\w.-]+\.woff2/g)].map((m) => m[0]);
    expect(new Set(files).size, `stylesheet font URLs: ${files}`).toBe(2);

    for (const path of new Set(files)) {
      const response = await page.request.get(path);
      expect(response.status(), path).toBe(200);
      // Without this the browser re-fetches the font on every navigation,
      // which is most of the reason to serve it ourselves.
      expect(response.headers()["cache-control"], path).toBe(
        "public, max-age=31536000, immutable",
      );
    }
  });

  test("the stylesheet is content-addressed and cached for a year (#89)", async ({ page }) => {
    // The half of #89 only a browser can show: the page really does fetch a
    // sheet, under a name made of that sheet's own digest, and really is told
    // never to ask for it again. Take away the cache header and the switch out
    // of inlining has kept every cost and bought nothing.
    await page.goto("/login");
    const href = await page.getAttribute('link[rel="stylesheet"]', "href");
    expect(href).toMatch(/^\/assets\/style-[0-9a-f]{16}\.css$/);
    const sheet = await page.request.get(href as string);
    expect(sheet.status()).toBe(200);
    expect(sheet.headers()["content-type"]).toContain("text/css");
    expect(sheet.headers()["cache-control"]).toBe("public, max-age=31536000, immutable");

    // And the document itself no longer carries a copy of it.
    expect(await page.content()).not.toContain("<style>");
  });

  test("the asset route does not serve the rest of the repository", async ({ page }) => {
    for (const path of ["/assets/fonts/nope.woff2", "/assets/../src/style.css"]) {
      expect((await page.request.get(path)).status(), path).not.toBe(200);
    }
  });
});
