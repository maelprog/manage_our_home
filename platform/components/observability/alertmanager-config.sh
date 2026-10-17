#!/usr/bin/env bash
# Prints Alertmanager's configuration (#437): alertmanager.template.yaml,
# next to this file, with the values of one cluster, read from the
# environment. Its output is what is sealed into the Secret
# `alertmanager-config` (platform/README.md, "Observabilité"); the k3d
# job fills it the same way, with a throwaway mail catcher.
#
#   SMTP_HOST=… SMTP_FROM=… SMTP_USERNAME=… SMTP_PASSWORD=… ALERT_TO=… \
#     platform/components/observability/alertmanager-config.sh
#
# SMTP_PORT (587) and SMTP_REQUIRE_TLS (true) may be set too. Needs yq
# (mikefarah, v4). Writes nothing to disk.

set -euo pipefail

: "${SMTP_HOST:?}" "${SMTP_FROM:?}" "${SMTP_USERNAME:?}" "${SMTP_PASSWORD:?}" "${ALERT_TO:?}"
export SMTP_PORT=${SMTP_PORT:-587} SMTP_REQUIRE_TLS=${SMTP_REQUIRE_TLS:-true}
here=$(cd "$(dirname "$0")" && pwd)

yq e '
  .global.smtp_smarthost = strenv(SMTP_HOST) + ":" + strenv(SMTP_PORT) |
  .global.smtp_from = strenv(SMTP_FROM) |
  .global.smtp_auth_username = strenv(SMTP_USERNAME) |
  .global.smtp_auth_password = strenv(SMTP_PASSWORD) |
  .global.smtp_require_tls = (strenv(SMTP_REQUIRE_TLS) == "true") |
  (.receivers[] | select(.name == "mail") | .email_configs[0].to) = strenv(ALERT_TO)
' "$here/alertmanager.template.yaml"
