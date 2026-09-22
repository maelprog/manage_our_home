-- Issue #137. Art. 8 GDPR: the service accepts accounts from 15 years old —
-- the French threshold of the loi Informatique et Libertés (art. 45) — and
-- does not accept anyone younger, so no parental-consent path exists.
--
-- What is stored is the *declaration*, not the age: the registration form
-- asks a yes/no ("I am 15 or older"), and this column records when that box
-- was ticked. Asking for a birth date instead would put a date of birth in
-- the database to derive a single boolean from it — more personal data than
-- the check needs (art. 5.1.c), no more verifiable, and one more field to
-- export, purge and justify in the register of processing.
--
-- NULL means "no declaration on file": accounts created before this migration,
-- and accounts created through Google sign-in, which never sees the form.
ALTER TABLE users ADD COLUMN age_declared_at TIMESTAMPTZ;

COMMENT ON COLUMN users.age_declared_at IS
    'When the account holder declared being at least 15 years old at registration (art. 8 GDPR, issue #137). NULL: no declaration on file.';
