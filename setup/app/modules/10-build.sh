#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

if [[ -z "${SUDO_USER:-}" ]]; then
    echo "[ERROR] SUDO_USER is not set. Run setup via sudo." >&2
    exit 1
fi

BUILD_USER="${SUDO_USER}"

echo "[INFO] Building wifi-manager as ${BUILD_USER}"
sudo -u "${BUILD_USER}" -- bash -lc "cd '${REPO_ROOT}' && cargo build --release -p wifi-manager"

echo "[INFO] Build complete"
