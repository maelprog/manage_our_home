//! Pure logic behind `scripts/seed-perf-data.ts` (unit-tested in
//! `seed-core.test.ts`). No I/O, no clock reads: the seed has to produce the
//! same bytes today and next week, otherwise the page-weight numbers it
//! exists to make measurable stop being comparable.

/**
 * mulberry32 — a tiny, seeded PRNG.
 *
 * `Math.random()` is banned in the seed script: two runs of the same seed
 * must produce byte-identical data, or a page-weight regression can't be told
 * apart from a reshuffled corpus.
 */
export function createRng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** Draws one element out of a corpus, using a seeded rng. */
export function pick<T>(rng: () => number, corpus: readonly T[]): T {
  if (corpus.length === 0) {
    throw new Error("pick() a reçu une liste vide");
  }
  return corpus[Math.floor(rng() * corpus.length)];
}

/** `corpus[index % corpus.length]` — walks a corpus in order, wrapping. */
export function cycle<T>(corpus: readonly T[], index: number): T {
  if (corpus.length === 0) {
    throw new Error("cycle() a reçu une liste vide");
  }
  return corpus[index % corpus.length];
}

/**
 * How many rows are still missing before a collection hits its target.
 *
 * This is the whole of the script's idempotence rule: re-running tops each
 * collection back up to its target instead of doubling it, so `npm run seed`
 * is safe to run twice and a partially-seeded stack (interrupted run) can be
 * finished off without wiping the database.
 */
export function missingCount(target: number, existing: number): number {
  return Math.max(0, target - existing);
}

/**
 * **La** fenêtre : les 42 jours que `/agenda` rend pour le mois de
 * `reference`, du lundi qui précède (ou est) le 1er au 42ᵉ jour inclus.
 *
 * Miroir de `month_grid` (apps/shared/src/validation/agenda.rs). Le seed s'en
 * sert pour compter ce qui existe déjà, la mesure pour compter ce qui est
 * stocké : **une seule fenêtre pour les deux**. La version précédente en avait
 * deux — le seed comptait sur ±1 an, la page n'en rend que 42 jours — et cette
 * divergence était la panne : le seed se croyait complet avec un `/agenda`
 * vide, et `npm run seed`, l'instruction de réparation que le mesureur
 * imprime, ne créait alors plus rien. La stack devenait irréparable.
 */
export function monthGridWindow(reference: Date): { from: Date; to: Date } {
  const first = new Date(
    Date.UTC(reference.getUTCFullYear(), reference.getUTCMonth(), 1, 0, 0, 0, 0),
  );
  // getUTCDay() : 0 = dimanche. On veut le décalage depuis lundi.
  const offsetFromMonday = (first.getUTCDay() + 6) % 7;
  const from = new Date(first.getTime() - offsetFromMonday * 86_400_000);
  const to = new Date(from.getTime() + 42 * 86_400_000 - 1);
  return { from, to };
}

/**
 * `count` dates spread evenly over the reference date's month.
 *
 * `GET /agenda` renders the grid of the *current* month, so events seeded
 * outside it would leave the page exactly as empty as an unseeded stack —
 * the failure mode this whole pair of scripts exists to prevent. Days 1..28
 * only, so the spread doesn't depend on the month's length (and therefore
 * neither do the byte counts).
 */
export function spreadOverMonth(reference: Date, count: number): Date[] {
  const year = reference.getUTCFullYear();
  const month = reference.getUTCMonth();
  return Array.from({ length: count }, (_, i) => {
    const day = 1 + (i % 28);
    // Hours cycle through the waking day so several events share a day cell
    // without sharing a start time.
    const hour = 7 + ((i * 3) % 13);
    return new Date(Date.UTC(year, month, day, hour, (i * 5) % 60, 0));
  });
}
