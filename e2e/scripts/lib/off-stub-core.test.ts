import assert from "node:assert/strict";
import test from "node:test";

import { KNOWN_PRODUCTS, offStubAnswer } from "./off-stub-core.ts";

test("a known code gets a found product, in the shape Open Food Facts answers", () => {
  const answer = offStubAnswer("/api/v2/product/3017620422003?fields=product_name,quantity");
  assert.equal(answer.status, 200);
  assert.deepEqual(answer.body, {
    code: "3017620422003",
    status: 1,
    product: KNOWN_PRODUCTS["3017620422003"],
  });
});

test("an unknown code is a 404 carrying status 0, as the real service answers", () => {
  const answer = offStubAnswer("/api/v2/product/4006381333931");
  assert.equal(answer.status, 404);
  assert.deepEqual(answer.body, { code: "4006381333931", status: 0 });
});

test("any other path is a 404 without a product", () => {
  for (const path of ["/", "/api/v2/search?q=x", "/api/v2/product/", "/api/v2/product/12/34"]) {
    const answer = offStubAnswer(path);
    assert.equal(answer.status, 404, path);
    assert.equal((answer.body as { status: number }).status, 0, path);
  }
});
