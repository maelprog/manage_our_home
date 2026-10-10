#!/usr/bin/env bash
# Checks that the data of Postgres and MinIO sits on a dm-crypt (LUKS)
# device (#380, docs/v2-deployment.md item #20). Read-only: it opens,
# formats and mounts nothing.
#
# Usage, on the production host, as root (Docker's data root is not
# readable by anyone else):
#
#   sudo ./check-volume-encryption.sh                 # volumes of the `infra` project
#   sudo ./check-volume-encryption.sh myproj_postgres_data /srv/minio
#
# Each argument is a Docker volume name, or an absolute path taken as is.
# Without arguments, the two named volumes of infra/docker-compose.yml under
# Compose's default project name (the directory name, `infra`); pass the
# names yourself if the stack runs under another project name
# (`docker volume ls`).
#
# A path passes when the block device of the file system holding it has a
# `crypt` layer somewhere beneath it: `lsblk --inverse` walks every layer
# down to the disk, so the crypt layer need not be the top one. Encryption
# done by the hosting provider below the virtual disk cannot be seen from
# inside the guest and fails here: check it in the provider's console.
#
# Exit status: 0 if every path passes, 1 if one does not, 2 on usage or
# lookup error.

set -euo pipefail

if [[ $# -eq 0 ]]; then
    set -- infra_postgres_data infra_minio_data
fi

for tool in findmnt lsblk; do
    if ! command -v "$tool" >/dev/null; then
        echo "error: $tool not found (util-linux)" >&2
        exit 2
    fi
done

status=0
for target in "$@"; do
    if [[ "$target" == /* ]]; then
        path="$target"
    elif ! path="$(docker volume inspect --format '{{.Mountpoint}}' "$target" 2>/dev/null)"; then
        echo "error: no Docker volume named $target (docker volume ls)" >&2
        exit 2
    fi
    if [[ ! -e "$path" ]]; then
        echo "error: $path does not exist or is not readable (run as root)" >&2
        exit 2
    fi
    # SOURCE may carry a bracketed subvolume (btrfs: /dev/sda2[/@docker]).
    source="$(findmnt --noheadings --output SOURCE --target "$path")"
    source="${source%%\[*}"
    if [[ "$source" == /dev/* ]] &&
        lsblk --inverse --list --noheadings --output TYPE "$source" 2>/dev/null | grep -qx 'crypt'; then
        echo "ok: $target ($path) is on $source, encrypted (dm-crypt)"
    else
        echo "NOT ENCRYPTED: $target ($path) is on ${source:-an unknown device}, with no dm-crypt layer"
        status=1
    fi
done
exit "$status"
