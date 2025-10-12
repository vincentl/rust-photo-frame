#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
MODULE_DIR="${SCRIPT_DIR}/modules"
TOOLS_DIR="${SCRIPT_DIR}/tools"
SCRIPT_NAME="$(basename "$0")"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${SCRIPT_NAME}" "$level: $*"
}

trap 'log ERROR "${SCRIPT_NAME} failed on line ${LINENO}"' ERR

if [[ $(id -u) -ne 0 ]]; then
    exec sudo -- "$0" "$@"
fi

if [[ ! -d "${MODULE_DIR}" ]]; then
    log ERROR "Module directory not found: ${MODULE_DIR}"
    exit 1
fi

export REPO_ROOT SCRIPT_DIR MODULE_DIR TOOLS_DIR
export SYSTEM_STAGE="system"

shopt -s nullglob
modules=("${MODULE_DIR}"/[0-9][0-9]-*.sh)
shopt -u nullglob

if [[ ${#modules[@]} -eq 0 ]]; then
    log INFO "No system modules found in ${MODULE_DIR}."
    exit 0
fi

log INFO "Executing system provisioning modules as user $(id -un)"
for module in "${modules[@]}"; do
    module_name="$(basename "${module}")"
    log INFO "Starting ${module_name}"
    if [[ ! -x "${module}" ]]; then
        chmod +x "${module}"
    fi
    "${module}"
    log INFO "Completed ${module_name}"
    echo
done

# Ensure greetd owns VT1 and launches on graphical.target
if command -v systemctl >/dev/null 2>&1; then
    log INFO "Enforcing greetd ownership of tty1"
    # Ensure greetd owns VT1 and no getty competes for tty1
    sudo systemctl disable --now getty@tty1.service || true
    sudo systemctl mask getty@tty1.service || true

    # Make greetd the display manager
    sudo systemctl enable greetd.service

    # Default to graphical target so greetd starts at boot
    sudo systemctl set-default graphical.target

    # Clear rate-limit and (re)launch greetd cleanly
    sudo systemctl reset-failed greetd.service || true
    sudo systemctl stop greetd.service || true
    sleep 1
    sudo systemctl start greetd.service
else
    log WARN "systemctl not available; skipping greetd ownership enforcement"
fi


log INFO "System provisioning complete."


