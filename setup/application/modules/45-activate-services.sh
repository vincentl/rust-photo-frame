#!/usr/bin/env bash
set -euo pipefail

MODULE="app:45-activate-services"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/photoframe}"
SYNC_ENV_PATH="${SYNC_ENV_PATH:-/etc/photoframe/sync.env}"
SYNC_TIMER="${SYNC_TIMER:-photoframe-sync.timer}"
SYNC_SERVICE="${SYNC_SERVICE:-photoframe-sync.service}"
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

read_env_value() {
    local key="$1"
    local file="$2"
    awk -v key="${key}" '
        /^[[:space:]]*#/ { next }
        $0 ~ "^[[:space:]]*" key "[[:space:]]*=" {
            value = $0
            sub(/^[[:space:]]*[^=]+=[[:space:]]*/, "", value)
            sub(/[[:space:]]+#.*/, "", value)
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
            sub(/^"/, "", value)
            sub(/"$/, "", value)
            print value
            exit
        }
    ' "${file}"
}

sync_is_configured() {
    local sync_env="$1"
    local rclone_remote=""
    local rsync_source=""
    if [[ ! -f "${sync_env}" ]]; then
        return 1
    fi
    rclone_remote="$(read_env_value "RCLONE_REMOTE" "${sync_env}")"
    rsync_source="$(read_env_value "RSYNC_SOURCE" "${sync_env}")"
    [[ -n "${rclone_remote}" || -n "${rsync_source}" ]]
}

configure_sync_timer() {
    if ! run_sudo systemctl list-unit-files "${SYNC_TIMER}" >/dev/null 2>&1; then
        log INFO "${SYNC_TIMER} not installed; skipping sync timer activation"
        return
    fi

    if sync_is_configured "${SYNC_ENV_PATH}"; then
        log INFO "Sync source configured; enabling ${SYNC_TIMER}"
        run_sudo systemctl enable "${SYNC_TIMER}" >/dev/null 2>&1 || true
        run_sudo systemctl start "${SYNC_TIMER}" >/dev/null 2>&1 || true
        return
    fi

    log INFO "Sync source not configured in ${SYNC_ENV_PATH}; keeping ${SYNC_TIMER} disabled"
    run_sudo systemctl disable "${SYNC_TIMER}" >/dev/null 2>&1 || true
    run_sudo systemctl stop "${SYNC_TIMER}" >/dev/null 2>&1 || true
    run_sudo systemctl stop "${SYNC_SERVICE}" >/dev/null 2>&1 || true
    run_sudo systemctl reset-failed "${SYNC_SERVICE}" "${SYNC_TIMER}" >/dev/null 2>&1 || true
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

# Enable and restart greetd now that binaries are present.
# A clean restart ensures the kiosk session picks up freshly installed binaries
# and recreates the control socket without requiring a reboot.
if run_sudo systemctl list-unit-files greetd.service >/dev/null 2>&1; then
    log INFO "Enabling greetd.service"
    run_sudo systemctl enable greetd.service >/dev/null 2>&1 || true
    log INFO "Restarting greetd.service with stop/sleep/start sequence"
    run_sudo systemctl stop greetd.service >/dev/null 2>&1 || true
    sleep 1
    run_sudo systemctl start greetd.service >/dev/null 2>&1 || true
fi

# Enable and start app-specific services if present
for unit in photoframe-wifi-manager.service buttond.service; do
    if run_sudo systemctl list-unit-files "${unit}" >/dev/null 2>&1; then
        log INFO "Enabling ${unit}"
        run_sudo systemctl enable "${unit}" >/dev/null 2>&1 || true
        log INFO "Starting ${unit}"
        run_sudo systemctl start "${unit}" >/dev/null 2>&1 || true
    fi
done

configure_sync_timer

log INFO "Kiosk services activated"
