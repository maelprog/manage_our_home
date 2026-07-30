#!/usr/bin/env node
//! Peuple une stack de développement avec un mois de données de foyer
//! ordinaires, **via l'API HTTP**, pour que `measure-page-weight.ts` ait
//! quelque chose de réaliste à peser.
//!
//! Pourquoi ce script existe : le poids des pages a d'abord été mesuré à la
//! main, sur une stack que personne n'avait peuplée. Les chiffres avaient
//! l'air normaux (la feuille de style inlinée fait le gros du poids d'une
//! page vide), un second agent les a « confirmés » sur sa propre stack vide,
//! et le budget qui en est sorti était faux de près du double. Un jeu de
//! données reproductible, versionné et validé par l'API elle-même remplace
//! ces manipulations ad hoc.
//!
//! Trois propriétés non négociables :
//!
//! 1. **Tout passe par l'API** sauf le jeton de vérification d'e-mail, lu
//!    directement en base — la frontière de confiance que `lib/db.ts`
//!    documente déjà et que les tests d'intégration Rust utilisent aussi.
//!    Les données sont donc valides par construction, et le script survit à
//!    un changement de schéma.
//! 2. **Aucune erreur n'est avalée** : chaque requête déclare les codes
//!    qu'elle accepte, tout le reste jette avec la méthode, l'URL, le code et
//!    le corps (`lib/http.ts`).
//! 3. **Déterminisme** : aucune source d'aléa non semée, donc deux exécutions
//!    de la même graine produisent les mêmes octets. Le corpus est du vrai
//!    texte de foyer et non `article 1`, `article 2` : gzip écrase des
//!    libellés répétitifs, et une mesure sur du texte artificiel produit un
//!    budget faussement rassurant. La campagne manuelle antérieure (issue #83)
//!    avait relevé 13 741 o contre 14 990 o sur `/messagerie` de part et
//!    d'autre du budget pour le même nombre de messages — chiffres pris sur un
//!    autre jeu de données que celui-ci, cités pour l'ordre de grandeur et non
//!    reproduits ici.
//!
//! Idempotence : le script **complète** chaque collection jusqu'à sa cible au
//! lieu de la dupliquer (`missingCount`). Le relancer est sans effet une fois
//! la cible atteinte ; une exécution interrompue se termine en le relançant.
//! Il ne supprime jamais rien.

import { Client } from "pg";

import {
  BUDGET_ENTRIES,
  EVENTS,
  GROCERY_ITEMS,
  MESSAGES,
  RECIPES,
  STOCK_ITEMS,
} from "./lib/corpus-fr.ts";
import { HttpSession, waitForService } from "./lib/http.ts";
import { createRng, cycle, missingCount, monthGridWindow, spreadOverMonth } from "./lib/seed-core.ts";

// --- configuration ---------------------------------------------------------

const API_BASE_URL = (process.env.API_BASE_URL ?? "http://localhost:8080").replace(/\/$/, "");
const GROUP_NAME = process.env.SEED_GROUP_NAME ?? "Foyer Perrin";
const PASSWORD = process.env.SEED_PASSWORD ?? "mesure du poids des pages";
const RNG_SEED = Number(process.env.SEED_RNG_SEED ?? 424242);

/** Ancré sur *aujourd'hui* par défaut, et c'est important : voir REFERENCE. */
const REFERENCE = process.env.SEED_REFERENCE_DATE
  ? new Date(`${process.env.SEED_REFERENCE_DATE}T12:00:00Z`)
  : new Date();

// Deux membres, et pas un seul : le fil de discussion affiche le nom de
// l'auteur de chaque message. Surchargeables pour que deux personnes puissent
// peupler la même base sans se marcher dessus (comme SEED_GROUP_NAME).
const OWNER = {
  email: process.env.SEED_OWNER_EMAIL ?? "camille.perf@example.test",
  displayName: process.env.SEED_OWNER_NAME ?? "Camille Perrin",
};
const PARTNER = {
  email: process.env.SEED_PARTNER_EMAIL ?? "jonas.perf@example.test",
  displayName: process.env.SEED_PARTNER_NAME ?? "Jonas Perrin",
};

/** Cibles : un mois de foyer ordinaire. `messages` remplit exactement la
 *  première page de `/messagerie` (DEFAULT_PAGE_LIMIT = 50 côté API). */
const TARGETS = {
  events: Number(process.env.SEED_EVENTS ?? 40),
  stockItems: Number(process.env.SEED_STOCK_ITEMS ?? 40),
  groceryItems: Number(process.env.SEED_GROCERY_ITEMS ?? 30),
  budgetEntries: Number(process.env.SEED_BUDGET_ENTRIES ?? 30),
  recipes: Number(process.env.SEED_RECIPES ?? 25),
  messages: Number(process.env.SEED_MESSAGES ?? 50),
};

