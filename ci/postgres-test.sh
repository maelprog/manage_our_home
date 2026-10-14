#!/usr/bin/env bash
# Staging's Postgres (#433), on the k3d cluster ci/staging-smoke.sh has
# just brought up — CloudNativePG Cluster `postgres`, api and web running
# on it, its backups going to the throwaway MinIO of ci/minio.yaml:
#
#   1. Roles: app_role, migration_role and admin_role, their attributes,
#      the default privileges, the grants on the schema `public`, the
#      extensions, the encoding and the locale are the same as on the
#      reference Postgres that infra/postgres/init/01-roles.sh of main
#      initialised (ci/postgres.yaml).
#   2. RLS: app_role neither bypasses RLS nor owns a table, and the
#      policies apply to it: a group's membership is invisible to it
#      without that group's `app.family_id`, visible with it.
#   3. Restore (README.md, "Restaurer"): a row written before an instant T
#      and one written after; Argo CD paused and the api stopped, the
#      Cluster is deleted, volume included, and recreated from the object
#      store at T with the changes the procedure makes. The
#      first row is there, the second is not, the roles are still those of
#      01-roles.sh, RLS still applies, the restored Cluster archives again,
#      and the api answers /readyz again.
#
#   ci/postgres-test.sh
#
# Needs kubectl, kustomize, yq and curl, and the kube context of the k3d
# cluster.

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
ns=manage-our-home
work=$(mktemp -d)
pf_pid=""

on_exit() {
  local status=$?
  if [ "$status" != 0 ]; then
    echo "::group::diagnostics"
    kubectl -n "$ns" get clusters.postgresql.cnpg.io,backups.postgresql.cnpg.io,pods -o wide || true
    kubectl -n "$ns" get clusters.postgresql.cnpg.io postgres -o yaml | yq e '.status' - || true
    for p in $(kubectl -n "$ns" get pods -l cnpg.io/cluster=postgres -o name 2> /dev/null); do
      echo "--- logs $p"
      kubectl -n "$ns" logs "$p" --all-containers --tail=80 || true
    done
    kubectl -n cnpg-system logs deploy/cnpg-cloudnative-pg --tail=80 || true
    kubectl get events -n "$ns" --sort-by=.lastTimestamp | tail -n 40 || true
    echo "::endgroup::"
  fi
  [ -z "$pf_pid" ] || kill "$pf_pid" 2> /dev/null || true
  rm -rf "$work"
  exit "$status"
}
trap on_exit EXIT

# SQL as the superuser, through the primary's local socket: the Cluster
# has no superuser reachable over the network (enableSuperuserAccess:
# false). One value per line, columns separated by `|`.
primary() { kubectl -n "$ns" get clusters.postgresql.cnpg.io postgres -o jsonpath='{.status.currentPrimary}'; }
cnpg() { kubectl -n "$ns" exec -i "$(primary)" -c postgres -- psql -X -q -v ON_ERROR_STOP=1 -At -d manage_our_home "$@"; }
reference() { kubectl -n ci-postgres exec -i deploy/postgres -- psql -X -q -v ON_ERROR_STOP=1 -At -U mhome -d manage_our_home "$@"; }

# Everything 01-roles.sh decides, nothing it does not (the passwords, which
# differ by construction, and validity dates).
ROLES_SQL="
SELECT 'role', rolname, rolsuper, rolinherit, rolcreaterole, rolcreatedb, rolcanlogin, rolreplication, rolbypassrls, rolconnlimit
  FROM pg_roles WHERE rolname IN ('app_role', 'migration_role', 'admin_role') ORDER BY rolname;
SELECT 'member', r.rolname, m.rolname FROM pg_auth_members a
  JOIN pg_roles r ON r.oid = a.member JOIN pg_roles m ON m.oid = a.roleid
  WHERE r.rolname IN ('app_role', 'migration_role', 'admin_role') ORDER BY 2, 3;
SELECT 'default_acl', pg_get_userbyid(defaclrole), defaclnamespace::regnamespace, defaclobjtype, defaclacl::text
  FROM pg_default_acl ORDER BY 2, 3, 4;
SELECT 'schema_public', nspacl::text FROM pg_namespace WHERE nspname = 'public';
SELECT 'extension', extname FROM pg_extension ORDER BY extname;
SELECT 'database', pg_encoding_to_char(encoding), datcollate, datctype FROM pg_database WHERE datname = current_database();
"

