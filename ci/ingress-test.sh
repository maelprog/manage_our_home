#!/usr/bin/env bash
# The ingress (#434), on the k3d cluster ci/staging-smoke.sh left running,
# reached the way a client reaches it: the node's address, ports 80 and 443
# (k3s's ServiceLB), the host as SNI and Host. Checks that:
#   - Traefik runs with the settings of platform/components/traefik, and
#     serves the application's certificate for its host;
#   - the security headers of `/`, `/login` and `/api/healthz` are those
#     of the Compose stack, character for character: the same requests go
#     through Caddy with main's infra/Caddyfile (ci/caddy.yaml), in front
#     of the same pods;
#   - plain HTTP redirects to HTTPS; `/readyz` and `/api/readyz` are
#     refused from outside (403);
#   - an answer to a request with a body reaches an HTTP/1.1 client when
#     it is sent, not once the client has sent the whole body (#244): a
#     401 within 5 s while the body still trickles in, for a POST and for
#     a GET that announces a body;
#   - the two upload routes refuse a body over 21 037 056 bytes with a 413
#     at once, and let one of exactly that size through to the api;
#   - responses are compressed, zstd then gzip, the stylesheet included
#     and its Cache-Control kept, a font not;
#   - the login throttle counts the client's address, not the ingress's:
#     an address locked out by 10 failed logins stays locked whatever
#     X-Forwarded-For it sends, while another client, same email, is not;
#   - the Messagerie's WebSocket outlives the 5 min idle timeout: opened,
#     left silent 5 min 20 s, it still delivers a message posted then;
#   - a client trickling its headers over HTTP/1.1 is cut, at the 2 min
#     read timeout (Traefik has no bound for headers alone; over HTTP/2
#     the 5 min idle timeout ends it: README.md, "Ingress").
# The last two run side by side, from the start: the job waits about 6
# minutes for the first.
#
#   ci/ingress-test.sh <path to infra/Caddyfile of main>
#
# Needs kubectl, yq, curl, python3, openssl and docker (a second client, on
# the cluster's Docker network).

set -euo pipefail

caddyfile=$1
root=$(cd "$(dirname "$0")/.." && pwd)
ns=manage-our-home
work=$(mktemp -d)
pf_pid=""
bg_pids=()

# A second client for the login throttle, on the cluster's Docker network
# rather than the runner: another source address.
CURL_IMAGE=curlimages/curl:8.22.0@sha256:58adaa4e8dca9c988bae2aba4ab3434a0bb2da16bbe3f92dec39ec7785166777
# The timeouts of platform/components/traefik/helm-chart-config.yaml.
READ_TIMEOUT=120
IDLE_TIMEOUT=300

