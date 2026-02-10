#!/usr/bin/env bash
set -euo pipefail

MODULE="app:50-postcheck"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/photoframe}"
SERVICE_USER="${SERVICE_USER:-kiosk}"
if id -u "${SERVICE_USER}" >/dev/null 2>&1; then
    SERVICE_GROUP="${SERVICE_GROUP:-$(id -gn "${SERVICE_USER}")}"
else
    SERVICE_GROUP="${SERVICE_GROUP:-${SERVICE_USER}}"
fi
KIOSK_SERVICE="${KIOSK_SERVICE:-greetd.service}"
WIFI_SERVICE="${WIFI_SERVICE:-photoframe-wifi-manager.service}"
SYNC_TIMER="${SYNC_TIMER:-photoframe-sync.timer}"
BUTTON_SERVICE="${BUTTON_SERVICE:-buttond.service}"
SEATD_SERVICE="${SEATD_SERVICE:-seatd.service}"
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

BIN_PATH="${INSTALL_ROOT}/bin/photoframe"
WIFI_BIN_PATH="${INSTALL_ROOT}/bin/wifi-manager"
CONFIG_TEMPLATE="${INSTALL_ROOT}/etc/photoframe/config.yaml"
WIFI_CONFIG_TEMPLATE="${INSTALL_ROOT}/etc/wifi-manager.yaml"
WORDLIST_PATH="${INSTALL_ROOT}/share/wordlist.txt"
VAR_DIR="/var/lib/photoframe"
SYSTEM_CONFIG="/etc/photoframe/config.yaml"
DEFAULT_CONTROL_SOCKET_PATH="/run/photoframe/control.sock"

dump_logs_on_failure() {
    local status=$1
    if (( status == 0 )); then
        return
    fi

    set +e
    log ERROR "Application postcheck failed; dumping photoframe journal tail"
    if command -v journalctl >/dev/null 2>&1; then
        if command -v sudo >/dev/null 2>&1; then
            sudo journalctl -t photoframe -b -n 100 || true
        else
            journalctl -t photoframe -b -n 100 || true
        fi
    else
        log WARN "journalctl unavailable; cannot show photoframe logs"
    fi
    printf '[postcheck] HINT: Temporarily set /etc/greetd/config.toml to command="/usr/bin/kmscube" for smoke tests.\n'
}

trap 'dump_logs_on_failure $?' EXIT

