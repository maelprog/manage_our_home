import { readFileSync } from "node:fs";
import { expect, test } from "@playwright/test";

// Issue #381 — the application behind infra/Caddyfile, over HTTPS, as a
// browser meets it once deployed. Every other spec drives apps/web
// directly; the security headers Caddy sets and the CSP's reporting were
// exercised nowhere before this one. CI's `e2e` job runs it with
// WEB_BASE_URL=https://localhost, behind the Caddyfile with its own local
// certificate (Caddy issues one from its internal CA for `localhost`).
//
// Over plain HTTP there is nothing to check here: the HSTS header is
// ignored, and browsers send no CSP report from a page that is not a
// secure context — hence the skip.

const BASE = process.env.WEB_BASE_URL ?? "";
const HTTPS = BASE.startsWith("https://");

/** The policy as infra/Caddyfile writes it, on its one uncommented line. */
function caddyfilePolicy(): string {
  const caddyfile = readFileSync(new URL("../../infra/Caddyfile", import.meta.url), "utf8");
  const line = caddyfile
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l.startsWith("Content-Security-Policy "));
  expect(line, "one Content-Security-Policy line in infra/Caddyfile").toHaveLength(1);
  return /^Content-Security-Policy "([^"]+)"$/.exec(line[0])![1];
}

test.describe("Behind Caddy over HTTPS (#381)", () => {
  test.skip(!HTTPS, "needs the stack behind infra/Caddyfile over HTTPS (WEB_BASE_URL=https://…)");

  test("apps/web's and apps/api's responses carry the security headers", async ({
    page,
    request,
  }) => {
    const document = await page.goto("/login");
    expect(document).not.toBeNull();
    // What the Reporting API and `__Host-` cookies require of the page.
    expect(await page.evaluate(() => window.isSecureContext)).toBe(true);
    // apps/api's answer, through Caddy's `/api/*` route.
    const api = await request.get("/api/auth/me");
    expect(api.status()).toBe(401);

    const policy = caddyfilePolicy();
    for (const [what, headers] of [
      ["apps/web /login", document!.headers()],
      ["apps/api /auth/me", api.headers()],
    ] as const) {
      // The policy Caddy sends is the one the file writes, unedited.
      expect(headers["content-security-policy"], what).toBe(policy);
      expect(headers["reporting-endpoints"], what).toBe('csp="/api/csp-report"');
      expect(headers["strict-transport-security"], what).toBe("max-age=31536000");
      expect(headers["x-content-type-options"], what).toBe("nosniff");
      expect(headers["x-frame-options"], what).toBe("DENY");
      expect(headers["referrer-policy"], what).toBe("no-referrer");
    }
    // A few directives pinned here too, independently of the file: what
    // the browser enforces is this header, not apps/web/src/csp.rs's
    // reading of the Caddyfile.
    const directives = policy.split(";").map((d) => d.trim());
    for (const directive of [
      "default-src 'self'",
      "script-src 'self'",
      "frame-ancestors 'none'",
      "report-to csp",
    ]) {
      expect(directives, directive).toContain(directive);
    }
  });

  test("a CSP violation reaches /api/csp-report through Reporting-Endpoints", async ({
    page,
    context,
  }) => {
    // The Reporting API's own view of the upload, from Chromium's DevTools
    // protocol: which endpoint the report went to, and whether it was
    // delivered (a 2xx from the endpoint) — the relative URL of
    // `Reporting-Endpoints` resolved against the page.
    const cdp = await context.newCDPSession(page);
    type Report = { type: string; destination: string; status: string; body: Record<string, unknown> };
    const reports: Report[] = [];
    const record = ({ report }: { report: Report }) => reports.push(report);
    cdp.on("Network.reportingApiReportAdded", record);
    cdp.on("Network.reportingApiReportUpdated", record);
    const endpoints: { origin: string; endpoints: { url: string; groupName: string }[] }[] = [];
    cdp.on("Network.reportingApiEndpointsChangedForOrigin", (e) => endpoints.push(e));
    await cdp.send("Network.enable");
    await cdp.send("Network.enableReportingApi", { enable: true });

    await page.goto("/login");
    const origin = new URL(page.url()).origin;
    // An image from a host of its own: `default-src 'self'` refuses it, and
    // the host, unlike a query string, survives apps/api's redaction of
    // the logged URL — it is what finds this report among the others.
    const marker = `csp-probe-${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
    const blocked = `https://${marker}.invalid/pixel.png`;
    const violation = await page.evaluate(
      (src) =>
        new Promise<{ directive: string; blocked: string }>((resolve) => {
          document.addEventListener(
            "securitypolicyviolation",
            (e) => resolve({ directive: e.effectiveDirective, blocked: e.blockedURI }),
            { once: true },
          );
          const img = document.createElement("img");
          img.src = src;
          document.body.append(img);
        }),
      blocked,
    );
    expect(violation).toEqual({ directive: "img-src", blocked });

    await expect
      .poll(
        () =>
          reports.some(
            (r) =>
              r.type === "csp-violation" &&
              r.destination === "csp" &&
              r.status === "Success" &&
              r.body.blockedURL === blocked,
          ),
        { message: "the report was delivered to the `csp` endpoint", timeout: 30_000 },
      )
      .toBe(true);
    expect(endpoints).toContainEqual({
      origin,
      endpoints: [expect.objectContaining({ groupName: "csp", url: `${origin}/api/csp-report` })],
    });

    // And apps/api read it: its log line for this violation. The log is
    // the API's standard output, which the CI job writes to API_LOG_PATH.
    const log = process.env.API_LOG_PATH;
    expect(log, "API_LOG_PATH names apps/api's log file").toBeTruthy();
    await expect
      .poll(
        () =>
          readFileSync(log!, "utf8")
            .split("\n")
            .filter((l) => l.includes("content security policy violation reported"))
            .filter((l) => l.includes(blocked)).length,
        { message: "apps/api logged the violation", timeout: 10_000 },
      )
      .toBe(1);
  });
});
