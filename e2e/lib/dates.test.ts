import { test } from "node:test";
import assert from "node:assert/strict";

import { parisDay, parisDayOfMonth } from "./dates.ts";

// Le fuseau d'affichage v1 est Europe/Paris (apps/shared/src/validation/*.rs,
// `chrono_tz::Europe::Paris`). Le runner CI, lui, est en UTC : c'est tout
// l'écart que ces fonctions doivent absorber.

test("en pleine journée, le jour de Paris est le jour UTC", () => {
	assert.equal(parisDay(0, new Date("2026-09-04T12:00:00Z")), "2026-09-04");
	assert.equal(parisDay(0, new Date("2026-01-15T12:00:00Z")), "2026-01-15");
});

test("après minuit à Paris, le jour UTC est encore la veille", () => {
	// 23:51 UTC le 4 = 01:51 le 5 à Paris (CEST, UTC+2). C'est la fenêtre qui
	// faisait échouer `home.spec.ts` toutes les nuits entre 22 h et minuit UTC.
	assert.equal(parisDay(0, new Date("2026-09-04T23:51:00Z")), "2026-09-05");
	// L'hiver la bascule est une heure plus tard (CET, UTC+1).
	assert.equal(parisDay(0, new Date("2026-01-15T22:30:00Z")), "2026-01-15");
	assert.equal(parisDay(0, new Date("2026-01-15T23:30:00Z")), "2026-01-16");
});

test("le décalage saute les fins de mois et d'année", () => {
	assert.equal(parisDay(1, new Date("2026-09-30T12:00:00Z")), "2026-10-01");
	// Paris est déjà le 1er octobre : +1 donne le 2, pas le 1er.
	assert.equal(parisDay(1, new Date("2026-09-30T23:00:00Z")), "2026-10-02");
	assert.equal(parisDay(1, new Date("2026-12-31T12:00:00Z")), "2027-01-01");
});

test("le décalage ne se laisse pas décaler par un changement d'heure", () => {
	// Le 25 octobre 2026 dure 25 h à Paris (retour à CET). Ajouter 24 h à
	// l'instant retomberait dans la même journée ; le calcul se fait donc sur
	// les composantes de date, pas sur l'instant.
	assert.equal(parisDay(1, new Date("2026-10-24T23:30:00Z")), "2026-10-26");
	// 29 mars 2026 : journée de 23 h (passage à CEST).
	assert.equal(parisDay(1, new Date("2026-03-28T23:30:00Z")), "2026-03-30");
});

test("parisDayOfMonth garde le mois de Paris, pas celui du runner", () => {
	assert.equal(parisDayOfMonth(15, new Date("2026-09-04T12:00:00Z")), "2026-09-15");
	// Dernier jour du mois passé 22 h UTC : Paris est en octobre.
	assert.equal(parisDayOfMonth(15, new Date("2026-09-30T23:00:00Z")), "2026-10-15");
});
