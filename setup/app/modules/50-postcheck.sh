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

run_sudo() {
    sudo "$@"
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

if ! run_sudo -u "${SERVICE_USER}" test -f "${VAR_CONFIG}"; then
    log WARN "Runtime config missing at ${VAR_CONFIG}; copy ${CONFIG_TEMPLATE} or rerun ./setup/app/run.sh"
fi

if command -v systemctl >/dev/null 2>&1; then
    systemd_unit_exists() {
        local unit="$1"
        systemctl cat "${unit}" >/dev/null 2>&1
    }

    check_service() {
        local service="$1"
        local level_on_fail="$2"
        local missing_hint="$3"
        local message="${service} is not active"

        if ! systemd_unit_exists "${service}"; then
            if [[ -n "${missing_hint}" ]]; then
                log WARN "${service} not installed; ${missing_hint}"
            else
                log WARN "${service} not installed"
            fi
            return 0
        fi

        if systemctl is-active --quiet "${service}"; then
            return 0
        fi

        systemctl status "${service}" --no-pager || true

        if [[ "${level_on_fail}" == "ERROR" ]]; then
            log ERROR "${message}"
            exit 1
        fi

        log "${level_on_fail}" "${message}"
    }

    check_enabled() {
        local service="$1"
        local missing_hint="$2"

        if ! systemd_unit_exists "${service}"; then
            if [[ -n "${missing_hint}" ]]; then
                log WARN "${service} not installed; ${missing_hint}"
            else
                log WARN "${service} not installed"
            fi
            return
        fi

        if ! systemctl is-enabled --quiet "${service}"; then
            log WARN "${service} is not enabled"
        fi
    }

    kiosk_hint="run setup/kiosk-bookworm.sh to provision kiosk services"

    check_service "${KIOSK_SERVICE}" "ERROR" "${kiosk_hint}"
    check_enabled "${KIOSK_SERVICE}" "${kiosk_hint}"

    check_service "${WIFI_SERVICE}" "ERROR" "${kiosk_hint}"
    check_enabled "${WIFI_SERVICE}" "${kiosk_hint}"

    check_service "${BUTTON_SERVICE}" "WARN" "${kiosk_hint}"
    check_service "${SYNC_TIMER}" "WARN" "${kiosk_hint}"
else
    log WARN "systemctl not available; skipping service state checks"
fi

rustc_version=$(rustc --version 2>/dev/null || echo "rustc unavailable")
cargo_version=$(cargo --version 2>/dev/null || echo "cargo unavailable")

if command -v systemctl >/dev/null 2>&1; then
    service_status=$(systemctl show -p ActiveState --value "${KIOSK_SERVICE}" 2>/dev/null || echo "not-found")
    wifi_service_status=$(systemctl show -p ActiveState --value "${WIFI_SERVICE}" 2>/dev/null || echo "not-found")
    button_status=$(systemctl show -p ActiveState --value "${BUTTON_SERVICE}" 2>/dev/null || echo "not-found")
    sync_status=$(systemctl show -p ActiveState --value "${SYNC_TIMER}" 2>/dev/null || echo "not-found")
else
    service_status="not checked (systemctl unavailable)"
    wifi_service_status="not checked (systemctl unavailable)"
    button_status="not checked (systemctl unavailable)"
    sync_status="not checked (systemctl unavailable)"
fi

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
