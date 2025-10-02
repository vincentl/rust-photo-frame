#!/usr/bin/env bash
set -euo pipefail

INSTALL_ROOT="${INSTALL_ROOT:-/opt/photo-frame}"
VAR_DIR="${VAR_DIR:-/var/lib/photo-frame}"
CONFIG_PATH="${CONFIG_PATH:-${INSTALL_ROOT}/etc/wifi-manager.yaml}"
SERVICE_NAME="${SERVICE_NAME:-photoframe-wifi-manager.service}"
SERVICE_USER="${SERVICE_USER:-$(id -un)}"
MANAGER_BIN="${MANAGER_BIN:-${INSTALL_ROOT}/bin/wifi-manager}"
HOTSPOT_ID="${HOTSPOT_ID:-pf-hotspot}"
TMP_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t wifi-status)"
trap 'rm -rf "${TMP_DIR}"' EXIT

print_header() {
    printf '\n=== %s ===\n' "$1"
}

SYSTEMCTL_AVAILABLE=0
if command -v systemctl >/dev/null 2>&1; then
    SYSTEMCTL_AVAILABLE=1
fi

unit_exists() {
    local unit="$1"
    if [[ ${SYSTEMCTL_AVAILABLE} -ne 1 ]]; then
        return 1
    fi
    systemctl cat "${unit}" >/dev/null 2>&1
}

unit_status() {
    local unit="$1"
    if [[ ${SYSTEMCTL_AVAILABLE} -ne 1 ]]; then
        echo "systemctl-unavailable"
        return
    fi
    if unit_exists "${unit}"; then
        systemctl is-active "${unit}" 2>/dev/null || echo "inactive"
    else
        echo "not-found"
    fi
}

unit_enabled() {
    local unit="$1"
    if [[ ${SYSTEMCTL_AVAILABLE} -ne 1 ]]; then
        echo "systemctl-unavailable"
        return
    fi
    if unit_exists "${unit}"; then
        systemctl is-enabled "${unit}" 2>/dev/null || echo "disabled"
    else
        echo "not-found"
    fi
}

print_header "Wi-Fi Connectivity"
if command -v nmcli >/dev/null 2>&1; then
    CONNECTIVITY_FILE="${TMP_DIR}/connectivity"
    if nmcli -t -f CONNECTIVITY general status >"${CONNECTIVITY_FILE}" 2>/dev/null; then
        CONNECTIVITY="$(tr -d '\n' < "${CONNECTIVITY_FILE}")"
        printf 'Connectivity: %s\n' "${CONNECTIVITY:-unknown}"
    else
        printf 'Connectivity: error (%s)\n' "$(tr -d '\n' < "${CONNECTIVITY_FILE}" 2>/dev/null || echo 'unknown')"
    fi
    ACTIVE_FILE="${TMP_DIR}/active"
    if nmcli -t -f NAME,DEVICE,TYPE connection show --active >"${ACTIVE_FILE}" 2>/dev/null; then
        ACTIVE_WIFI="$(awk -F: '$3=="wifi" {print $1 " on " $2}' "${ACTIVE_FILE}" | head -n1)"
        printf 'Active connection: %s\n' "${ACTIVE_WIFI:-none}"
    else
        printf 'Active connection: error querying nmcli\n'
    fi

    HOTSPOT_STATE="unknown"
    HOTSPOT_FILE="${TMP_DIR}/hotspot"
    if nmcli -t -f NAME connection show --active >"${HOTSPOT_FILE}" 2>/dev/null; then
        if grep -Fxq "${HOTSPOT_ID}" "${HOTSPOT_FILE}"; then
            HOTSPOT_STATE="active"
        else
            HOTSPOT_STATE="inactive"
        fi
    fi
else
    printf 'nmcli not available on PATH.\n'
fi

if [[ "${HOTSPOT_STATE:-}" != "" ]]; then
    printf 'Hotspot profile (%s): %s\n' "${HOTSPOT_ID}" "${HOTSPOT_STATE}"
fi

print_header "Wi-Fi Manager"
MANAGER_STATUS="$(unit_status "${SERVICE_NAME}")"
MANAGER_ENABLED="$(unit_enabled "${SERVICE_NAME}")"
printf 'Service (%s): %s (enabled: %s)\n' "${SERVICE_NAME}" "${MANAGER_STATUS}" "${MANAGER_ENABLED}"

if [[ -x "${MANAGER_BIN}" ]]; then
    if [[ $EUID -eq 0 && "${SERVICE_USER}" != "root" && $(id -u "${SERVICE_USER}" 2>/dev/null || echo) != "" ]]; then
        VERSION="$(sudo -u "${SERVICE_USER}" "${MANAGER_BIN}" --version 2>/dev/null || echo 'unknown')"
    else
        VERSION="$("${MANAGER_BIN}" --version 2>/dev/null || echo 'unknown')"
    fi
    printf 'Binary: %s\n' "${MANAGER_BIN}"
    printf 'Version: %s\n' "${VERSION:-unknown}"
