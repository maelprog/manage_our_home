-- #1036: the receipts (tickets de caisse) a family reads into its stock and,
-- optionally, its budget. Schema only: the upload and the reading (#1040),
-- the import (#1041), the export and the purges (#1042) come after it.
--
-- A receipt is kept as proof of the expenses it was imported into (user's
-- decision of 2026-11-28). One expense per article, so several
-- `budget_entries` share one receipt (`budget_entries.receipt_id` below).
--
-- `storage_key` is the object's key in the bucket, never a public URL (as
-- `event_attachments.storage_key`, 0002). `store_name` and `purchased_on`
-- are what the reading found on the receipt, NULL when it found nothing.
-- `imported_at` NULL is a receipt read but not validated yet: those are the
-- ones the purge of #1042 deletes once stale, hence the partial index on
-- `created_at`.
--
-- `uploaded_by` is a key onto `users`, as `event_attachments.uploaded_by`:
-- the purge anonymises the `users` row and never deletes it, so no
-- `ON DELETE` fires towards it (`jobs::account_purge`).
CREATE TABLE receipts (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id        UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    uploaded_by     UUID NOT NULL REFERENCES users(id),
    storage_key     TEXT NOT NULL UNIQUE,
    mime_type       TEXT NOT NULL,
    size_bytes      BIGINT NOT NULL CHECK (size_bytes > 0),
    store_name      TEXT,
    purchased_on    DATE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    imported_at     TIMESTAMPTZ,
    -- Target of the composite key from `budget_entries` below.
    UNIQUE (id, group_id)
);
-- A family's receipts, newest first; the leading column also serves the
-- family's delete (the CASCADE) and plain `WHERE group_id = $1`.
CREATE INDEX receipts_group_id_created_at_idx ON receipts (group_id, created_at);
-- The receipts read but never validated, oldest first, across families:
-- what the purge of stale unvalidated receipts scans (#1042).
CREATE INDEX receipts_pending_created_at_idx ON receipts (created_at)
    WHERE imported_at IS NULL;
-- An account's receipts, for its export and its purge (#1042).
CREATE INDEX receipts_uploaded_by_idx ON receipts (uploaded_by);

ALTER TABLE receipts ENABLE ROW LEVEL SECURITY;
ALTER TABLE receipts FORCE ROW LEVEL SECURITY;
CREATE POLICY receipts_isolation ON receipts
    USING (group_id::text = current_setting('app.group_id', true));

-- The receipt an expense was imported from, or none (NULL: every expense
-- written before this migration, and those entered by hand). The key is
-- composite, as `category_id` (0035): an expense can only name a receipt of
-- its own family, whatever the API checks. Deleting the receipt clears
-- `receipt_id` only (PostgreSQL 15+ column list), the expense stays.
--
-- RLS unchanged: the column lives on `budget_entries`, under its policy.
ALTER TABLE budget_entries ADD COLUMN receipt_id UUID;
ALTER TABLE budget_entries
    ADD CONSTRAINT budget_entries_receipt_fkey
    FOREIGN KEY (receipt_id, group_id)
    REFERENCES receipts (id, group_id)
    ON DELETE SET NULL (receipt_id);
-- Serves the receipt's delete (the key's SET NULL) and a receipt's expenses.
CREATE INDEX budget_entries_receipt_id_idx ON budget_entries (receipt_id)
    WHERE receipt_id IS NOT NULL;
