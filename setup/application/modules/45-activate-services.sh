#!/usr/bin/env bash
set -euo pipefail

MODULE="app:45-activate-services"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/photo-frame}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../../lib/systemd.sh
source "${SCRIPT_DIR}/../../lib/systemd.sh"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${MODULE}" "$level: $*"
}

run_sudo() {
    sudo "$@"
}

if ! systemd_available; then
    log WARN "systemctl not available; skipping service activation"
    exit 0
fi

# Install or refresh app unit files from the installed tree
UNIT_SRC_DIR="${INSTALL_ROOT}/systemd"
if [[ -d "${UNIT_SRC_DIR}" ]]; then
    log INFO "Installing systemd unit files from ${UNIT_SRC_DIR}"
    while IFS= read -r -d '' unit_file; do
        dest="/etc/systemd/system/$(basename "${unit_file}")"
        run_sudo install -D -m 0644 "${unit_file}" "${dest}"
    done < <(find "${UNIT_SRC_DIR}" -type f \( -name '*.service' -o -name '*.timer' \) -print0)
    run_sudo systemctl daemon-reload
else
    log WARN "Unit source directory not found: ${UNIT_SRC_DIR}"
fi

# Ensure seatd is enabled and started for DRM access
for seatd_unit in seatd.socket seatd.service; do
    if run_sudo systemctl list-unit-files "${seatd_unit}" >/dev/null 2>&1; then
        log INFO "Enabling ${seatd_unit}"
        run_sudo systemctl enable "${seatd_unit}" >/dev/null 2>&1 || true
        log INFO "Starting ${seatd_unit}"
        run_sudo systemctl start "${seatd_unit}" >/dev/null 2>&1 || true
    fi
done

# Enable and start greetd now that binaries are present
if run_sudo systemctl list-unit-files greetd.service >/dev/null 2>&1; then
    log INFO "Enabling greetd.service"
    run_sudo systemctl enable greetd.service >/dev/null 2>&1 || true
    log INFO "Starting greetd.service"
    run_sudo systemctl start greetd.service >/dev/null 2>&1 || true
fi

# Enable and start app-specific services if present
for unit in photoframe-wifi-manager.service buttond.service photoframe-sync.timer; do
    if run_sudo systemctl list-unit-files "${unit}" >/dev/null 2>&1; then
        log INFO "Enabling ${unit}"
        run_sudo systemctl enable "${unit}" >/dev/null 2>&1 || true
        log INFO "Starting ${unit}"
        run_sudo systemctl start "${unit}" >/dev/null 2>&1 || true
    fi
done

log INFO "Kiosk services activated"

