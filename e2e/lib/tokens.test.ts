import { test } from "node:test";
import assert from "node:assert/strict";

import { bearerTokenFrom, newBearerToken } from "./tokens.ts";

// Même construction qu'apps/api (`auth::token`, #222, #335) : la base ne
// garde que le SHA-256 des 32 octets, le lien porte ces octets en base64url
// sans bourrage. Les helpers de `db.ts` posent ce SHA-256 sur la ligne que
// l'API vient de créer, et rendent le jeton correspondant.

test("32 octets nuls : 43 « A » et le SHA-256 publié", () => {
	const { token, hash } = bearerTokenFrom(Buffer.alloc(32));
	assert.equal(token, "A".repeat(43));
	assert.equal(
		hash.toString("hex"),
		"66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925",
	);
});

test("un jeton neuf fait 43 caractères base64url, et deux jetons diffèrent", () => {
	const a = newBearerToken();
	const b = newBearerToken();
	assert.match(a.token, /^[A-Za-z0-9_-]{43}$/);
	assert.equal(a.hash.length, 32);
	assert.notEqual(a.token, b.token);
});

test("l'empreinte d'un jeton neuf est celle de ses propres octets", () => {
	const fresh = newBearerToken();
	const again = bearerTokenFrom(Buffer.from(fresh.token, "base64url"));
	assert.equal(again.token, fresh.token);
	assert.deepEqual(again.hash, fresh.hash);
});
