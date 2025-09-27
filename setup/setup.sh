#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODULE_DIR="${SCRIPT_DIR}/modules"

if [[ $EUID -ne 0 ]]; then
    echo "[ERROR] This script must be run as root." >&2
    exit 1
fi

if [[ ! -d "${MODULE_DIR}" ]]; then
    echo "[WARN] No module directory at ${MODULE_DIR}" >&2
    exit 0
fi

shopt -s nullglob
modules=("${MODULE_DIR}"/[0-9][0-9]-*.sh)
shopt -u nullglob

if [[ ${#modules[@]} -eq 0 ]]; then
    echo "[INFO] No setup modules to execute in ${MODULE_DIR}."
    exit 0
fi

echo "[INFO] Executing rust-photo-frame services modules"
for module in "${modules[@]}"; do
    echo "[INFO] Running $(basename "${module}")"
    if [[ ! -x "${module}" ]]; then
        chmod +x "${module}"
    fi
    "${module}"
    echo "[INFO] Completed $(basename "${module}")"
    echo
done

echo "[INFO] rust-photo-frame services modules finished"
