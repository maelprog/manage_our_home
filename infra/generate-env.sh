#!/usr/bin/env bash
# Generates a production-grade infra/.env with random secrets, for the same
# variable set docker-compose.yml consumes. For local testing, prefer
# `cp .env.example .env` (fixed throwaway values).

#
# Usage: ./generate-env.sh <domain>     e.g. ./generate-env.sh maison.example.org
#
# The domain is the public name the stack is served at (#141). It becomes
# Caddy's site address, which turns on automatic HTTPS, and the https://
# origin the applications build their links from. Without HTTPS the
# Secure session cookie is refused and nobody can sign in, so there is no
# plain-HTTP variant of this file.

set -euo pipefail

# A fully qualified host name and nothing else: dot-separated labels of
# letters, digits and inner hyphens, ending in an alphabetic TLD. That keeps
# out an option (`-h`), a wildcard (`*.example.org`, which Caddy would only
# serve with a DNS challenge this stack does not configure), a scheme, a
# port, a path and an IP address.
HOST_RE='^([A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?\.)+[A-Za-z]{2,63}$'
DOMAIN="${1:-}"
if [[ $# -ne 1 || ! "$DOMAIN" =~ $HOST_RE ]]; then
    echo "usage: $0 <domain>   (a bare host name, e.g. maison.example.org)" >&2
    exit 1
fi

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

# A P-256 private scalar, unpadded base64url: the 32 bytes after the 7-byte
# header of the SEC1 DER key (30 77 02 01 01 04 20).
gen_vapid_key() {
    openssl ecparam -name prime256v1 -genkey -noout -outform DER \
        | tail -c +8 | head -c 32 | basenc --base64url | tr -d '='
}

cat > "$ENV_FILE" <<EOF
########################################
# Application
########################################

# Caddy's site address: a domain name turns on automatic HTTPS. It must
# resolve to this host, with ports 80 and 443 reachable from the Internet.
SITE_ADDRESS=$DOMAIN

# Public origin the stack is reached at (Caddy). No trailing slash.
PUBLIC_BASE_URL=https://$DOMAIN

# Defaults to true when unset; set to false only for local http testing.
#SECURE_COOKIES=false

########################################
# PostgreSQL
########################################
# DB name (manage_our_home) and bootstrap superuser (mhome) are fixed in
# docker-compose.yml. The API does not connect as mhome: it serves requests
# as app_role (APP_ROLE_PASSWORD below, #311).

POSTGRES_PASSWORD=$(gen_pwd)

# Roles created at first postgres boot by postgres/init/01-roles.sh:
# app_role serves requests under RLS (NOSUPERUSER NOBYPASSRLS, #311), and
# two BYPASSRLS roles: migration_role applies the migrations and owns the
# tables (issue #105), admin_role serves the superadmin endpoints and the
# background jobs (#215). All three are described in apps/api/README.md.
APP_ROLE_PASSWORD=$(gen_pwd)
MIGRATION_ROLE_PASSWORD=$(gen_pwd)
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
# Reminder notifications (Web Push, #306)
########################################
# The VAPID key every push is signed with. Keep it: a new key voids every
# device's subscription (apps/api/README.md).

VAPID_PRIVATE_KEY=$(gen_vapid_key)
VAPID_SUBJECT=mailto:admin@$DOMAIN

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
