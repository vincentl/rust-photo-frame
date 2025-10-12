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
    getty_state="$(systemctl show -p UnitFileState getty@tty1.service 2>/dev/null | cut -d'=' -f2 | tr -d $'\n' || true)"
    if [[ "${getty_state}" == "masked" || "${getty_state}" == "masked-runtime" ]]; then
        log INFO "getty@tty1.service already masked; skipping disable/mask"
    else
        if sudo systemctl disable --now getty@tty1.service >/dev/null 2>&1; then
            log INFO "Disabled getty@tty1.service"
        else
            log WARN "Failed to disable getty@tty1.service"
        fi
        if sudo systemctl mask getty@tty1.service >/dev/null 2>&1; then
            log INFO "Masked getty@tty1.service"
        else
            log WARN "Failed to mask getty@tty1.service"
        fi
    fi

    greetd_state="$(systemctl show -p UnitFileState greetd.service 2>/dev/null | cut -d'=' -f2 | tr -d $'\n' || true)"
    case "${greetd_state}" in
        static)
            log INFO "greetd.service is static; nothing to enable"
            ;;
        masked|masked-runtime)
            log WARN "greetd.service is masked; enable manually once unmasked"
            ;;
        *)
            if sudo systemctl enable greetd.service >/dev/null 2>&1; then
                log INFO "Enabled greetd.service"
            else
                log WARN "Failed to enable greetd.service (state: ${greetd_state:-unknown})"
            fi
            ;;
    esac

    if sudo systemctl set-default graphical.target >/dev/null 2>&1; then
        log INFO "Set graphical.target as default boot target"
    else
        log WARN "Failed to set graphical.target as default"
    fi

    if sudo systemctl reset-failed greetd.service >/dev/null 2>&1; then
        log INFO "Cleared greetd.service failure state"
    else
        log WARN "Failed to reset greetd.service failure state"
    fi

    if sudo systemctl stop greetd.service >/dev/null 2>&1; then
        log INFO "Stopped greetd.service"
    else
        log WARN "Failed to stop greetd.service"
    fi
    sleep 1
    if sudo systemctl start greetd.service >/dev/null 2>&1; then
        log INFO "Started greetd.service"
    else
        log WARN "Failed to start greetd.service"
    fi
else
    log WARN "systemctl not available; skipping greetd ownership enforcement"
fi


log INFO "System provisioning complete."


