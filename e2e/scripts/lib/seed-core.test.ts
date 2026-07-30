import assert from "node:assert/strict";
import test from "node:test";

import { createRng, cycle, missingCount, pick, spreadOverMonth } from "./seed-core.ts";

// ---------------------------------------------------------------------------
// createRng — determinism is a hard requirement: byte measurements taken on
// two different days must be comparable, so nothing in the seed may come from
// an unseeded Math.random().
// ---------------------------------------------------------------------------

test("createRng yields the same sequence for the same seed", () => {
  const a = createRng(1234);
  const b = createRng(1234);
  assert.deepEqual([a(), a(), a(), a()], [b(), b(), b(), b()]);
});

test("createRng yields a different sequence for a different seed", () => {
  assert.notEqual(createRng(1).toString + createRng(1)(), createRng(2)());
});

test("createRng stays in [0, 1)", () => {
  const rng = createRng(7);
  for (let i = 0; i < 500; i += 1) {
    const v = rng();
    assert.ok(v >= 0 && v < 1, `out of range: ${v}`);
  }
});

// ---------------------------------------------------------------------------
// pick / cycle — how the French corpora turn into rows.
// ---------------------------------------------------------------------------

test("pick draws from the array using the rng", () => {
  const rng = createRng(42);
  const drawn = [pick(rng, ["a", "b", "c"]), pick(rng, ["a", "b", "c"])];
  const rng2 = createRng(42);
  assert.deepEqual(drawn, [pick(rng2, ["a", "b", "c"]), pick(rng2, ["a", "b", "c"])]);
  drawn.forEach((v) => assert.ok(["a", "b", "c"].includes(v)));
});

test("pick refuses an empty corpus rather than returning undefined", () => {
  assert.throws(() => pick(createRng(1), []), /vide/);
});

test("cycle walks the corpus in order and wraps around", () => {
  const c = ["x", "y"];
  assert.deepEqual([cycle(c, 0), cycle(c, 1), cycle(c, 2), cycle(c, 3)], ["x", "y", "x", "y"]);
});

// ---------------------------------------------------------------------------
// missingCount — the idempotence rule: re-running tops a collection back up
// to its target instead of doubling it.
// ---------------------------------------------------------------------------

test("missingCount asks for the full target on an empty collection", () => {
  assert.equal(missingCount(40, 0), 40);
});

test("missingCount asks only for the shortfall", () => {
  assert.equal(missingCount(40, 31), 9);
});

test("missingCount asks for nothing once the target is reached or passed", () => {
  assert.equal(missingCount(40, 40), 0);
  assert.equal(missingCount(40, 57), 0);
});

// ---------------------------------------------------------------------------
// spreadOverMonth — the agenda renders the *current* month grid, so events
// seeded outside it would leave the page as empty as an unseeded stack.
// ---------------------------------------------------------------------------

test("spreadOverMonth keeps every date inside the reference month", () => {
  const days = spreadOverMonth(new Date("2026-02-14T00:00:00Z"), 40);
  assert.equal(days.length, 40);
  for (const d of days) {
    assert.equal(d.getUTCFullYear(), 2026);
    assert.equal(d.getUTCMonth(), 1);
    assert.ok(d.getUTCDate() >= 1 && d.getUTCDate() <= 28);
  }
});

test("spreadOverMonth is deterministic for a given month and count", () => {
  const a = spreadOverMonth(new Date("2026-07-30T00:00:00Z"), 12).map((d) => d.toISOString());
  const b = spreadOverMonth(new Date("2026-07-02T00:00:00Z"), 12).map((d) => d.toISOString());
  assert.deepEqual(a, b);
});

test("spreadOverMonth covers the whole 1..28 window rather than piling up on one day", () => {
  // 1..28 only, so the spread — and therefore the byte counts — don't depend
  // on whether the month has 28, 30 or 31 days.
  const days = spreadOverMonth(new Date("2026-07-30T00:00:00Z"), 28);
  assert.equal(new Set(days.map((d) => d.getUTCDate())).size, 28);
});

test("spreadOverMonth wraps past 28 instead of leaking into the next month", () => {
  const days = spreadOverMonth(new Date("2026-02-14T00:00:00Z"), 40);
  assert.equal(days[28].getUTCDate(), 1);
  assert.equal(days[28].getUTCMonth(), 1);
  // ...but the times differ, so the two events on day 1 are not duplicates.
  assert.notEqual(days[0].getUTCHours(), days[28].getUTCHours());
});
