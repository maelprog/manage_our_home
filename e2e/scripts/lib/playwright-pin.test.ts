import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  CI_PATH,
  LOCK_PATH,
  playwrightPinViolations,
  type SourceFile,
} from "./playwright-pin.ts";

// ---------------------------------------------------------------------------
// Image Playwright du job `e2e` alignée sur `@playwright/test` (#610).
//
// Le job ne lance plus `npx playwright install --with-deps chromium` : la
// suite tourne dans `mcr.microsoft.com/playwright`, dont les navigateurs sont
// ceux d'UNE version de Playwright. Une image et un paquet npm désaccordés
// cherchent un build de Chromium que l'image n'a pas. Cette porte lit les deux
// fichiers et refuse tout écart, en plus d'exiger le digest.
// ---------------------------------------------------------------------------

const REPO = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");

function real(path: string): SourceFile {
  return { path, text: readFileSync(join(REPO, path), "utf8") };
}

const DIGEST = "@sha256:" + "a".repeat(64);
const OTHER_DIGEST = "@sha256:" + "b".repeat(64);

function ci(...refs: string[]): SourceFile {
  return {
    path: CI_PATH,
    text: refs
      .map((ref) => `        run: docker run --rm --network host ${ref} npx playwright test\n`)
      .join(""),
  };
}

function lock(test: string | null, core = test, pw = test): SourceFile {
  const packages: Record<string, { version: string }> = {};
  if (test !== null) packages["node_modules/@playwright/test"] = { version: test };
  if (core !== null) packages["node_modules/playwright-core"] = { version: core };
  if (pw !== null) packages["node_modules/playwright"] = { version: pw };
  return { path: LOCK_PATH, text: JSON.stringify({ packages }) };
}

const OK_REF = `mcr.microsoft.com/playwright:v1.61.1-noble${DIGEST}`;

test("les vrais ci.yml et package-lock.json sont alignés", () => {
  assert.deepEqual(playwrightPinViolations(real(CI_PATH), real(LOCK_PATH)), []);
});

test("une image à la version du paquet, épinglée par digest, passe", () => {
  assert.deepEqual(playwrightPinViolations(ci(OK_REF), lock("1.61.1")), []);
});

test("refuse une image d'une autre version que @playwright/test", () => {
  const v = playwrightPinViolations(ci(OK_REF), lock("1.62.0"));
  assert.equal(v.length, 3);
  assert.match(v[0], /v1\.61\.1-noble/);
  assert.match(v[0], /1\.62\.0/);
});

test("refuse un playwright-core resté derrière @playwright/test", () => {
  const v = playwrightPinViolations(ci(OK_REF), lock("1.61.1", "1.60.0"));
  assert.equal(v.length, 1);
  assert.match(v[0], /playwright-core/);
  assert.match(v[0], /1\.60\.0/);
});

test("exige le digest derrière le tag", () => {
  const v = playwrightPinViolations(
    ci("mcr.microsoft.com/playwright:v1.61.1-noble"),
    lock("1.61.1"),
  );
  assert.equal(v.length, 1);
  assert.match(v[0], /digest/);
});

test("refuse un digest mal formé", () => {
  const v = playwrightPinViolations(
    ci("mcr.microsoft.com/playwright:v1.61.1-noble@sha256:zz"),
    lock("1.61.1"),
  );
  assert.equal(v.length, 1);
  assert.match(v[0], /digest/);
});

test("refuse un tag sans version complète", () => {
  for (const tag of ["latest", "noble", "v1.61-noble", "1.61.1-noble"]) {
    const v = playwrightPinViolations(
      ci(`mcr.microsoft.com/playwright:${tag}${DIGEST}`),
      lock("1.61.1"),
    );
    assert.equal(v.length, 1, tag);
    assert.match(v[0], new RegExp(tag.replace(/\./g, "\\.")), tag);
  }
});

test("refuse une image sans tag", () => {
  const v = playwrightPinViolations(
    ci(`mcr.microsoft.com/playwright${DIGEST}`),
    lock("1.61.1"),
  );
  assert.equal(v.length, 1);
});

test("refuse deux références au même tag sur des digests différents", () => {
  const v = playwrightPinViolations(
    ci(OK_REF, `mcr.microsoft.com/playwright:v1.61.1-noble${OTHER_DIGEST}`),
    lock("1.61.1"),
  );
  assert.equal(v.length, 1);
  assert.match(v[0], /divergent/);
});

test("refuse un ci.yml où l'image n'est plus trouvée", () => {
  const v = playwrightPinViolations(
    { path: CI_PATH, text: "      - run: npx playwright install chromium\n" },
    lock("1.61.1"),
  );
  assert.equal(v.length, 1);
  assert.match(v[0], /aucune/);
});

test("ignore les lignes de commentaire, qui ne comptent pas non plus comme référence", () => {
  const commented = {
    path: CI_PATH,
    text: "      # mcr.microsoft.com/playwright:v1.48.0-noble, l'ancienne\n",
  };
  const v = playwrightPinViolations(commented, lock("1.61.1"));
  assert.equal(v.length, 1);
  assert.match(v[0], /aucune/);
  const both = { path: CI_PATH, text: commented.text + ci(OK_REF).text };
  assert.deepEqual(playwrightPinViolations(both, lock("1.61.1")), []);
});

test("ne confond pas l'image du serveur MCP avec celle de la suite", () => {
  const text = ci(OK_REF).text + "  IMAGE=mcr.microsoft.com/playwright/mcp:v0.0.80\n";
  assert.deepEqual(
    playwrightPinViolations({ path: CI_PATH, text }, lock("1.61.1")),
    [],
  );
});

test("refuse un package-lock.json sans @playwright/test", () => {
  const v = playwrightPinViolations(ci(OK_REF), lock(null, "1.61.1", "1.61.1"));
  assert.equal(v.length, 1);
  assert.match(v[0], /@playwright\/test/);
});

test("refuse un package-lock.json illisible au lieu de l'ignorer", () => {
  const v = playwrightPinViolations(ci(OK_REF), { path: LOCK_PATH, text: "{" });
  assert.equal(v.length, 1);
  assert.match(v[0], /package-lock/);
});

test("refuse le vrai ci.yml sans son digest", () => {
  const ciFile = real(CI_PATH);
  const stripped = ciFile.text.replace(
    /(mcr\.microsoft\.com\/playwright:[^\s@"']+)@sha256:[0-9a-f]{64}/g,
    "$1",
  );
  assert.notEqual(stripped, ciFile.text);
  const v = playwrightPinViolations({ ...ciFile, text: stripped }, real(LOCK_PATH));
  assert.ok(v.length >= 1);
  assert.ok(v.every((m) => /digest/.test(m)));
});