// --- petites aides ---------------------------------------------------------

function log(message: string): void {
  process.stdout.write(`${message}\n`);
}

function isoDay(d: Date): string {
  return d.toISOString().slice(0, 10);
}

/**
 * Lit le jeton de vérification d'e-mail directement en Postgres.
 *
 * Même mécanisme et même justification que `e2e/lib/db.ts` : apps/api n'expose
 * aucun crochet HTTP de test pour les jetons, et en ajouter un affaiblirait la
 * production. C'est la seule écriture/lecture directe en base de ce script.
 */
async function fetchVerificationToken(email: string): Promise<string> {
  const url = process.env.DATABASE_URL;
  if (!url) {
    throw new Error(
      "DATABASE_URL doit pointer sur le Postgres d'apps/api (cf. e2e/README.md) : " +
        "le jeton de vérification d'e-mail ne s'obtient pas par l'API.",
    );
  }
  const client = new Client({ connectionString: url });
  await client.connect();
  try {
    const { rows } = await client.query(
      `SELECT t.token FROM email_verification_tokens t
       JOIN users u ON u.id = t.user_id
       WHERE u.email = $1 AND t.consumed_at IS NULL
       ORDER BY t.created_at DESC
       LIMIT 1`,
      [email],
    );
    if (rows.length === 0) {
      throw new Error(`aucun jeton de vérification en attente pour ${email}`);
    }
    return rows[0].token as string;
  } finally {
    await client.end();
  }
}

// --- comptes et famille ----------------------------------------------------

async function signIn(who: { email: string; displayName: string }): Promise<HttpSession> {
  const session = new HttpSession(API_BASE_URL);
  const registered = await session.raw("POST", "/auth/register", {
    body: { email: who.email, password: PASSWORD, display_name: who.displayName },
    expect: [201, 409],
  });

  if (registered.status === 201) {
    const token = await fetchVerificationToken(who.email);
    await session.raw("GET", `/auth/verify-email?token=${encodeURIComponent(token)}`, {
      expect: [200, 204, 302, 303],
    });
    log(`  compte créé et vérifié : ${who.email}`);
  } else {
    log(`  compte déjà présent : ${who.email}`);
  }

  await session.raw("POST", "/auth/login", {
    body: { email: who.email, password: PASSWORD },
    expect: [200],
  });
  return session;
}

type GroupSummary = { group_id: string; name: string; role: string };

async function ensureGroup(owner: HttpSession, partner: HttpSession): Promise<string> {
  const groups = await owner.json<GroupSummary[]>("GET", "/groups");
  const existing = groups.find((g) => g.name === GROUP_NAME);
  let groupId: string;
  if (existing) {
    groupId = existing.group_id;
    log(`  famille déjà présente : « ${GROUP_NAME} » (${groupId})`);
  } else {
    const created = await owner.json<{ id: string }>("POST", "/groups", {
      body: { name: GROUP_NAME },
      expect: [201],
    });
    groupId = created.id;
    log(`  famille créée : « ${GROUP_NAME} » (${groupId})`);
  }

  const partnerGroups = await partner.json<GroupSummary[]>("GET", "/groups");
  if (partnerGroups.some((g) => g.group_id === groupId)) {
    log(`  ${PARTNER.displayName} est déjà membre`);
    return groupId;
  }

  const invitation = await owner.json<{ token: string }>("POST", `/groups/${groupId}/invitations`, {
    body: { invited_email: PARTNER.email },
    expect: [200, 201],
  });
  await partner.raw("POST", `/groups/invitations/${invitation.token}/accept`, {
    body: {},
    expect: [200, 201, 204],
  });
  log(`  ${PARTNER.displayName} a rejoint la famille`);
  return groupId;
}

// --- inventaire courant ----------------------------------------------------

/**
 * La fenêtre de comptage des événements = **celle que `/agenda` rend**
 * (`monthGridWindow`), et rien d'autre.
 *
 * La version précédente comptait sur ±1 an. C'était un bug de conception :
 * après un changement de mois, les 40 événements du mois passé comptaient
 * toujours pour la cible, `missingCount` renvoyait 0, et comme le seed ne
 * supprime jamais rien, `npm run seed` — l'instruction de réparation que le
 * mesureur imprime — ne créait plus rien. La stack n'avait plus de sortie
 * supportée. Compter dans la fenêtre rendue rend le message vrai : relancer
 * le seed repeuple bien le mois courant.
 */
function countingWindow(reference: Date): { from: string; to: string } {
  const win = monthGridWindow(reference);
  return { from: win.from.toISOString(), to: win.to.toISOString() };
}

