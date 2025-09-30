#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STAGE_DIR="${SCRIPT_DIR}/../stage"
INSTALL_ROOT="/opt/photo-frame"
UNIT_NAME="wifi-manager.service"

if [[ ! -f "${INSTALL_ROOT}/systemd/${UNIT_NAME}" ]]; then
    echo "[ERROR] Missing staged unit at ${INSTALL_ROOT}/systemd/${UNIT_NAME}" >&2
    exit 1
fi

install -m 0644 "${INSTALL_ROOT}/systemd/${UNIT_NAME}" "/etc/systemd/system/${UNIT_NAME}"
systemctl daemon-reload
systemctl enable --now "${UNIT_NAME}"

echo "[INFO] ${UNIT_NAME} enabled and started"

BINARY="${INSTALL_ROOT}/bin/wifi-manager"
VERSION="$(sudo -u photo-frame "${BINARY}" --version 2>/dev/null || echo "unknown")"
CONFIG_PATH="${INSTALL_ROOT}/etc/wifi-manager.yaml"
HOTSPOT_SSID="$(awk -F': ' '/^[[:space:]]*ssid:/ {print $2; exit}' "${CONFIG_PATH}" | tr -d '"')"
HOTSPOT_IP="$(awk -F': ' '/ipv4-addr:/ {print $2; exit}' "${CONFIG_PATH}" | tr -d '"')"
UI_PORT="$(awk -F': ' '/^[[:space:]]*port:/ {print $2; exit}' "${CONFIG_PATH}" | tr -d '"')"
ENABLED_STATE="$(systemctl is-enabled "${UNIT_NAME}" 2>/dev/null || echo disabled)"
ACTIVE_STATE="$(systemctl is-active "${UNIT_NAME}" 2>/dev/null || echo inactive)"

cat <<SUMMARY
================ Wi-Fi Install Summary ================
Binary version : ${VERSION}
Binary path    : ${BINARY}
Config path    : ${CONFIG_PATH}
Systemd unit   : /etc/systemd/system/${UNIT_NAME}
Unit enabled   : ${ENABLED_STATE}
Unit active    : ${ACTIVE_STATE}
Hotspot SSID   : ${HOTSPOT_SSID:-PhotoFrame-Setup}
UI URL         : http://${HOTSPOT_IP:-192.168.4.1}:${UI_PORT:-8080}/
State folder   : ${INSTALL_ROOT}/var
Password file  : ${INSTALL_ROOT}/var/hotspot-password.txt
QR asset       : ${INSTALL_ROOT}/var/wifi-qr.png
=======================================================
SUMMARY
