-- #401: an expiry date on a stock item, the first step of the article-scan
-- work (#402 scan, #403 GS1 dates, #404 pre-fill all write into it).
--
-- v1 semantics: **one date per article, the nearest one.** No per-batch
-- tracking — a pantry row holding two packs keeps the date of the one to eat
-- first. NULL means no date was recorded. DATE rather than TIMESTAMPTZ: a
-- "à consommer jusqu'au" date is a civil day, not an instant.
--
-- The status shown on /stocks (expired / soon / ok) is derived on read
-- against the viewer's day (`validation::stocks::expiry_status` in
-- apps/shared), never stored, like `low_stock`. The column needs no new
-- policy: the row-level `stock_items_isolation` policy already covers it.

ALTER TABLE stock_items ADD COLUMN expires_on DATE;
