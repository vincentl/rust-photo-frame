#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "${SCRIPT_DIR}/../.." && pwd)}"
MODULE="app:40-systemd"
DRY_RUN="${DRY_RUN:-0}"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/photo-frame}"
SERVICE_USER="${SERVICE_USER:-$(id -un)}"
SYSTEMD_SOURCE="${INSTALL_ROOT}/systemd"
SYSTEMD_TARGET="/etc/systemd/system"

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

ensure_nm_permissions() {
    local nm_module="${REPO_ROOT}/setup/system/modules/50-networkmanager.sh"

    if [[ ! -f "${nm_module}" ]]; then
        log WARN "NetworkManager permission helper missing at ${nm_module}; skipping"
        return 0
    fi

    if [[ ! -x "${nm_module}" ]]; then
        chmod +x "${nm_module}"
    fi

    log INFO "Ensuring NetworkManager permissions for ${SERVICE_USER}"

    if ! SERVICE_USER="${SERVICE_USER}" DRY_RUN="${DRY_RUN}" "${nm_module}"; then
        log ERROR "Failed to configure NetworkManager permissions for ${SERVICE_USER}"
        exit 1
    fi

    return 0
}

if [[ ! -d "${SYSTEMD_SOURCE}" ]]; then
    log WARN "No systemd units staged at ${SYSTEMD_SOURCE}"
    exit 0
fi

shopt -s nullglob
units=("${SYSTEMD_SOURCE}"/*.service "${SYSTEMD_SOURCE}"/*.timer)
shopt -u nullglob

if [[ ${#units[@]} -eq 0 ]]; then
    log WARN "No systemd unit files to install"
    exit 0
fi

for unit in "${units[@]}"; do
    unit_name="$(basename "${unit}")"
    target_path="${SYSTEMD_TARGET}/${unit_name}"
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would link ${unit} -> ${target_path}"
        continue
    fi
    run_sudo install -D -m 644 "${unit}" "${target_path}"
    log INFO "Installed ${unit_name}"
done

if [[ "${DRY_RUN}" == "1" ]]; then
    log INFO "DRY_RUN: would run systemctl daemon-reload"
else
    run_sudo systemctl daemon-reload
fi

ensure_nm_permissions

enable_unit() {
    local unit_name="$1"
    if [[ ! -f "${SYSTEMD_SOURCE}/${unit_name}" ]]; then
        return
    fi
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would systemctl enable ${unit_name}"
    else
        run_sudo systemctl enable "${unit_name}"
    fi
}

activate_unit() {
    local unit_name="$1"
    if [[ ! -f "${SYSTEMD_SOURCE}/${unit_name}" ]]; then
        return
    fi
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would ensure ${unit_name} is running (reload-or-restart if already active)"
        return
    fi
    if run_sudo systemctl is-active --quiet "${unit_name}"; then
        run_sudo systemctl reload-or-restart "${unit_name}"
    else
        run_sudo systemctl start "${unit_name}"
    fi
}

enable_unit photo-frame.service
activate_unit photo-frame.service

enable_unit wifi-manager.service
activate_unit wifi-manager.service

enable_unit photo-sync.timer
activate_unit photo-sync.timer
enable_unit photo-sync.service

log INFO "Systemd units installed and activated"
