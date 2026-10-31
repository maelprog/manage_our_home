-- #570: the "you already have an account" notice that `POST /auth/register`
-- mails to a verified address (#460) is capped per address
-- (`auth::account_exists_notice::RATE`), counted in `rate_limit_log` like
-- the recipe import and Open Food Facts (`0029_rate_limits.sql`): same
-- table, same advisory lock, same database clock. The address is stored as
-- the HMAC pseudonym the other limits use (`Pseudonyms::limit`, under the
-- label `account_exists_notice`), never in clear.
--
-- Only the list of kinds the table accepts changes; 0029 is left as it was.
ALTER TABLE rate_limit_log DROP CONSTRAINT rate_limit_log_kind_check;
ALTER TABLE rate_limit_log ADD CONSTRAINT rate_limit_log_kind_check
    CHECK (kind IN ('recipe_import', 'openfoodfacts', 'account_exists_notice'));
