#!/usr/bin/env bash
# The alerts that send a mail (#437): the rules of
# platform/components/observability/alerts.yaml are valid
# (`promtool check rules`), and each fires on what it is about and only
# on it (`promtool test rules` on ci/alerts-test.yaml); Alertmanager's
# configuration, filled by alertmanager-config.sh, is valid (`amtool
# check-config`) and mails those alerts, and only those. Needs promtool,
# amtool and yq (mikefarah, v4) on PATH; the CI installs them with
# ci/install-tools.sh.
#
#   ci/alerts-test.sh

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
obs="$root/platform/components/observability"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The PrometheusRule's spec is a Prometheus rule file.
yq e 'select(.kind == "PrometheusRule") | .spec' "$obs/alerts.yaml" > "$work/rules.yaml"
cp "$root/ci/alerts-test.yaml" "$work/alerts-test.yaml"
promtool check rules "$work/rules.yaml"

# Every alert that mails has a test, and every test is of such an alert.
rules=$(yq e -N '.groups[].rules[] | select(.labels.notify == "mail") | .alert' "$work/rules.yaml" | sort -u)
tested=$(yq e -N '.tests[].alert_rule_test[].alertname' "$work/alerts-test.yaml" | sort -u)
if [ "$rules" != "$tested" ]; then
  echo "alerts that mail: $(tr '\n' ' ' <<< "$rules")" >&2
  echo "alerts tested:    $(tr '\n' ' ' <<< "$tested")" >&2
  exit 1
fi

promtool test rules "$work/alerts-test.yaml"

# Alertmanager's configuration, filled as for a cluster.
SMTP_HOST=smtp.example.invalid SMTP_FROM=alerts@example.invalid SMTP_USERNAME=user \
  SMTP_PASSWORD=password ALERT_TO=someone@example.invalid \
  "$obs/alertmanager-config.sh" > "$work/alertmanager.yaml"
amtool check-config "$work/alertmanager.yaml"
if grep -v '^ *#' "$work/alertmanager.yaml" | grep -E 'SMTP_|ALERT_TO'; then
  echo "a placeholder is left in Alertmanager's configuration" >&2
  exit 1
fi
route() { amtool config routes test --config.file="$work/alertmanager.yaml" "$@"; }
failures=0
for a in $rules; do
  labels=$(yq e -N ".groups[].rules[] | select(.alert == \"$a\") | .labels | to_entries | map(.key + \"=\" + .value) | join(\" \")" "$work/rules.yaml")
  # shellcheck disable=SC2086 # one argument per label
  v=$(route alertname="$a" $labels)
  [ "$v" = mail ] || { echo "$a ($labels) goes to '$v', expected mail" >&2; failures=$((failures + 1)); }
done
# The chart's own alerts, which carry no `notify`, mail nothing.
for a in "alertname=Watchdog severity=none" "alertname=KubePodCrashLooping severity=warning" "alertname=TargetDown severity=critical"; do
  # shellcheck disable=SC2086 # one argument per label
  v=$(route $a)
  [ "$v" = null ] || { echo "$a goes to '$v', expected null" >&2; failures=$((failures + 1)); }
done
[ "$failures" = 0 ]
echo "alerts-test: OK"
