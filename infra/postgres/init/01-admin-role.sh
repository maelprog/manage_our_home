#!/bin/bash
# Runs once, at first boot of the postgres container (docker-entrypoint-initdb.d,
# mounted from docker-compose.yml). Creates the BYPASSRLS role that
# ADMIN_DATABASE_URL connects as (superadmin endpoints — see apps/api/README.md,
# Epic #8). The API's admin pool connects eagerly at startup, so this role must
# exist before the api service comes up.
#
# Tables don't exist yet at this point (sqlx migrations run at API startup, as
# $POSTGRES_USER), so the grant goes through default privileges instead of
# GRANT ... ON ALL TABLES.
set -euo pipefail

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" <<-SQL
	CREATE ROLE admin_role LOGIN PASSWORD '${ADMIN_ROLE_PASSWORD}' NOSUPERUSER BYPASSRLS;
	ALTER DEFAULT PRIVILEGES FOR ROLE ${POSTGRES_USER} IN SCHEMA public
	    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO admin_role;
SQL
