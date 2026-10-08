-- #402: scanning an article's barcode to add it to the family's stock, with
-- the product's name and pack size read from Open Food Facts.

-- The code an article was added under, in its normalized form
-- (`stocks::barcode::normalize`: EAN-13, EAN-8, or a UPC-A stored as its
-- EAN-13). NULL for an article entered without one, which is every article
-- created before this migration. Covered by `stock_items_isolation` like
-- the other columns.
ALTER TABLE stock_items ADD COLUMN barcode TEXT
    CHECK (barcode IS NULL OR barcode ~ '^([0-9]{8}|[0-9]{13})$');

-- One article per code in a family: rescanning a code already in stock
-- adds to that article (the quantity adjustment open to any member) rather
-- than creating a second one. Partial, so articles without a code are not
-- constrained. A second article with the same code is a 409
-- (`barcode_already_in_stock`).
CREATE UNIQUE INDEX stock_items_group_barcode_key
    ON stock_items (group_id, barcode) WHERE barcode IS NOT NULL;

-- What Open Food Facts answered for a code, so a product is not asked again
-- for every scan (`stocks::openfoodfacts`: `CACHE_TTL` for a product,
-- `MISS_TTL` for an unknown or nameless one, `name` NULL).
--
-- RLS posture: none, deliberately. The tables without RLS so far are the
-- account-level ones (`users`, `sessions`, the token tables, `audit_log`,
-- `push_subscriptions`…), read on the caller's own id; every family-scoped
-- table has a policy on `group_id`. This one is neither: the first table
-- outside both. Its rows are Open Food Facts' public records (ODbL), keyed
-- by product code, and belong to no family and no account: no `group_id` to
-- scope by, no user id, nothing a member types. The same record serves every
-- family that scans the code, and that is the point of caching it. What it
-- tells a family is what Open Food Facts already tells anyone who asks. It
-- does not say who scanned a code, or when, or which family holds it: that
-- is `stock_items.barcode`, behind `stock_items_isolation`. `fetched_at` is
-- when the server last asked Open Food Facts, which any family's scan may
-- have caused, so it says nothing about a given family either.
CREATE TABLE off_products (
    code            TEXT PRIMARY KEY CHECK (code ~ '^([0-9]{8}|[0-9]{13})$'),
    name            TEXT,
    quantity        TEXT,
    categories_tags TEXT[] NOT NULL DEFAULT '{}',
    fetched_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
