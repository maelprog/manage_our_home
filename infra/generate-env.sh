#!/usr/bin/env bash
# Generates a production-grade infra/.env with random secrets, for the same
# variable set docker-compose.yml consumes. For local testing, prefer
# `cp .env.example .env` (fixed throwaway values).

set -euo pipefail

# Output file
ENV_FILE=".env"

# Helper functions
gen_key() {
    openssl rand -base64 32
}

# Passwords end up embedded in postgres:// URLs (docker-compose.yml), so
# keep them hex — base64's '+', '/' and '=' break URL parsing.
gen_pwd() {
    openssl rand -hex 24
}

cat > "$ENV_FILE" <<EOF
########################################
# Application
########################################

# Public origin the stack is reached at (Caddy). No trailing slash.
PUBLIC_BASE_URL=http://localhost

# Defaults to true when unset; set to false only for local http testing.
#SECURE_COOKIES=false

########################################
# PostgreSQL
########################################
# DB name (manage_our_home) and app user (mhome) are fixed in docker-compose.yml.

POSTGRES_PASSWORD=$(gen_pwd)

# BYPASSRLS role created at first postgres boot by
# postgres/init/01-admin-role.sh (superadmin endpoints, apps/api/README.md).
ADMIN_ROLE_PASSWORD=$(gen_pwd)

########################################
# Google OAuth
########################################

GOOGLE_CLIENT_ID=
GOOGLE_CLIENT_SECRET=

########################################
# Encryption keys
########################################

OAUTH_ENCRYPTION_KEY=$(gen_key)
MESSAGE_ENCRYPTION_KEY=$(gen_key)
CALENDAR_FEED_ENCRYPTION_KEY=$(gen_key)

########################################
# SMTP
########################################
# SMTP_FROM must parse as a mailbox (e.g. no-reply@example.com) or the API
# exits at startup.
# The dev-only knobs from .env.example (COMPOSE_PROFILES, SMTP_PORT,
# SMTP_ALLOW_INSECURE, DEV_SEED_USERS) are deliberately absent: real
# deployments must use the TLS relay path and never seed dev accounts.

SMTP_HOST=
SMTP_USERNAME=
SMTP_PASSWORD=
SMTP_FROM=

########################################
# MinIO
########################################

MINIO_ROOT_USER=minioadmin
MINIO_ROOT_PASSWORD=$(gen_pwd)

EOF

echo "✅ Generated $ENV_FILE — fill in the empty GOOGLE_* and SMTP_* values."