on_exit() {
  local status=$?
  if [ "$status" != 0 ]; then
    echo "::group::diagnostics"
    kubectl -n kube-system get deploy/traefik svc/traefik -o wide || true
    kubectl -n kube-system logs deploy/traefik --tail=80 || true
    kubectl -n "$ns" get ingressroutes.traefik.io,middlewares.traefik.io,certificates.cert-manager.io -o wide || true
    kubectl -n ci-caddy logs deploy/caddy --tail=40 || true
    for f in "$work"/*.log; do [ -e "$f" ] && { echo "--- $f"; cat "$f"; }; done
    echo "::endgroup::"
  fi
  for p in "${bg_pids[@]}" $pf_pid; do kill "$p" 2> /dev/null || true; done
  rm -rf "$work"
  exit "$status"
}
trap on_exit EXIT

failures=0
fail() { echo "::error::$*"; failures=$((failures + 1)); }

host=$(kubectl -n "$ns" get certificates.cert-manager.io manage-our-home-tls -o jsonpath='{.spec.dnsNames[0]}')
node=$(kubectl get nodes -o jsonpath='{.items[0].metadata.name}')
address=$(kubectl get node "$node" -o jsonpath='{.status.addresses[?(@.type=="InternalIP")].address}')
echo "host $host, node $node at $address"

# Through the ingress: `ingress <path> [curl options]`.
ingress() {
  local path=$1
  shift
  curl --silent --insecure --resolve "$host:443:$address" "https://$host$path" "$@"
}
headers() { # file -> its lines `name: value`, names lowercased, sorted
  tr -d '\r' < "$1" | awk -F': ' 'NF > 1 { printf "%s: %s\n", tolower($1), substr($0, length($1) + 3) }' | LC_ALL=C sort
}

echo "== Traefik's settings"
# k3s's helm-controller upgrades Traefik after the HelmChartConfig: wait
# until the Deployment carries its values, then for the rollout.
for i in $(seq 1 60); do
  args=$(kubectl -n kube-system get deploy/traefik -o jsonpath='{.spec.template.spec.containers[0].args}')
  [[ "$args" == *"websecure.transport.respondingTimeouts.readTimeout="* ]] && break
  [ "$i" = 60 ] && { fail "Traefik does not run with the HelmChartConfig's timeouts: $args"; exit 1; }
  sleep 5
done
kubectl -n kube-system rollout status deploy/traefik --timeout=180s
for want in web.transport.respondingTimeouts.readTimeout=2m websecure.transport.respondingTimeouts.readTimeout=2m \
  web.transport.respondingTimeouts.idleTimeout=5m websecure.transport.respondingTimeouts.idleTimeout=5m; do
  [[ "$args" == *"$want"* ]] || fail "Traefik's arguments lack $want"
done
v=$(kubectl -n kube-system get svc/traefik -o jsonpath='{.spec.externalTrafficPolicy}')
[ "$v" = Local ] || fail "Traefik's Service has externalTrafficPolicy '$v', expected Local"
kubectl -n "$ns" wait certificates.cert-manager.io/manage-our-home-tls --for=condition=Ready --timeout=120s
# The routes reach the pods once Traefik has read them: wait for a 200.
for i in $(seq 1 60); do
  code=$(ingress /api/healthz --output /dev/null --write-out '%{http_code}' || true)
  [ "$code" = 200 ] && break
  [ "$i" = 60 ] && { fail "https://$host/api/healthz answers $code through the ingress"; exit 1; }
  sleep 2
done
subject=$(openssl s_client -connect "$address:443" -servername "$host" < /dev/null 2> /dev/null \
  | openssl x509 -noout -ext subjectAltName 2> /dev/null || true)
[[ "$subject" == *"DNS:$host"* ]] || fail "the certificate served for $host is not the application's: '$subject'"

# The two long probes, from now on: the WebSocket needs a session first.
echo "== a member with a family, for the WebSocket"
email="ci-ingress-$(openssl rand -hex 4)@example.test"
password=$(openssl rand -hex 16)
jar="$work/cookies"
code=$(ingress /api/auth/register --output "$work/register.log" --write-out '%{http_code}' \
  -H 'Content-Type: application/json' \
  -d "{\"email\":\"$email\",\"password\":\"$password\",\"display_name\":\"CI ingress\",\"declares_minimum_age\":true,\"accepts_terms\":true}")
[ "$code" = 201 ] || { fail "register: $code"; exit 1; }
# The verification mail goes nowhere here (SMTP_HOST is .invalid): the
# address is verified in the database, as the e2e suite of main does with
# the token (e2e/lib/db.ts).
primary=$(kubectl -n "$ns" get clusters.postgresql.cnpg.io postgres -o jsonpath='{.status.currentPrimary}')
v=$(kubectl -n "$ns" exec "$primary" -c postgres -- psql -X -At -d manage_our_home \
  -c "UPDATE users SET email_verified = true WHERE email = '$email'")
[ "$v" = "UPDATE 1" ] || { fail "verifying $email: $v"; exit 1; }
code=$(ingress /api/auth/login --cookie-jar "$jar" --output "$work/login.log" --write-out '%{http_code}' \
  -H 'Content-Type: application/json' -d "{\"email\":\"$email\",\"password\":\"$password\"}")
[ "$code" = 200 ] || { fail "login: $code"; exit 1; }
group=$(ingress /api/groups --cookie "$jar" -H 'Content-Type: application/json' -d '{"name":"CI ingress"}' \
  | python3 -c 'import json, sys; print(json.load(sys.stdin)["id"])')
cookie=$(awk '$6 ~ /session_id$/ { print $6 "=" $7 }' "$jar")
[ -n "$cookie" ] || { fail "no session cookie in the login's answer"; exit 1; }

echo "== WebSocket and trickled headers, in the background"
marker="ci-ingress-$(openssl rand -hex 4)"
python3 "$root/ci/ingress-probe.py" ws "$address" "$host" "/api/groups/$group/messages/ws" "$cookie" "$marker" \
  $((IDLE_TIMEOUT + 120)) > "$work/ws.log" 2>&1 &
ws_pid=$!
bg_pids+=("$ws_pid")
python3 "$root/ci/ingress-probe.py" drip "$address" "$host" $((READ_TIMEOUT + 90)) > "$work/drip.log" 2>&1 &
drip_pid=$!
bg_pids+=("$drip_pid")
drip_start=$(date +%s)
for i in $(seq 1 30); do
  grep -q '^open$' "$work/ws.log" && break
  [ "$i" = 30 ] && { fail "the WebSocket did not open: $(cat "$work/ws.log")"; exit 1; }
  sleep 1
done
ws_open=$(date +%s)

echo "== security headers: the ingress against main's Caddyfile"
kubectl apply -f - <<< "$(yq e 'select(.kind == "Namespace")' "$root/ci/caddy.yaml")"
kubectl -n ci-caddy create configmap caddyfile --from-file=Caddyfile="$caddyfile" --dry-run=client -o yaml | kubectl apply -f -
kubectl apply -f "$root/ci/caddy.yaml"
kubectl -n ci-caddy rollout status deploy/caddy --timeout=180s
kubectl -n ci-caddy port-forward svc/caddy 18081:80 > "$work/pf-caddy.log" 2>&1 &
pf_pid=$!
for i in $(seq 1 30); do
  curl --silent --output /dev/null http://127.0.0.1:18081/api/healthz && break
  [ "$i" = 30 ] && { fail "Caddy does not answer"; exit 1; }
  sleep 1
done
SECURITY='^(content-security-policy|reporting-endpoints|strict-transport-security|x-content-type-options|x-frame-options|referrer-policy): '
for path in / /login /api/healthz; do
  ingress "$path" --output /dev/null --dump-header "$work/traefik.h"
  curl --silent --output /dev/null --dump-header "$work/caddy.h" "http://127.0.0.1:18081$path"
  headers "$work/traefik.h" | grep -E "$SECURITY" > "$work/traefik.sec" || true
  headers "$work/caddy.h" | grep -E "$SECURITY" > "$work/caddy.sec" || true
  n=$(wc -l < "$work/caddy.sec")
  if [ "$n" != 6 ]; then
    fail "$path: Caddy sent $n of the 6 security headers — is $caddyfile main's?"
  elif ! diff -u "$work/caddy.sec" "$work/traefik.sec"; then
    fail "$path: the ingress's security headers differ from Caddy's (- Caddy, + ingress)"
  else
    echo "$path: the 6 security headers are Caddy's"
  fi
  # Every other header, for the record: what differs there is no guard.
  diff <(headers "$work/caddy.h" | cut -d: -f1 | sort -u) <(headers "$work/traefik.h" | cut -d: -f1 | sort -u) \
    | sed -n 's/^</  only from Caddy:/p; s/^>/  only from the ingress:/p' || true
done

echo "== HTTP, /readyz"
v=$(curl --silent --resolve "$host:80:$address" --output /dev/null --write-out '%{http_code} %{redirect_url}' "http://$host/login?next=x")
[ "$v" = "301 https://$host/login?next=x" ] || fail "plain HTTP answers '$v', expected a 301 to https://$host/login?next=x"
for path in /readyz /api/readyz; do
  code=$(ingress "$path" --output /dev/null --write-out '%{http_code}')
  [ "$code" = 403 ] || fail "$path answers $code through the ingress, expected 403"
done

echo "== an answer before the body's end (#244), over HTTP/1.1"
# apps/api answers 401 to a request without a session before reading its
# body. Go's server, Traefik's, holds such an answer until it has read the
# rest of the body (under 256 KiB), unless the answer says
# `Connection: close` (middlewares.yaml, connection-close).
probe_post() { # path length [method] -> `<status line> after <s> s, ...`
  python3 "$root/ci/ingress-probe.py" post "$address" "$host" "$1" "$2" 30 "${3:-POST}" || true
}
within() { # answer status seconds: the answer has that status, within that time
  [[ "$1" =~ ^HTTP/1\.1\ $2\ .*\ after\ ([0-9.]+)\ s ]] && awk -v t="${BASH_REMATCH[1]}" -v m="$3" 'BEGIN { exit !(t < m) }'
}
v=$(probe_post /api/groups 100000)
echo "POST /api/groups, 100 kB at 2 kB/s: $v"
within "$v" 401 5 || fail "POST /api/groups: '$v', expected its 401 within 5 s, before the body's end"
# A GET that announces a body (Content-Length) too: the Caddyfile's
# close_on_408 closed after any 408, a GET's included.
v=$(probe_post /api/groups 100000 GET)
echo "GET /api/groups, 100 kB at 2 kB/s: $v"
within "$v" 401 5 || fail "GET /api/groups with a body: '$v', expected its 401 within 5 s, before the body's end"

echo "== upload size (#219)"
# One byte over storage::MAX_UPLOAD_BODY_BYTES of main (20 MiB + 64 KiB of
# multipart framing): refused at the ingress, before the body is read.
over=21037057
fake=00000000-0000-0000-0000-000000000000
for path in "/api/groups/$fake/events/$fake/attachments" "/agenda/$fake/attachments"; do
  v=$(probe_post "$path" "$over")
  echo "POST $path, $over bytes announced: $v"
  within "$v" 413 5 || fail "POST $path with $over bytes: '$v', expected a 413 within 5 s"
done
# At the limit, the ingress lets the body by: Traefik reads it whole, then
# passes it to the api, which answers 401 (no session) without reading it
# and closes. Depending on when that close reaches Traefik, still sending
# the buffered body, the client gets the api's 401 or Traefik's 502 (both
# seen in k3d) — either way past the limit, never its 413.
head -c $((over - 1)) /dev/zero > "$work/limit.bin"
code=$(ingress "/api/groups/$fake/events/$fake/attachments" --output /dev/null --write-out '%{http_code}' \
  -H 'Content-Type: application/octet-stream' --data-binary @"$work/limit.bin")
rm -f "$work/limit.bin"
echo "POST of $((over - 1)) bytes: $code"
[ "$code" = 401 ] || [ "$code" = 502 ] \
  || fail "a body of exactly $((over - 1)) bytes got $code, expected to pass the limit (the api's 401, or a 502)"

echo "== compression"
enc() { # path accept-encoding -> Content-Encoding (or `none`)
  ingress "$1" --output /dev/null --dump-header - -H "Accept-Encoding: $2" \
    | tr -d '\r' | awk -F': ' 'tolower($1) == "content-encoding" { e = $2 } END { print (e == "" ? "none" : e) }'
}
v=$(enc /login 'zstd, gzip')
[ "$v" = zstd ] || fail "/login: Content-Encoding '$v' for zstd, gzip; expected zstd"
v=$(enc /login gzip)
[ "$v" = gzip ] || fail "/login: Content-Encoding '$v' for gzip alone"
v=$(enc /login identity)
[ "$v" = none ] || fail "/login: Content-Encoding '$v' for identity"
css=$(ingress /login | grep -oE '/assets/style-[0-9a-f]+\.css' | head -n 1)
if [ -z "$css" ]; then
  fail "no stylesheet linked from /login"
else
  v=$(enc "$css" 'zstd, gzip')
  [ "$v" = zstd ] || fail "$css: Content-Encoding '$v', expected zstd"
  v=$(ingress "$css" --output /dev/null --dump-header - -H 'Accept-Encoding: zstd, gzip' | tr -d '\r' | grep -i '^cache-control:' || true)
  [[ "$v" == *immutable* ]] || fail "$css: Cache-Control '$v' lost its immutable"
  font=$(ingress "$css" --compressed | grep -oE '/assets/[^)"'"'"' ]+\.woff2' | head -n 1)
  if [ -z "$font" ]; then
    fail "no .woff2 font named by $css: the check that fonts are not compressed again has nothing to check"
  else
    v=$(enc "$font" 'zstd, gzip')
    [ "$v" = none ] || fail "$font: a font compressed again ($v)"
  fi
fi

echo "== login throttle: the client's address"
# An address with no account: the throttle counts (address, email) pairs
# whether the account exists or not, and the member's session stays out
# of it.
throttled="ci-throttle-$(openssl rand -hex 4)@example.test"
attempt='{"email":"'"$throttled"'","password":"wrong"}'
login_code() { # curl options...
  curl --silent --insecure --output /dev/null --write-out '%{http_code}' "$@" \
    -H 'Content-Type: application/json' -d "$attempt"
}
first_429=""
for i in $(seq 1 12); do
  code=$(login_code --resolve "$host:443:$address" "https://$host/api/auth/login")
  echo "runner, attempt $i: $code"
  if [ "$code" = 429 ]; then
    first_429=$i
    break
  fi
done
[ -n "$first_429" ] || fail "12 failed logins from one address, none refused 429"
code=$(login_code --resolve "$host:443:$address" -H 'X-Forwarded-For: 203.0.113.7' "https://$host/api/auth/login")
[ "$code" = 429 ] || fail "the locked-out address got $code by sending X-Forwarded-For: 203.0.113.7"
network=$(docker inspect "$node" --format '{{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}' | awk '{ print $1 }')
code=$(docker run --rm --network "$network" "$CURL_IMAGE" --silent --insecure --output /dev/null --write-out '%{http_code}' \
  --resolve "$host:443:$address" "https://$host/api/auth/login" \
  -H 'Content-Type: application/json' -d "$attempt")
echo "another client, same email: $code"
[ "$code" = 401 ] || fail "another client got $code after the runner was locked out, expected 401: the api sees one address for both"

echo "== WebSocket after $((IDLE_TIMEOUT + 20)) s of silence"
left=$((ws_open + IDLE_TIMEOUT + 20 - $(date +%s)))
[ "$left" -le 0 ] || sleep "$left"
code=$(ingress "/api/groups/$group/messages" --cookie "$jar" --output /dev/null --write-out '%{http_code}' \
  -H 'Content-Type: application/json' -d "{\"content\":\"$marker\"}")
[ "$code" = 201 ] || fail "posting the message: $code"
if wait "$ws_pid"; then
  echo "WebSocket: $(tail -n 1 "$work/ws.log")"
else
  fail "WebSocket: $(cat "$work/ws.log")"
fi

echo "== trickled headers (cut by $((READ_TIMEOUT + 30)) s, given up at $((READ_TIMEOUT + 90)) s)"
drip_status=0
wait "$drip_pid" || drip_status=$?
cat "$work/drip.log"
if [ "$drip_status" != 0 ]; then
  fail "a client trickling its headers was not cut"
else
  t=$(sed -n 's/^cut after \([0-9]*\) s.*/\1/p' "$work/drip.log")
  # Cut by the read timeout of the HelmChartConfig, neither Traefik's 60 s
  # default nor a forgotten longer one.
  [ "$t" -ge $((READ_TIMEOUT - 10)) ] && [ "$t" -le $((READ_TIMEOUT + 30)) ] \
    || fail "trickled headers cut after $t s, expected the ${READ_TIMEOUT} s read timeout"
fi
echo "(started $(($(date +%s) - drip_start)) s ago)"

[ "$failures" = 0 ] || { echo "ingress-test: $failures failure(s)"; exit 1; }
echo "ingress: OK"
