#!/bin/bash
# Runs once, at first boot of the postgres container (docker-entrypoint-initdb.d,
# mounted from docker-compose.yml). Creates the three roles the API connects
# as. $POSTGRES_USER, the bootstrap superuser, is not one of them:
#
#   app_role        serves requests (DATABASE_URL), NOSUPERUSER NOBYPASSRLS,
#                   so the RLS policies apply to it (apps/api/README.md,
#                   issue #311). Owns no table.
#   migration_role  applies the sqlx migrations at API startup and therefore
#                   owns every table (MIGRATION_DATABASE_URL, issue #105).
#   admin_role      serves the three superadmin endpoints (Epic #8) and the
#                   background jobs (#215 and after) (ADMIN_DATABASE_URL,
#                   apps/api/README.md).
#
# All three pools connect eagerly at startup, so all three roles must exist
# before the api service comes up.
#
# Tables don't exist yet at this point (migrations run at API startup), so the
# grants go through default privileges instead of GRANT ... ON ALL TABLES —
# and they are declared FOR ROLE migration_role, because that is now the role
# that creates them.
set -euo pipefail

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" <<-SQL
	-- 0001_users_auth_groups.sql opens with CREATE EXTENSION IF NOT EXISTS
	-- pgcrypto. pgcrypto is a trusted extension since PG13, so migration_role
	-- could install it given CREATE on the database — installing it here
	-- instead keeps that privilege off the migration role and makes 0001's
	-- statement a no-op.
	CREATE EXTENSION IF NOT EXISTS pgcrypto;

	CREATE ROLE migration_role LOGIN PASSWORD '${MIGRATION_ROLE_PASSWORD}' NOSUPERUSER BYPASSRLS;
	GRANT USAGE, CREATE ON SCHEMA public TO migration_role;

	CREATE ROLE admin_role LOGIN PASSWORD '${ADMIN_ROLE_PASSWORD}' NOSUPERUSER BYPASSRLS;
	ALTER DEFAULT PRIVILEGES FOR ROLE migration_role IN SCHEMA public
	    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO admin_role;
	-- audit_log.id is BIGSERIAL (0001), so its owned sequence needs a grant
	-- of its own — table privileges don't cover it, and without this an
	-- INSERT by a non-owner fails on nextval().
	ALTER DEFAULT PRIVILEGES FOR ROLE migration_role IN SCHEMA public
	    GRANT USAGE, SELECT ON SEQUENCES TO admin_role;

	-- The runtime role: the grants apps/api/README.md prescribes, and no
	-- more. It must exist, with these default privileges, before the first
	-- migration pass: 0019 grants the account export's two functions to the
	-- role it finds holding SELECT and INSERT on events, at the moment it
	-- runs. Default privileges IN SCHEMA public cover that schema's tables
	-- and sequences only; a migration creating another schema needs its own
	-- grants here.
	CREATE ROLE app_role LOGIN PASSWORD '${APP_ROLE_PASSWORD}' NOSUPERUSER NOBYPASSRLS;
	ALTER DEFAULT PRIVILEGES FOR ROLE migration_role IN SCHEMA public
	    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO app_role;
	ALTER DEFAULT PRIVILEGES FOR ROLE migration_role IN SCHEMA public
	    GRANT USAGE, SELECT ON SEQUENCES TO app_role;
SQL
