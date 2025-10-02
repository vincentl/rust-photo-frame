#!/usr/bin/env bash
set -euo pipefail

MODULE="app:50-postcheck"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/photo-frame}"
SERVICE_USER="${SERVICE_USER:-kiosk}"
if id -u "${SERVICE_USER}" >/dev/null 2>&1; then
    SERVICE_GROUP="${SERVICE_GROUP:-$(id -gn "${SERVICE_USER}")}"
else
    SERVICE_GROUP="${SERVICE_GROUP:-${SERVICE_USER}}"
fi
KIOSK_SERVICE="${KIOSK_SERVICE:-cage@tty1.service}"
WIFI_SERVICE="${WIFI_SERVICE:-photoframe-wifi-manager.service}"
SYNC_TIMER="${SYNC_TIMER:-photoframe-sync.timer}"
BUTTON_SERVICE="${BUTTON_SERVICE:-photoframe-buttond.service}"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${MODULE}" "$level: $*"
}

BIN_PATH="${INSTALL_ROOT}/bin/rust-photo-frame"
WIFI_BIN_PATH="${INSTALL_ROOT}/bin/wifi-manager"
CONFIG_TEMPLATE="${INSTALL_ROOT}/etc/config.yaml"
WIFI_CONFIG_TEMPLATE="${INSTALL_ROOT}/etc/wifi-manager.yaml"
WORDLIST_PATH="${INSTALL_ROOT}/share/wordlist.txt"
VAR_DIR="/var/lib/photo-frame"
VAR_CONFIG="${VAR_DIR}/config/config.yaml"

if [[ ! -x "${BIN_PATH}" ]]; then
    log ERROR "Binary ${BIN_PATH} missing or not executable"
    exit 1
fi

if [[ ! -x "${WIFI_BIN_PATH}" ]]; then
    log ERROR "Binary ${WIFI_BIN_PATH} missing or not executable"
    exit 1
fi

if [[ ! -f "${CONFIG_TEMPLATE}" ]]; then
    log ERROR "Default config template missing at ${CONFIG_TEMPLATE}"
    exit 1
fi

if [[ ! -f "${WIFI_CONFIG_TEMPLATE}" ]]; then
    log ERROR "Wi-Fi manager config template missing at ${WIFI_CONFIG_TEMPLATE}"
    exit 1
fi

if [[ ! -f "${WORDLIST_PATH}" ]]; then
    log ERROR "Wi-Fi hotspot wordlist missing at ${WORDLIST_PATH}"
    exit 1
fi

if [[ ! -d "${VAR_DIR}" ]]; then
    log ERROR "Var directory missing at ${VAR_DIR}"
    exit 1
fi

var_owner="$(stat -c %U "${VAR_DIR}")"
var_group="$(stat -c %G "${VAR_DIR}")"
if [[ "${var_owner}" != "${SERVICE_USER}" || "${var_group}" != "${SERVICE_GROUP}" ]]; then
    log ERROR "${VAR_DIR} is owned by ${var_owner}:${var_group}, expected ${SERVICE_USER}:${SERVICE_GROUP}"
    exit 1
fi

if [[ ! -f "${VAR_CONFIG}" ]]; then
    log WARN "Runtime config missing at ${VAR_CONFIG}; seed it with setup/system/create-users-and-perms.sh and app install"
fi

if ! systemctl is-active --quiet "${KIOSK_SERVICE}"; then
    log ERROR "${KIOSK_SERVICE} is not active"
    systemctl status "${KIOSK_SERVICE}" --no-pager || true
    exit 1
fi

if ! systemctl is-enabled --quiet "${KIOSK_SERVICE}"; then
    log WARN "${KIOSK_SERVICE} is not enabled"
fi

if ! systemctl is-active --quiet "${WIFI_SERVICE}"; then
    log ERROR "${WIFI_SERVICE} is not active"
    systemctl status "${WIFI_SERVICE}" --no-pager || true
    exit 1
fi

if ! systemctl is-enabled --quiet "${WIFI_SERVICE}"; then
    log WARN "${WIFI_SERVICE} is not enabled"
fi

if ! systemctl is-active --quiet "${BUTTON_SERVICE}"; then
    log WARN "${BUTTON_SERVICE} is not active"
fi

if ! systemctl is-active --quiet "${SYNC_TIMER}"; then
    log WARN "${SYNC_TIMER} is not active"
fi

rustc_version=$(rustc --version 2>/dev/null || echo "rustc unavailable")
cargo_version=$(cargo --version 2>/dev/null || echo "cargo unavailable")
service_status=$(systemctl is-active "${KIOSK_SERVICE}")
wifi_service_status=$(systemctl is-active "${WIFI_SERVICE}")
button_status=$(systemctl is-active "${BUTTON_SERVICE}" || true)
sync_status=$(systemctl is-active "${SYNC_TIMER}" || true)

log INFO "Deployment summary:"
cat <<SUMMARY
----------------------------------------
Install root : ${INSTALL_ROOT}
Service user : ${SERVICE_USER}
Service group: ${SERVICE_GROUP}
Binary       : ${BIN_PATH}
Config (RO)  : ${CONFIG_TEMPLATE}
Config (RW)  : ${VAR_CONFIG}
Wi-Fi binary : ${WIFI_BIN_PATH}
Wi-Fi config : ${WIFI_CONFIG_TEMPLATE}
Wi-Fi wordlist: ${WORDLIST_PATH}
rustc        : ${rustc_version}
cargo        : ${cargo_version}
${KIOSK_SERVICE} : ${service_status}
${WIFI_SERVICE}: ${wifi_service_status}
${BUTTON_SERVICE}: ${button_status}
${SYNC_TIMER}: ${sync_status}
Next steps:
  - Customize ${VAR_CONFIG} for your site.
  - Review journal logs with 'journalctl -u ${KIOSK_SERVICE} -f'.
----------------------------------------
SUMMARY

log INFO "Post-install checks passed"
