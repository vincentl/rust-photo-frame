#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

resolve_build_user() {
    if [[ -n "${FRAME_USER:-}" ]]; then
        echo "${FRAME_USER}"
        return
    fi
    if [[ -n "${SUDO_USER:-}" ]]; then
        echo "${SUDO_USER}"
        return
    fi
    local owner
    owner="$(stat -c '%U' "${REPO_ROOT}" 2>/dev/null || stat -f '%Su' "${REPO_ROOT}" 2>/dev/null || echo '')"
    if [[ -n "${owner}" && "${owner}" != "root" ]]; then
        echo "${owner}"
        return
    fi
    echo ""
}

BUILD_USER="$(resolve_build_user)"
if [[ -z "${BUILD_USER}" ]]; then
    echo "[ERROR] Unable to determine non-root build user. Set FRAME_USER before running." >&2
    exit 1
fi

if ! id -u "${BUILD_USER}" >/dev/null 2>&1; then
    echo "[ERROR] Build user '${BUILD_USER}' does not exist." >&2
    exit 1
fi

echo "[INFO] Building wifi-manager as ${BUILD_USER}"
sudo -u "${BUILD_USER}" -- bash -lc "cd '${REPO_ROOT}' && cargo build --release -p wifi-manager"

echo "[INFO] Build complete"
