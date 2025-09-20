#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODULE_DIR="${SCRIPT_DIR}/setup-modules"

if [[ $EUID -ne 0 ]]; then
    echo "[ERROR] This script must be run with sudo or as root." >&2
    exit 1
fi

if [[ ! -d "${MODULE_DIR}" ]]; then
    echo "[ERROR] Module directory not found: ${MODULE_DIR}" >&2
    exit 1
fi

shopt -s nullglob
MODULES=("${MODULE_DIR}"/[0-9][0-9]-*.sh)
shopt -u nullglob

if [[ ${#MODULES[@]} -eq 0 ]]; then
    echo "[WARN] No setup modules found in ${MODULE_DIR}. Nothing to do."
    exit 0
fi

echo "[INFO] Executing setup modules from ${MODULE_DIR}" 
for module in "${MODULES[@]}"; do
    echo "[INFO] Running $(basename "${module}")"
    if [[ ! -x "${module}" ]]; then
        echo "[INFO] Making module executable"
        chmod +x "${module}"
    fi
    "${module}"
    echo "[INFO] Completed $(basename "${module}")"
    echo

done

echo "[INFO] All setup modules completed successfully."
