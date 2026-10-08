//! What the Open Food Facts stand-in of the e2e job answers (#402): the
//! stack's apps/api is pointed at it (`OPENFOODFACTS_BASE_URL`), so the
//! suite never reaches the real service. `off-stub.ts` serves it.

// NB : pas d'`enum` ni de « parameter properties » ici — Node exécute ces
// fichiers en effaçant les types.

/** The records the suite scans, in Open Food Facts' `product` shape. */
export const KNOWN_PRODUCTS: Record<string, Record<string, unknown>> = {
  "3017620422003": {
    product_name: "Pâte à tartiner",
    quantity: "400 g",
    categories_tags: ["en:spreads"],
  },
  // A category with a default shelf life (#404): `en:yogurts`.
  "3033490001063": {
    product_name: "Yaourt nature",
    categories_tags: ["en:dairies", "en:fermented-milk-products", "en:yogurts"],
  },
};

export interface StubAnswer {
  status: number;
  body: unknown;
}

/** The answer to `GET <path>`: `/api/v2/product/<code>` gives the known
 *  record, or a 404 with `status: 0` like the real service; anything else is
 *  a 404 too. */
export function offStubAnswer(path: string): StubAnswer {
  const match = /^\/api\/v2\/product\/([0-9]+)(?:\?.*)?$/.exec(path);
  const code = match ? match[1] : "";
  const product = code ? KNOWN_PRODUCTS[code] : undefined;
  if (product) {
    return { status: 200, body: { code, status: 1, product } };
  }
  return { status: 404, body: { code, status: 0 } };
}
