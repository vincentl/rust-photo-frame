#!/usr/bin/env bash
set -euo pipefail

MODULE="app:50-postcheck"
DRY_RUN="${DRY_RUN:-0}"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/photo-frame}"
SERVICE_USER="${SERVICE_USER:-photo-frame}"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${MODULE}" "$level: $*"
}

if [[ "${DRY_RUN}" == "1" ]]; then
    log INFO "DRY_RUN: would perform post-install verification"
    exit 0
fi

BIN_PATH="${INSTALL_ROOT}/bin/rust-photo-frame"
CONFIG_TEMPLATE="${INSTALL_ROOT}/etc/config.yaml"
VAR_DIR="${INSTALL_ROOT}/var"

if [[ ! -x "${BIN_PATH}" ]]; then
    log ERROR "Binary ${BIN_PATH} missing or not executable"
    exit 1
fi

if [[ ! -f "${CONFIG_TEMPLATE}" ]]; then
    log ERROR "Default config template missing at ${CONFIG_TEMPLATE}"
    exit 1
fi

if [[ ! -d "${VAR_DIR}" ]]; then
    log ERROR "Var directory missing at ${VAR_DIR}"
    exit 1
fi

var_owner="$(stat -c %U "${VAR_DIR}")"
if [[ "${var_owner}" != "${SERVICE_USER}" ]]; then
    log ERROR "${VAR_DIR} is owned by ${var_owner}, expected ${SERVICE_USER}"
    exit 1
fi

if ! systemctl is-active --quiet photo-frame.service; then
    log ERROR "photo-frame.service is not active"
    systemctl status photo-frame.service --no-pager || true
    exit 1
fi

if ! systemctl is-enabled --quiet photo-frame.service; then
    log WARN "photo-frame.service is not enabled"
fi

rustc_version=$(rustc --version 2>/dev/null || echo "rustc unavailable")
cargo_version=$(cargo --version 2>/dev/null || echo "cargo unavailable")
service_status=$(systemctl is-active photo-frame.service)

log INFO "Deployment summary:"
cat <<SUMMARY
----------------------------------------
Install root : ${INSTALL_ROOT}
Service user : ${SERVICE_USER}
Binary       : ${BIN_PATH}
Config (RO)  : ${CONFIG_TEMPLATE}
Config (RW)  : ${INSTALL_ROOT}/var/config.yaml
rustc        : ${rustc_version}
cargo        : ${cargo_version}
photo-frame.service : ${service_status}
Next steps:
  - Customize ${INSTALL_ROOT}/var/config.yaml for your site.
  - Review journal logs with 'journalctl -u photo-frame.service -f'.
----------------------------------------
SUMMARY

log INFO "Post-install checks passed"
