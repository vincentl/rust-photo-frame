#!/usr/bin/env bash
set -euo pipefail

INSTALL_ROOT="${INSTALL_ROOT:-/opt/photo-frame}"
VAR_DIR="${VAR_DIR:-${INSTALL_ROOT}/var}"
CONFIG_PATH="${CONFIG_PATH:-${INSTALL_ROOT}/etc/wifi-manager.yaml}"
SERVICE_NAME="${SERVICE_NAME:-wifi-manager.service}"
SERVICE_USER="${SERVICE_USER:-photo-frame}"
MANAGER_BIN="${MANAGER_BIN:-${INSTALL_ROOT}/bin/wifi-manager}"
HOTSPOT_ID="${HOTSPOT_ID:-pf-hotspot}"
TMP_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t wifi-status)"
trap 'rm -rf "${TMP_DIR}"' EXIT

print_header() {
    printf '\n=== %s ===\n' "$1"
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
SERVICE_ACTIVE="inactive"
SERVICE_ENABLED="disabled"
if command -v systemctl >/dev/null 2>&1; then
    SERVICE_ACTIVE="$(systemctl is-active "${SERVICE_NAME}" 2>/dev/null || true)"
    SERVICE_ENABLED="$(systemctl is-enabled "${SERVICE_NAME}" 2>/dev/null || true)"
fi
printf 'Service: %s (enabled: %s)\n' "${SERVICE_ACTIVE}" "${SERVICE_ENABLED}"

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

print_header "Photo App"
PHOTO_STATE="$(systemctl is-active photo-app.service 2>/dev/null || true)"
printf 'photo-app.service: %s\n' "${PHOTO_STATE:-unknown}"
TARGET_STATE="$(systemctl is-active photo-app.target 2>/dev/null || true)"
printf 'photo-app.target: %s\n' "${TARGET_STATE:-unknown}"

print_header "Sync"
SYNC_STATE="$(systemctl is-active sync-photos.service 2>/dev/null || true)"
printf 'sync-photos.service: %s\n' "${SYNC_STATE:-unknown}"
NEXT_TIMER="$(systemctl list-timers --all 2>/dev/null | awk '/sync-photos.timer/ {print $5, "(left: "$6")"; exit}')"
if [[ -n "${NEXT_TIMER}" ]]; then
    printf 'Next sync: %s\n' "${NEXT_TIMER}"
else
    printf 'Next sync: timer inactive\n'
fi
LAST_TRIGGER="$(systemctl show -p LastTriggerUSec sync-photos.timer 2>/dev/null | cut -d= -f2)"
if [[ -n "${LAST_TRIGGER}" && "${LAST_TRIGGER}" != "n/a" ]]; then
    printf 'Last sync trigger: %s\n' "${LAST_TRIGGER}"
else
    printf 'Last sync trigger: never\n'
fi

printf '\nStatus summary complete.\n'
