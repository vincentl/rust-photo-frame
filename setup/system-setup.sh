#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT_NAME="$(basename "${BASH_SOURCE[0]}")"
MODULE_DIR="${SCRIPT_DIR}/setup-modules"

if [[ $EUID -ne 0 ]]; then
    export FRAME_USER="${FRAME_USER:-${USER}}"
    exec sudo FRAME_USER="${FRAME_USER}" "${SCRIPT_DIR}/${SCRIPT_NAME}" "$@"
fi

prompt_for_frame_user() {
    local input
    while true; do
        read -r -p "Enter the username that should own the rust-photo-frame installation: " input
        input="${input// /}"
        if [[ -z "${input}" ]]; then
            echo "[ERROR] Username cannot be empty." >&2
            continue
        fi
        if ! id -u "${input}" >/dev/null 2>&1; then
            echo "[ERROR] User '${input}' does not exist on this system." >&2
            continue
        fi
        FRAME_USER="${input}"
        export FRAME_USER
        break
    done
}

if [[ -z "${FRAME_USER:-}" ]]; then
    if [[ -n "${SUDO_USER:-}" ]]; then
        FRAME_USER="${SUDO_USER}"
        export FRAME_USER
    else
        prompt_for_frame_user
    fi
fi

if ! id -u "${FRAME_USER}" >/dev/null 2>&1; then
    echo "[ERROR] Target user '${FRAME_USER}' does not exist. Use FRAME_USER to specify a valid account." >&2
    exit 1
fi

echo "[INFO] Configuring rust-photo-frame for user '${FRAME_USER}'."

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
