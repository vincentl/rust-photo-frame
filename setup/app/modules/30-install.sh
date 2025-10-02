#!/usr/bin/env bash
set -euo pipefail

MODULE="app:30-install"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/photo-frame}"
SERVICE_USER="${SERVICE_USER:-kiosk}"
SERVICE_GROUP="${SERVICE_GROUP:-}"
if id -u "${SERVICE_USER}" >/dev/null 2>&1; then
    SERVICE_GROUP="${SERVICE_GROUP:-$(id -gn "${SERVICE_USER}")}"
fi
SERVICE_GROUP="${SERVICE_GROUP:-${SERVICE_USER}}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STAGE_ROOT="${STAGE_ROOT:-${SCRIPT_DIR}/../build}"
STAGE_DIR="${STAGE_ROOT}/stage"
VAR_ROOT="/var/lib/photo-frame"
CONFIG_DEST="${VAR_ROOT}/config/config.yaml"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${MODULE}" "$level: $*"
}

run_sudo() {
    sudo "$@"
}

require_stage_dir() {
    if [[ ! -d "${STAGE_DIR}" ]]; then
        log ERROR "Stage directory ${STAGE_DIR} does not exist. Run the stage module first."
        exit 1
    fi
}

validate_service_principal() {
    if ! id -u "${SERVICE_USER}" >/dev/null 2>&1; then
        log ERROR "Service user ${SERVICE_USER} does not exist."
        exit 1
    fi
    if ! getent group "${SERVICE_GROUP}" >/dev/null 2>&1; then
        log ERROR "Service group ${SERVICE_GROUP} does not exist."
        exit 1
    fi
    if ! id -Gn "${SERVICE_USER}" | tr ' ' '\n' | grep -Fxq "${SERVICE_GROUP}"; then
        log ERROR "Service user ${SERVICE_USER} is not a member of ${SERVICE_GROUP}."
        exit 1
    fi
    log INFO "Using service account ${SERVICE_USER}:${SERVICE_GROUP}"
}

install_tree() {
    run_sudo install -d -m 755 "${INSTALL_ROOT}"
    local dir
    for dir in bin etc docs share systemd; do
        if [[ -d "${STAGE_DIR}/${dir}" ]]; then
            run_sudo install -d -m 755 "${INSTALL_ROOT}/${dir}"
            run_sudo rsync -a --delete "${STAGE_DIR}/${dir}/" "${INSTALL_ROOT}/${dir}/"
        else
            run_sudo rm -rf "${INSTALL_ROOT}/${dir}" 2>/dev/null || true
            run_sudo install -d -m 755 "${INSTALL_ROOT}/${dir}"
        fi
    done
}

bootstrap_runtime() {
    if [[ ! -d "${VAR_ROOT}" ]]; then
        run_sudo install -d -m 750 -o "${SERVICE_USER}" -g "${SERVICE_GROUP}" "${VAR_ROOT}"
    fi
    local subdir
    for subdir in photos config; do
        local path="${VAR_ROOT}/${subdir}"
        if [[ ! -d "${path}" ]]; then
            run_sudo install -d -m 770 -o "${SERVICE_USER}" -g "${SERVICE_GROUP}" "${path}"
        fi
    done
    if [[ -f "${STAGE_DIR}/etc/config.yaml" && ! -f "${CONFIG_DEST}" ]]; then
        run_sudo install -m 660 -o "${SERVICE_USER}" -g "${SERVICE_GROUP}" "${STAGE_DIR}/etc/config.yaml" "${CONFIG_DEST}"
        log INFO "Seeded default config at ${CONFIG_DEST}"
    fi
}

set_permissions() {
    run_sudo chmod -R u+rwX,go+rX \
        "${INSTALL_ROOT}/bin" \
        "${INSTALL_ROOT}/docs" \
        "${INSTALL_ROOT}/share"
    if [[ -d "${INSTALL_ROOT}/systemd" ]]; then
        run_sudo chmod -R u+rwX,go+rX "${INSTALL_ROOT}/systemd"
    fi
    if [[ -d "${INSTALL_ROOT}/etc" ]]; then
        run_sudo find "${INSTALL_ROOT}/etc" -type f -exec chmod 644 {} +
    fi
}

require_stage_dir
validate_service_principal
install_tree
bootstrap_runtime
set_permissions

log INFO "Installation tree updated at ${INSTALL_ROOT}"