type Counts = {
  events: number;
  stockItems: number;
  groceryItems: number;
  budgetEntries: number;
  recipes: number;
  messages: number;
};

/** Tous les volumes, **relus depuis l'API** — jamais depuis les constantes. */
async function readCounts(session: HttpSession, gid: string): Promise<Counts> {
  const win = countingWindow(REFERENCE);
  const occurrences = await session.json<{ occurrences: { id: string }[] }>(
    "GET",
    `/groups/${gid}/events?from=${encodeURIComponent(win.from)}&to=${encodeURIComponent(win.to)}`,
  );
  const stock = await session.json<{ items: unknown[] }>("GET", `/groups/${gid}/stock-items`);
  const grocery = await session.json<{ items: unknown[] }>("GET", `/groups/${gid}/grocery-items`);
  const budget = await session.json<{ entries: unknown[] }>("GET", `/groups/${gid}/budget-entries`);
  const recipes = await session.json<{ recipes: unknown[] }>("GET", `/groups/${gid}/recipes`);
  const messages = await session.json<{ messages: unknown[] }>(
    "GET",
    `/groups/${gid}/messages?limit=100`,
  );
  return {
    // Un événement récurrent apparaît une fois par occurrence : on déduplique
    // sur l'id de l'événement.
    events: new Set(occurrences.occurrences.map((o) => o.id)).size,
    stockItems: stock.items.length,
    groceryItems: grocery.items.length,
    budgetEntries: budget.entries.length,
    recipes: recipes.recipes.length,
    messages: messages.messages.length,
  };
}

// --- création --------------------------------------------------------------

async function seedEvents(session: HttpSession, gid: string, existing: number): Promise<number> {
  const todo = missingCount(TARGETS.events, existing);
  // Les créneaux sont calculés pour la cible entière puis on ne joue que la
  // queue manquante : compléter une collection à moitié pleine produit les
  // mêmes dates que si elle avait été semée d'un coup.
  const slots = spreadOverMonth(REFERENCE, TARGETS.events);
  for (let i = existing; i < existing + todo; i += 1) {
    const seed = cycle(EVENTS, i);
    const starts = slots[i % slots.length];
    const ends = new Date(starts.getTime() + 90 * 60_000);
    await session.raw("POST", `/groups/${gid}/events`, {
      body: {
        title: seed.title,
        description: seed.description,
        location: seed.location,
        starts_at: starts.toISOString(),
        ends_at: ends.toISOString(),
        all_day: false,
        // Un tiers des entrées sont des tâches : la grille les rend avec un
        // marqueur différent, et une page réaliste mélange les deux.
        is_task: i % 3 === 0,
      },
      expect: [200, 201],
    });
  }
  return todo;
}

async function seedStockItems(session: HttpSession, gid: string, existing: number): Promise<number> {
  const todo = missingCount(TARGETS.stockItems, existing);
  for (let i = existing; i < existing + todo; i += 1) {
    const seed = cycle(STOCK_ITEMS, i);
    await session.raw("POST", `/groups/${gid}/stock-items`, {
      body: {
        name: seed.name,
        category: seed.category,
        quantity: seed.quantity,
        unit: seed.unit,
        reorder_threshold: seed.reorderThreshold,
      },
      expect: [200, 201],
    });
  }
  return todo;
}

async function seedGroceryItems(
  session: HttpSession,
  gid: string,
  existing: number,
): Promise<number> {
  const todo = missingCount(TARGETS.groceryItems, existing);
  // Un quart de la liste est déjà coché : c'est l'état ordinaire d'une liste
  // de courses en cours, et la ligne cochée n'a pas le même markup.
  //
  // Les tirages sont faits pour la cible ENTIÈRE puis indexés, jamais
  // consommés au fil de la queue manquante : sinon une reprise (30 articles
  // créés en deux passes) cocherait d'autres lignes qu'une passe unique, et
  // les octets mesurés cesseraient d'être comparables. Même raison que pour
  // les créneaux de `spreadOverMonth`.
  const rng = createRng(RNG_SEED + 7);
  const checkedFlags = Array.from({ length: TARGETS.groceryItems }, () => rng() < 0.25);

  const created: { id: string; index: number }[] = [];
  for (let i = existing; i < existing + todo; i += 1) {
    const seed = cycle(GROCERY_ITEMS, i);
    const item = await session.json<{ id: string }>("POST", `/groups/${gid}/grocery-items`, {
      body: { name: seed.name, quantity: seed.quantity, unit: seed.unit },
      expect: [200, 201],
    });
    created.push({ id: item.id, index: i });
  }
  for (const { id, index } of created) {
    if (checkedFlags[index % checkedFlags.length]) {
      await session.raw("POST", `/groups/${gid}/grocery-items/${id}/check`, {
        body: { checked: true },
        expect: [200, 201, 204],
      });
    }
  }
  return todo;
}

