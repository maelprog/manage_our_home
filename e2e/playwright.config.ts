import { defineConfig } from "@playwright/test";

// Drives a real running stack (apps/web + apps/api + Postgres) — see
// README.md in this directory for how to bring one up (docker-compose, or
// `cargo run` for both crates against a local Postgres). No `webServer`
// auto-start here: standing up the whole stack (DB + migrations + both
// Rust binaries) isn't something Playwright's single-process webServer
// hook is a good fit for; CI/local runs are expected to start the stack
// first, then point WEB_BASE_URL/API_BASE_URL/DATABASE_URL at it.
export default defineConfig({
  testDir: "./tests",
  fullyParallel: false,
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL: process.env.WEB_BASE_URL ?? "http://localhost:3000",
    trace: "retain-on-failure",
  },
});
