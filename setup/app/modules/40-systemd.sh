#!/usr/bin/env bash
set -euo pipefail

MODULE="app:40-systemd"
DRY_RUN="${DRY_RUN:-0}"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/photo-frame}"
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

legacy_units=(photo-app.service photo-app.target sync-photos.service sync-photos.timer wifi-hotspot@.service wifi-watcher.service wifi-setter.service)
for legacy in "${legacy_units[@]}"; do
    if [[ ! -f "${SYSTEMD_SOURCE}/${legacy}" && -e "${SYSTEMD_TARGET}/${legacy}" ]]; then
        if [[ "${DRY_RUN}" == "1" ]]; then
            log INFO "DRY_RUN: would remove legacy unit ${legacy}"
        else
            run_sudo systemctl disable "${legacy}" >/dev/null 2>&1 || true
            run_sudo rm -f "${SYSTEMD_TARGET}/${legacy}"
            log INFO "Removed legacy unit ${legacy}"
        fi
    fi
done

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

enable_and_start() {
    local unit_name="$1"
    local action="$2"
    if [[ ! -f "${SYSTEMD_SOURCE}/${unit_name}" ]]; then
        return
    fi
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would systemctl ${action} ${unit_name}"
    else
        run_sudo systemctl ${action} "${unit_name}"
    fi
}

enable_and_start photo-frame.service "enable --now"
if [[ -f "${SYSTEMD_SOURCE}/photo-sync.timer" ]]; then
    enable_and_start photo-sync.timer "enable --now"
    if [[ -f "${SYSTEMD_SOURCE}/photo-sync.service" ]]; then
        enable_and_start photo-sync.service "enable"
    fi
fi

log INFO "Systemd units installed and activated"