check_roles() { # label
  echo "== roles ($1): compared with 01-roles.sh"
  reference <<< "$ROLES_SQL" > "$work/roles.reference"
  cnpg <<< "$ROLES_SQL" > "$work/roles.cnpg"
  cat "$work/roles.cnpg"
  grep -q '^role|app_role|' "$work/roles.reference" || { echo "::error::the reference Postgres has no app_role"; exit 1; }
  if ! diff -u "$work/roles.reference" "$work/roles.cnpg"; then
    echo "::error::the roles of the Cluster differ from those of 01-roles.sh (- reference, + Cluster)"
    exit 1
  fi
  # The database's owner, which 01-roles.sh does not create, never logs in.
  local v
  v=$(cnpg -c "SELECT rolcanlogin::text || ' ' || rolsuper::text FROM pg_roles r JOIN pg_database d ON d.datdba = r.oid WHERE d.datname = current_database() AND r.rolname = 'mhome'")
  [ "$v" = "false false" ] || { echo "::error::owner mhome: login, superuser = '$v', expected 'false false'"; exit 1; }
  echo "owner mhome: no login"
}

# A user, a group and its membership, written as migration_role (which
# bypasses RLS, as the migrations do). Prints the group's id.
add_group() { # tag
  cnpg << SQL | tail -n 1
SET ROLE migration_role;
WITH u AS (
  INSERT INTO users (email, password_hash, display_name)
  VALUES ('ci-$1@example.invalid', 'not-a-hash', 'ci $1') RETURNING id
), g AS (
  INSERT INTO groups (name, created_by) SELECT 'ci $1', id FROM u RETURNING id, created_by
)
INSERT INTO group_members (group_id, user_id, role) SELECT id, created_by, 'owner' FROM g RETURNING group_id;
SQL
}
count_users() { # tag
  cnpg -c "SELECT count(*) FROM users WHERE email = 'ci-$1@example.invalid'"
}

check_rls() { # label
  echo "== RLS ($1)"
  local v gid
  v=$(cnpg -c "SELECT rolbypassrls::text || ' ' || rolsuper::text FROM pg_roles WHERE rolname = 'app_role'")
  [ "$v" = "false false" ] || { echo "::error::app_role: bypassrls, superuser = '$v'"; exit 1; }
  v=$(cnpg -c "SELECT count(*) FROM pg_tables WHERE schemaname = 'public'")
  [ "$v" -gt 0 ] || { echo "::error::no table in public: the migrations did not run"; exit 1; }
  v=$(cnpg -c "SELECT coalesce(string_agg(tablename || ' (' || tableowner || ')', ', '), '') FROM pg_tables WHERE schemaname = 'public' AND tableowner <> 'migration_role'")
  [ -z "$v" ] || { echo "::error::tables not owned by migration_role: $v"; exit 1; }
  v=$(cnpg -c "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'public' AND c.relkind = 'r' AND c.relrowsecurity AND c.relforcerowsecurity")
  [ "$v" -gt 0 ] || { echo "::error::no table under forced RLS"; exit 1; }
  echo "app_role: no BYPASSRLS, owns no table; $v tables under forced RLS"
  gid=$(add_group "rls-$1")
  v=$(cnpg -c "SET ROLE app_role; SELECT count(*) FROM group_members WHERE group_id = '$gid'" | tail -n 1)
  [ "$v" = 0 ] || { echo "::error::app_role sees $v membership(s) of group $gid without its app.family_id"; exit 1; }
  v=$(cnpg -c "SET ROLE app_role; SELECT set_config('app.family_id', '$gid', false); SELECT count(*) FROM group_members WHERE group_id = '$gid'" | tail -n 1)
  [ "$v" = 1 ] || { echo "::error::app_role sees $v membership(s) of group $gid with its app.family_id, expected 1"; exit 1; }
  echo "app_role: a group's membership hidden without its app.family_id, visible with it"
}

wait_archiving() {
  kubectl -n "$ns" wait clusters.postgresql.cnpg.io/postgres --for=condition=ContinuousArchiving --timeout=300s
}

check_roles "created"
check_rls "created"

# --- Restore.
echo "== a base backup in the object store"
wait_archiving
for _ in $(seq 1 60); do
  done_=$(kubectl -n "$ns" get backups.postgresql.cnpg.io -o yaml | yq e '[.items[] | select(.status.phase == "completed")] | length' -)
  [ "$done_" -gt 0 ] && break
  sleep 5
done
kubectl -n "$ns" get backups.postgresql.cnpg.io
[ "$done_" -gt 0 ] || { echo "::error::no completed base backup"; exit 1; }

echo "== a row before T, a row after"
add_group before > /dev/null
sleep 2
target=$(cnpg -c "SELECT to_char(clock_timestamp() AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS.US') || '+00:00'")
sleep 2
add_group after > /dev/null
echo "T = $target"
# The segment holding the second row is archived: recovery has to read past
# T to stop at it.
segment=$(cnpg -c "SELECT pg_walfile_name(pg_switch_wal())")
for _ in $(seq 1 60); do
  archived=$(cnpg -c "SELECT coalesce(last_archived_wal, '') FROM pg_stat_archiver")
  [[ "$archived" > "$segment" || "$archived" == "$segment" ]] && break
  sleep 5