resolve_control_socket_path() {
    local socket_path="${DEFAULT_CONTROL_SOCKET_PATH}"
    if [[ -f "${SYSTEM_CONFIG}" ]]; then
        local configured
        configured="$(run_sudo awk -F ':' '
            /^[[:space:]]*control-socket-path:[[:space:]]*/ {
                value=$2
                sub(/^[[:space:]]+/, "", value)
                sub(/[[:space:]]+#.*/, "", value)
                gsub(/["[:space:]]/, "", value)
                print value
                exit
            }
        ' "${SYSTEM_CONFIG}" 2>/dev/null || true)"
        if [[ -n "${configured}" ]]; then
            socket_path="${configured}"
        fi
    fi
    printf '%s' "${socket_path}"
}

wait_for_control_socket() {
    local socket_path="$1"
    local timeout_sec="${2:-20}"
    local elapsed=0
    while (( elapsed < timeout_sec )); do
        if run_sudo test -S "${socket_path}"; then
            return 0
        fi
        sleep 1
        elapsed=$((elapsed + 1))
    done
    return 1
}

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

if [[ ! -f "${SYSTEM_CONFIG}" ]]; then
    log WARN "System config missing at ${SYSTEM_CONFIG}; copy ${CONFIG_TEMPLATE} or rerun ./setup/app/run.sh"
fi

if systemd_available; then
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

        if systemd_is_active "${service}"; then
            return 0
        fi

        systemd_status "${service}" || true

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

        if ! systemd_is_enabled "${service}"; then
            log WARN "${service} is not enabled"
        fi
    }

    kiosk_hint="run 'sudo ./setup/system/install.sh' to provision kiosk services"
    seatd_hint="install seatd and rerun system provisioning"

    check_service "${KIOSK_SERVICE}" "ERROR" "${kiosk_hint}"
    check_enabled "${KIOSK_SERVICE}" "${kiosk_hint}"

    check_service "${SEATD_SERVICE}" "ERROR" "${seatd_hint}"
    check_enabled "${SEATD_SERVICE}" "${seatd_hint}"

    check_service "${WIFI_SERVICE}" "ERROR" "${kiosk_hint}"
    check_enabled "${WIFI_SERVICE}" "${kiosk_hint}"

    check_service "${BUTTON_SERVICE}" "WARN" "${kiosk_hint}"
    check_service "${SYNC_TIMER}" "WARN" "${kiosk_hint}"
else
    log WARN "systemctl not available; skipping service state checks"
fi

control_socket_path="$(resolve_control_socket_path)"
control_socket_status="not checked"
if systemd_available && systemd_unit_exists "${KIOSK_SERVICE}"; then
    log INFO "Checking control socket readiness at ${control_socket_path}"
    if wait_for_control_socket "${control_socket_path}" 15; then
        control_socket_status="ready"
    else
        log WARN "Control socket not present yet; restarting ${KIOSK_SERVICE} once"
        run_sudo systemctl stop "${KIOSK_SERVICE}" >/dev/null 2>&1 || true
        sleep 1
        run_sudo systemctl start "${KIOSK_SERVICE}" >/dev/null 2>&1 || true
        if wait_for_control_socket "${control_socket_path}" 25; then
            control_socket_status="ready-after-restart"
        else
            log ERROR "Control socket not found at ${control_socket_path}. Check ${KIOSK_SERVICE} logs."
            exit 1
        fi
    fi
fi

rustc_version=$(rustc --version 2>/dev/null || echo "rustc unavailable")
cargo_version=$(cargo --version 2>/dev/null || echo "cargo unavailable")

if systemd_available; then
    service_status=$(systemd_unit_property "${KIOSK_SERVICE}" ActiveState 2>/dev/null || echo "not-found")
    wifi_service_status=$(systemd_unit_property "${WIFI_SERVICE}" ActiveState 2>/dev/null || echo "not-found")
    button_status=$(systemd_unit_property "${BUTTON_SERVICE}" ActiveState 2>/dev/null || echo "not-found")
    sync_status=$(systemd_unit_property "${SYNC_TIMER}" ActiveState 2>/dev/null || echo "not-found")
    seatd_status=$(systemd_unit_property "${SEATD_SERVICE}" ActiveState 2>/dev/null || echo "not-found")
else
    service_status="not checked (systemctl unavailable)"
    wifi_service_status="not checked (systemctl unavailable)"
    button_status="not checked (systemctl unavailable)"
    sync_status="not checked (systemctl unavailable)"
    seatd_status="not checked (systemctl unavailable)"
fi

log INFO "Deployment summary:"
cat <<SUMMARY
----------------------------------------
Install root : ${INSTALL_ROOT}
Service user : ${SERVICE_USER}
Service group: ${SERVICE_GROUP}
Binary       : ${BIN_PATH}
Config (template): ${CONFIG_TEMPLATE}
Config (active)  : ${SYSTEM_CONFIG}
Wi-Fi binary : ${WIFI_BIN_PATH}
Wi-Fi config : ${WIFI_CONFIG_TEMPLATE}
Wi-Fi wordlist: ${WORDLIST_PATH}
Control socket: ${control_socket_path} (${control_socket_status})
rustc        : ${rustc_version}
cargo        : ${cargo_version}
${KIOSK_SERVICE} : ${service_status}
${WIFI_SERVICE}: ${wifi_service_status}
${BUTTON_SERVICE}: ${button_status}
${SYNC_TIMER}: ${sync_status}
${SEATD_SERVICE}: ${seatd_status}
Next steps:
  - Customize ${SYSTEM_CONFIG} for your site (requires sudo).
  - Review journal logs with 'journalctl -u ${KIOSK_SERVICE} -f'.
----------------------------------------
SUMMARY

log INFO "Post-install checks passed"