else
    printf 'Binary: %s (missing)\n' "${MANAGER_BIN}"
fi

if [[ -f "${CONFIG_PATH}" ]]; then
    printf 'Config: %s\n' "${CONFIG_PATH}"
else
    printf 'Config: %s (missing)\n' "${CONFIG_PATH}"
fi

PASSWORD_FILE="${VAR_DIR}/hotspot-password.txt"
if [[ -f "${PASSWORD_FILE}" ]]; then
    printf 'Hotspot password file: %s\n' "${PASSWORD_FILE}"
else
    printf 'Hotspot password file: %s (not created yet)\n' "${PASSWORD_FILE}"
fi

QR_FILE="${VAR_DIR}/wifi-qr.png"
if [[ -f "${QR_FILE}" ]]; then
    printf 'QR code asset: %s\n' "${QR_FILE}"
else
    printf 'QR code asset: %s (not created yet)\n' "${QR_FILE}"
fi

LAST_JSON="${VAR_DIR}/wifi-last.json"
if [[ -f "${LAST_JSON}" ]]; then
    MOD_TIME="$(stat -c '%y' "${LAST_JSON}" 2>/dev/null || echo 'unknown')"
    printf 'Last provisioning record: %s (updated %s)\n' "${LAST_JSON}" "${MOD_TIME}"
else
    printf 'Last provisioning record: %s (not recorded yet)\n' "${LAST_JSON}"
fi

print_header "Photo Frame"
if [[ -z "${PHOTO_SERVICE:-}" ]]; then
    PHOTO_SERVICE="cage@tty1.service"
fi
PHOTO_STATUS="$(unit_status "${PHOTO_SERVICE}")"
PHOTO_ENABLED="$(unit_enabled "${PHOTO_SERVICE}")"
printf '%s: %s (enabled: %s)\n' "${PHOTO_SERVICE}" "${PHOTO_STATUS}" "${PHOTO_ENABLED}"

print_header "Sync"
if [[ -z "${SYNC_SERVICE:-}" ]]; then
    if unit_exists "photoframe-sync.service"; then
        SYNC_SERVICE="photoframe-sync.service"
    else
        SYNC_SERVICE=""
    fi
fi
if [[ -z "${SYNC_TIMER:-}" ]]; then
    if unit_exists "photoframe-sync.timer"; then
        SYNC_TIMER="photoframe-sync.timer"
    else
        SYNC_TIMER=""
    fi
fi

if [[ -n "${SYNC_SERVICE}" ]]; then
    SYNC_STATUS="$(unit_status "${SYNC_SERVICE}")"
    SYNC_ENABLED="$(unit_enabled "${SYNC_SERVICE}")"
    printf '%s: %s (enabled: %s)\n' "${SYNC_SERVICE}" "${SYNC_STATUS}" "${SYNC_ENABLED}"
else
    printf 'Sync service: not installed\n'
fi

if [[ -n "${SYNC_TIMER}" ]]; then
    SYNC_TIMER_STATUS="$(unit_status "${SYNC_TIMER}")"
    SYNC_TIMER_ENABLED="$(unit_enabled "${SYNC_TIMER}")"
    printf '%s: %s (enabled: %s)\n' "${SYNC_TIMER}" "${SYNC_TIMER_STATUS}" "${SYNC_TIMER_ENABLED}"
    if [[ ${SYSTEMCTL_AVAILABLE} -eq 1 && "${SYNC_TIMER_STATUS}" != "not-found" && "${SYNC_TIMER_STATUS}" != "systemctl-unavailable" ]]; then
        NEXT_TIMER="$(systemctl list-timers --all --no-legend 2>/dev/null | awk -v unit="${SYNC_TIMER}" '$5==unit {print $1 " (left: "$2")"; exit}')"
        if [[ -n "${NEXT_TIMER}" ]]; then
            printf 'Next sync: %s\n' "${NEXT_TIMER}"
        else
            printf 'Next sync: timer inactive\n'
        fi
        LAST_TRIGGER_RAW="$(systemctl show -p LastTriggerUSec "${SYNC_TIMER}" 2>/dev/null | cut -d= -f2)"
        if [[ -n "${LAST_TRIGGER_RAW}" && "${LAST_TRIGGER_RAW}" != "n/a" ]]; then
            printf 'Last sync trigger: %s\n' "${LAST_TRIGGER_RAW}"
        else
            printf 'Last sync trigger: never\n'
        fi
    else
        printf 'Next sync: unavailable\n'
        printf 'Last sync trigger: unavailable\n'
    fi
else
    printf 'Sync timer: not installed\n'
    printf 'Next sync: timer not configured\n'
    printf 'Last sync trigger: unavailable\n'
fi

printf '\nStatus summary complete.\n'