done
echo "switched to a new segment after $segment; last archived: $archived"
[[ "$archived" > "$segment" || "$archived" == "$segment" ]] || { echo "::error::segment $segment not archived"; exit 1; }

# The procedure of README.md: Argo CD stops syncing (the root first, which
# would set it back on the others), the api stops — no more writes, and
# Postgres's shutdown does not wait on its connections —, the Cluster is
# deleted with its volume, then recreated from the object store at T.
echo "== Argo CD paused, api stopped, Cluster deleted"
for a in platform postgres-staging manage-our-home-staging; do
  kubectl -n argocd patch application "$a" --type merge -p '{"spec":{"syncPolicy":{"automated":null}}}'
done
api_replicas=$(kubectl -n "$ns" get deploy/api -o jsonpath='{.spec.replicas}')
kubectl -n "$ns" scale deploy/api --replicas=0
kubectl -n "$ns" wait --for=delete pod -l app.kubernetes.io/name=api --timeout=120s
old_pvc=$(kubectl -n "$ns" get pvc -l cnpg.io/cluster=postgres -o name)
kubectl -n "$ns" delete clusters.postgresql.cnpg.io postgres --wait=true --timeout=180s
kubectl -n "$ns" wait --for=delete pod -l cnpg.io/cluster=postgres --timeout=180s
for p in $old_pvc; do kubectl -n "$ns" wait --for=delete "$p" --timeout=180s; done
left=$(kubectl -n "$ns" get pvc -l cnpg.io/cluster=postgres -o name)
[ -z "$left" ] || { echo "::error::volumes left behind: $left"; exit 1; }
echo "deleted, volumes included: $old_pvc"

# The overlay as Argo CD renders it in this job (ci/postgres-backup-patch.yaml),
# with the three changes the procedure makes: bootstrap from the object
# store at T, the old backups as its source, a new serverName.
echo "== recreated from the object store at T"
mkdir -p "$work/restore"
cat > "$work/restore/kustomization.yaml" << EOF
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
resources:
  - $root/apps/manage-our-home/overlays/staging/postgres
patches:
  - target: {kind: Cluster, name: postgres}
    path: $root/ci/postgres-backup-patch.yaml
EOF
kustomize build --load-restrictor LoadRestrictionsNone "$work/restore" \
  | TARGET=$target yq e '
      select(.kind == "Cluster") |
      .spec.externalClusters = [{"name": "origin", "barmanObjectStore": .spec.backup.barmanObjectStore}] |
      .spec.bootstrap = {"recovery": {"source": "origin", "database": "manage_our_home", "owner": "mhome",
                                      "recoveryTarget": {"targetTime": strenv(TARGET)}}} |
      .spec.backup.barmanObjectStore.serverName = "postgres-r1"
    ' - > "$work/restore.yaml"
yq e '.spec | {"bootstrap": .bootstrap, "externalClusters": .externalClusters, "serverName": .backup.barmanObjectStore.serverName}' "$work/restore.yaml"
kubectl apply -f "$work/restore.yaml"
kubectl -n "$ns" wait clusters.postgresql.cnpg.io/postgres --for=condition=Ready --timeout=900s
kubectl -n "$ns" get clusters.postgresql.cnpg.io,pods -o wide

echo "== data at T"
before=$(count_users before); after=$(count_users after)
echo "row before T: $before, row after T: $after"
[ "$before" = 1 ] || { echo "::error::the row written before T is missing after the restore"; exit 1; }
[ "$after" = 0 ] || { echo "::error::the row written after T is back: the restore did not stop at T"; exit 1; }

check_roles "restored"
check_rls "restored"
wait_archiving
echo "the restored Cluster archives under its new serverName"

echo "== api /readyz on the restored database"
kubectl -n "$ns" scale deploy/api --replicas="$api_replicas"
kubectl -n "$ns" rollout status deploy/api --timeout=180s
kubectl -n "$ns" port-forward svc/api 18081:8080 > "$work/pf.log" 2>&1 &
pf_pid=$!
code=000
for _ in $(seq 1 60); do
  code=$(curl --silent --output /dev/null --write-out '%{http_code}' http://127.0.0.1:18081/readyz || true)
  [ "$code" = 200 ] && break
  sleep 2
done
echo "api /readyz: $code"
[ "$code" = 200 ] || { echo "::error::api /readyz is $code on the restored database"; exit 1; }

echo "postgres: roles, RLS and restore at T: OK"
