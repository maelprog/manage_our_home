-- Epic 8: User admin (global technical superadmin).
-- Single flag, no new table: v1 has one expected superadmin account, set
-- manually via SQL by the operator (no self-service promotion endpoint,
-- since there's no signup flow for this role per architecture.md).
ALTER TABLE users ADD COLUMN is_superadmin BOOLEAN NOT NULL DEFAULT FALSE;
