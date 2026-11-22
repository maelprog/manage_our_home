import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

// ---------------------------------------------------------------------------
// Garde de cible du banc d'attaque local (#915).
//
// `infra/attack-bench/bench.sh` rejoue des requêtes hostiles : il ne doit
// viser que la pile qu'il a démarrée, sur la machine. Toute cible qui n'est
// pas `http://127.0.0.1[:port]` ou `http://localhost[:port]` est refusée,
// avec un message qui le dit, AVANT le premier appel à docker ou à curl.
//
// Ces tests lancent le vrai script (bash), sans docker : un faux `docker` et
// un faux `curl` en tête du PATH laissent une trace s'ils sont appelés, et un
// refus doit n'en laisser aucune.
// ---------------------------------------------------------------------------

const REPO = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const BENCH = join(REPO, "infra", "attack-bench", "bench.sh");

type Run = { status: number | null; stdout: string; stderr: string; touched: boolean };

function run(args: string[], env: Record<string, string> = {}): Run {
  const dir = mkdtempSync(join(tmpdir(), "attack-bench-"));
  const marker = join(dir, "touched");
  try {
    for (const tool of ["docker", "curl"]) {
      const fake = join(dir, tool);
      writeFileSync(fake, `#!/bin/sh\necho "$0 $*" >> "${marker}"\nexit 97\n`);
      chmodSync(fake, 0o755);
    }
    const res = spawnSync("bash", [BENCH, ...args], {
      encoding: "utf8",
      env: { ...process.env, ...env, PATH: `${dir}:${process.env.PATH ?? ""}` },
    });
    return {
      status: res.status,
      stdout: res.stdout,
      stderr: res.stderr,
      touched: existsSync(marker),
    };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

const ACCEPTED = [
  "http://127.0.0.1",
  "http://127.0.0.1:18080",
  "http://localhost:18080",
  "http://localhost:18080/",
  "http://127.0.0.1:18080/auth/me",
];

const REFUSED = [
  // Le cas de l'issue : une URL https, donc un hôte qui n'est pas la pile.
  "https://maison.example.org",
  "https://127.0.0.1:18080",
  "https://localhost",
  "http://maison.example.org",
  "http://192.168.1.10:8080",
  "http://10.0.0.1",
  "http://0.0.0.0:8080",
  "http://[::1]:8080",
  "http://127.0.0.2:8080",
  // Un hôte qui commence comme la boucle locale sans l'être.
  "http://localhost.example.org",
  "http://127.0.0.1.nip.io",
  "http://localhost:8080.example.org",
  // L'hôte réel est après l'arobase.
  "http://127.0.0.1@maison.example.org",
  "http://localhost:80@maison.example.org",
  "http://user:pw@localhost:8080",
  // Formes approximatives : pas de schéma, schéma autre, casse, espaces.
  "localhost:8080",
  "127.0.0.1",
  "ftp://localhost",
  "HTTP://LOCALHOST",
  " http://localhost",
  "http://localhost ",
  "http://localhost:8080\nhttps://maison.example.org",
  "http://localhost:",
  "http://localhost:999999",
  "http://localhost?x=https://maison.example.org",
  "",
];

for (const url of ACCEPTED) {
  test(`check-target accepte ${JSON.stringify(url)}`, () => {
    const r = run(["check-target", url]);
    assert.equal(r.status, 0, r.stderr);
    assert.equal(r.touched, false);
  });
}

for (const url of REFUSED) {
  test(`check-target refuse ${JSON.stringify(url)}`, () => {
    const r = run(["check-target", url]);
    assert.equal(r.status, 2, `exit ${r.status}, stderr: ${r.stderr}`);
    assert.match(r.stderr, /cible refusée/);
    assert.match(r.stderr, /127\.0\.0\.1/);
    assert.equal(r.touched, false);
  });
}

test("up --api-url https://… échoue avant tout appel à docker ou curl", () => {
  const r = run(["up", "--api-url", "https://maison.example.org"]);
  assert.equal(r.status, 2, `exit ${r.status}, stderr: ${r.stderr}`);
  assert.match(r.stderr, /cible refusée : https:\/\/maison\.example\.org/);
  assert.match(r.stderr, /cible locale uniquement/);
  assert.equal(r.touched, false);
});

test("BENCH_API_URL hors boucle locale fait échouer up de la même façon", () => {
  const r = run(["up"], { BENCH_API_URL: "https://maison.example.org" });
  assert.equal(r.status, 2, `exit ${r.status}, stderr: ${r.stderr}`);
  assert.match(r.stderr, /cible refusée/);
  assert.equal(r.touched, false);
});

test("--web-url est gardée comme --api-url", () => {
  const r = run(["up", "--web-url", "http://maison.example.org:3000"]);
  assert.equal(r.status, 2, `exit ${r.status}, stderr: ${r.stderr}`);
  assert.match(r.stderr, /cible refusée/);
  assert.equal(r.touched, false);
});

test("sans sous-commande : usage, exit 2", () => {
  const r = run([]);
  assert.equal(r.status, 2);
  assert.match(r.stderr, /usage/);
  assert.equal(r.touched, false);
});
