# shellcheck shell=bash
# Shared by smoke-setup.sh and stage-to-pi.sh. Source it; do not run it.
# Everything here assumes it runs inside the Lima VM from developer/lima/photoframe.yaml.

LIMA_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOST_REPO_ROOT="$(cd "${LIMA_SCRIPT_DIR}/../.." && pwd)"
BUILD_ROOT="${BUILD_ROOT:-/var/cache/photoframe-build}"
# VM-local copy of the working tree. The macOS mount is read-only, and the
# application modules chmod and write inside the repo, so they run from here.
SRC_COPY="${SRC_COPY:-${BUILD_ROOT}/src}"

lima_log() { printf '[lima] %s\n' "$*"; }

lima_die() {
    printf '[lima] ERROR: %s\n' "$*" >&2
    exit 1
}

lima_require_vm() {
    [[ "$(uname -m)" == "aarch64" ]] || lima_die "expected an aarch64 guest, got $(uname -m); run this inside the Lima VM"
    [[ $(id -u) -ne 0 ]] || lima_die "run as the normal VM user; the scripts call sudo where needed"
    # shellcheck disable=SC1091
    . /etc/os-release
    [[ "${VERSION_CODENAME:-}" == "trixie" ]] || lima_die "Debian 13 (trixie) expected, got ${VERSION_CODENAME:-unknown}"
    [[ -d "${BUILD_ROOT}" && -w "${BUILD_ROOT}" ]] || lima_die "${BUILD_ROOT} missing or not writable; was the VM provisioned from developer/lima/photoframe.yaml?"
}

# Copy the host working tree (committed, modified and untracked files; anything
# .gitignored is skipped) to ${SRC_COPY}. rsync keeps mtimes, so cargo's
# incremental state survives repeated syncs.
lima_sync_src() {
    lima_log "Syncing ${HOST_REPO_ROOT} -> ${SRC_COPY}"
    mkdir -p "${SRC_COPY}"
    rsync -a --delete \
        --exclude=/.git --exclude=/.claude --exclude=/target \
        --filter=':- .gitignore' \
        "${HOST_REPO_ROOT}/" "${SRC_COPY}/"
}
