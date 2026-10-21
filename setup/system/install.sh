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
    # PHOTOFRAME_DISPLAY (kms|wayland) chooses the display stack; keep it
    # across sudo's environment reset.
    exec sudo --preserve-env=PHOTOFRAME_DISPLAY -- "$0" "$@"
fi

# shellcheck source=../lib/systemd.sh
source "${REPO_ROOT}/setup/lib/systemd.sh"
# shellcheck source=../lib/display-stack.sh
source "${REPO_ROOT}/setup/lib/display-stack.sh"

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

# Ensure polkit rules for kiosk are present (fallback in case a module skipped)
ensure_polkit_rules() {
    local src_dir="${REPO_ROOT}/setup/assets/kiosk/polkit-1/rules.d"
    local dest_dir="/etc/polkit-1/rules.d"

    if [[ ! -d "${src_dir}" ]]; then
        log WARN "Polkit rules source not found: ${src_dir}"
        return 0
    fi

    install -d -m 0755 "${dest_dir}"
    local installed=0
    local rule
    for rule in "${src_dir}"/*.rules; do
        [[ -f "${rule}" ]] || continue
        install -m 0644 "${rule}" "${dest_dir}/$(basename "${rule}")"
        installed=1
    done

    if (( installed )); then
        log INFO "Installed polkit rules to ${dest_dir}"
        if command -v systemctl >/dev/null 2>&1; then
            systemctl restart polkit >/dev/null 2>&1 || true
            log INFO "Restarted polkit to load updated rules"
        fi
    else
        log INFO "No polkit rule files to install from ${src_dir}"
    fi
}

ensure_polkit_rules

# Ensure the display stack owns VT1 and launches on graphical.target
if command -v systemctl >/dev/null 2>&1; then
    log INFO "Enforcing display-stack ownership of tty1"
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

    if sudo systemctl set-default graphical.target >/dev/null 2>&1; then
        log INFO "Set graphical.target as default boot target"
    else
        log WARN "Failed to set graphical.target as default"
    fi

    # photoframe-kms.service (default) or greetd/sway (PHOTOFRAME_DISPLAY=wayland);
    # enables one, disables the other, and restarts the chosen one, except
    # that a running Sway stack is not displaced for a viewer that cannot
    # present on KMS yet: the application deploy switches after installing it.
    display_choice="$(display_stack_resolve)"
    log INFO "Display stack: ${display_choice}"
    if ! display_stack_system_apply "${display_choice}" start; then
        log WARN "Display stack '${display_choice}' did not start cleanly; see the messages above"
    fi

    # Restart buttond so it rebinds to the fresh session after the restart
    if systemctl list-unit-files buttond.service >/dev/null 2>&1; then
        if sudo systemctl restart buttond.service >/dev/null 2>&1; then
            log INFO "Restarted buttond.service to sync with the new session"
        else
            log WARN "Failed to restart buttond.service"
        fi
    else
        log INFO "buttond.service not installed; skipping restart"
    fi
else
    log WARN "systemctl not available; skipping display-stack enforcement"
fi


log INFO "System provisioning complete."