async function seedBudgetEntries(
  session: HttpSession,
  gid: string,
  existing: number,
): Promise<number> {
  const todo = missingCount(TARGETS.budgetEntries, existing);
  const slots = spreadOverMonth(REFERENCE, TARGETS.budgetEntries);
  for (let i = existing; i < existing + todo; i += 1) {
    const seed = cycle(BUDGET_ENTRIES, i);
    await session.raw("POST", `/groups/${gid}/budget-entries`, {
      body: { name: seed.name, amount: seed.amount, spent_at: isoDay(slots[i % slots.length]) },
      expect: [200, 201],
    });
  }
  return todo;
}

async function seedRecipes(session: HttpSession, gid: string, existing: number): Promise<number> {
  const todo = missingCount(TARGETS.recipes, existing);
  for (let i = existing; i < existing + todo; i += 1) {
    const seed = cycle(RECIPES, i);
    await session.raw("POST", `/groups/${gid}/recipes`, {
      body: {
        name: seed.name,
        instructions: seed.instructions,
        ingredients: seed.ingredients.map((ing) => ({
          name: ing.name,
          quantity: ing.quantity,
          unit: ing.unit,
          is_optional: false,
        })),
      },
      expect: [200, 201],
    });
  }
  return todo;
}

async function seedMessages(
  owner: HttpSession,
  partner: HttpSession,
  gid: string,
  existing: number,
): Promise<number> {
  const todo = missingCount(TARGETS.messages, existing);
  for (let i = existing; i < existing + todo; i += 1) {
    // Les deux membres écrivent à tour de rôle : le fil affiche le nom de
    // l'auteur, un fil mono-auteur compresserait mieux qu'un vrai.
    const session = i % 2 === 0 ? owner : partner;
    await session.raw("POST", `/groups/${gid}/messages`, {
      body: { content: cycle(MESSAGES, i) },
      expect: [200, 201],
    });
  }
  return todo;
}

// --- programme -------------------------------------------------------------

async function main(): Promise<void> {
  log("Seed de données de mesure");
  log(`  API              : ${API_BASE_URL}`);
  log(`  graine           : ${RNG_SEED}`);
  log(`  date de référence: ${isoDay(REFERENCE)} (les données datées sont ancrées sur ce mois)`);
  log("");

  await waitForService(API_BASE_URL, "/auth/me");

  log("Comptes et famille");
  const owner = await signIn(OWNER);
  const partner = await signIn(PARTNER);
  const gid = await ensureGroup(owner, partner);
  log("");

  const before = await readCounts(owner, gid);
  log("Création (complète chaque collection jusqu'à sa cible)");
  const created = {
    events: await seedEvents(owner, gid, before.events),
    stockItems: await seedStockItems(owner, gid, before.stockItems),
    groceryItems: await seedGroceryItems(owner, gid, before.groceryItems),
    budgetEntries: await seedBudgetEntries(owner, gid, before.budgetEntries),
    recipes: await seedRecipes(owner, gid, before.recipes),
    messages: await seedMessages(owner, partner, gid, before.messages),
  };
  log("");

  // Le bilan est relu depuis l'API, pas déduit des constantes : c'est la
  // seule façon d'affirmer ce qui existe *vraiment* dans la base.
  const after = await readCounts(owner, gid);
  const rows: [string, number, number, number][] = [
    ["événements d'agenda", before.events, created.events, after.events],
    ["articles de stock", before.stockItems, created.stockItems, after.stockItems],
    ["articles de courses", before.groceryItems, created.groceryItems, after.groceryItems],
    ["dépenses", before.budgetEntries, created.budgetEntries, after.budgetEntries],
    ["recettes", before.recipes, created.recipes, after.recipes],
    ["messages", before.messages, created.messages, after.messages],
  ];
  const width = Math.max(...rows.map(([label]) => label.length));
  log("Bilan (volumes relus depuis l'API)");
  log(`  ${"collection".padEnd(width)}  avant  créés  après`);
  for (const [label, wasThere, made, now] of rows) {
    log(
      `  ${label.padEnd(width)}  ${String(wasThere).padStart(5)}  ${String(made).padStart(5)}  ${String(now).padStart(5)}`,
    );
  }
  log("");
  log(`Famille « ${GROUP_NAME} » — ${gid}`);
  log(`Connexion : ${OWNER.email} / ${PASSWORD}`);
  log("");
  log("Mesure : npm run measure");
}

main().catch((error: unknown) => {
  process.stderr.write(`\nÉCHEC DU SEED\n${error instanceof Error ? error.message : String(error)}\n`);
  if (error instanceof Error && error.stack) {
    process.stderr.write(`\n${error.stack}\n`);
  }
  process.exit(1);
});
