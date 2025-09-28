#!/usr/bin/env bash
set -euo pipefail

MODULE="system:20-boot-4k"
DRY_RUN="${DRY_RUN:-0}"
ENABLE_4K="${ENABLE_4K_BOOT:-1}"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${MODULE}" "$level: $*"
}

run_sudo() {
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: sudo $*"
    else
        sudo "$@"
    fi
}

if [[ "${ENABLE_4K}" != "1" ]]; then
    log INFO "4K boot configuration disabled via ENABLE_4K_BOOT=${ENABLE_4K}. Skipping."
    exit 0
fi

CONFIG_TXT="/boot/firmware/config.txt"
[[ -f "${CONFIG_TXT}" ]] || CONFIG_TXT="/boot/config.txt"

if [[ ! -f "${CONFIG_TXT}" ]]; then
    log WARN "Unable to locate config.txt (looked in /boot/firmware and /boot)."
    exit 0
fi

log INFO "Using configuration file ${CONFIG_TXT}"

declare -A SETTINGS=(
    ["hdmi_force_hotplug:1"]="1"
    ["hdmi_group:1"]="2"
    ["hdmi_mode:1"]="97"
    ["hdmi_drive:1"]="2"
)
OVERLAYS=("pi5-fan,temp0=55000,level0=50,temp1=65000,level1=150,temp2=75000,level2=255")

backup_taken=0
changes_needed=0

take_backup() {
    if [[ "${DRY_RUN}" == "1" || ${backup_taken} -eq 1 ]]; then
        return
    fi
    local backup_path="${CONFIG_TXT}.bak.$(date +%Y%m%d-%H%M%S)"
    run_sudo cp -a "${CONFIG_TXT}" "${backup_path}"
    log INFO "Backup written to ${backup_path}"
    backup_taken=1
}

ensure_setting() {
    local key="$1" value="$2"
    local current
    current=$(sudo awk -F'=' -v key="${key}" '$1==key {print $2}' "${CONFIG_TXT}" || true)
    if [[ "${current}" == "${value}" ]]; then
        log INFO "${key} already set to ${value}"
        return
    fi
    changes_needed=1
    if [[ "${DRY_RUN}" == "1" ]]; then
        if [[ -n "${current}" ]]; then
            log INFO "DRY_RUN: would update ${key}=${value}"
        else
            log INFO "DRY_RUN: would append ${key}=${value}"
        fi
        return
    fi
    take_backup
    if [[ -n "${current}" ]]; then
        run_sudo sed -i "s|^${key}=.*$|${key}=${value}|" "${CONFIG_TXT}"
    else
        printf '\n%s=%s\n' "${key}" "${value}" | run_sudo tee -a "${CONFIG_TXT}" >/dev/null
    fi
    log INFO "Set ${key}=${value}"
}

ensure_overlay() {
    local overlay_line="dtoverlay=$1"
    if sudo grep -qxF "${overlay_line}" "${CONFIG_TXT}"; then
        log INFO "${overlay_line} already present"
        return
    fi
    changes_needed=1
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would append ${overlay_line}"
        return
    fi
    take_backup
    printf '\n%s\n' "${overlay_line}" | run_sudo tee -a "${CONFIG_TXT}" >/dev/null
    log INFO "Added ${overlay_line}"
}

for key in "${!SETTINGS[@]}"; do
    ensure_setting "${key}" "${SETTINGS[${key}]}"
done

for overlay in "${OVERLAYS[@]}"; do
    ensure_overlay "${overlay}"
done

if [[ "${DRY_RUN}" == "1" ]]; then
    if [[ ${changes_needed} -eq 0 ]]; then
        log INFO "DRY_RUN: configuration already matches requirements"
    else
        log INFO "DRY_RUN: configuration changes summarized above"
    fi
    exit 0
fi

if [[ ${backup_taken} -eq 0 ]]; then
    log INFO "Boot configuration already satisfied"
    exit 0
fi

run_sudo sync
log INFO "4K boot configuration ensured"
