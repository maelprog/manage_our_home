import { defineConfig } from "@playwright/test";

// Drives a real running stack (apps/web + apps/api + Postgres) — see
// README.md in this directory for how to bring one up (docker-compose, or
// `cargo run` for both crates against a local Postgres). No `webServer`
// auto-start here: standing up the whole stack (DB + migrations + both
// Rust binaries) isn't something Playwright's single-process webServer
// hook is a good fit for; CI/local runs are expected to start the stack
// first, then point WEB_BASE_URL/API_BASE_URL/DATABASE_URL at it.
const baseURL = process.env.WEB_BASE_URL ?? "http://localhost:3000";

// Behind infra/Caddyfile over HTTPS (#381, tests/caddy-https.spec.ts):
// - Caddy's certificate comes from its internal CA, which no browser trusts;
// - the full Chromium build rather than Playwright's default headless shell:
//   with @playwright/test 1.61.1, the shell kept the CSP report queued for
//   the whole 30 s of the test (queued, pending, queued again, no upload
//   attempted), where Chromium delivered it within a second.
const https = baseURL.startsWith("https://");

export default defineConfig({
  testDir: "./tests",
  fullyParallel: false,
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL,
    trace: "retain-on-failure",
    // The suite browses as a visitor who asked for reduced motion, which
    // turns off the page transition (#523: `@view-transition` is only asked
    // for under `prefers-reduced-motion: no-preference`). Headless Chromium
    // stops producing frames after a cross-document view transition: in the
    // pinned image (Playwright 1.61.1), headless shell and new headless
    // alike, `requestAnimationFrame` did not fire once in 8 s on the page a
    // link led to, and every click there waited for "stable" until the test
    // timed out. A spec that needs full motion emulates it itself. Through
    // `contextOptions`: `reducedMotion` is no fixture option of its own.
    contextOptions: { reducedMotion: "reduce" },
    ...(https && {
      ignoreHTTPSErrors: true,
      channel: "chromium",
    }),
  },
});
