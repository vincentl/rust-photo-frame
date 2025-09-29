#!/usr/bin/env bash
set -euo pipefail

MODULE="app:30-install"
DRY_RUN="${DRY_RUN:-0}"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/photo-frame}"
SERVICE_USER="${SERVICE_USER:-$(id -un)}"
SERVICE_GROUP="${SERVICE_GROUP:-$(id -gn)}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STAGE_ROOT="${STAGE_ROOT:-${SCRIPT_DIR}/../build}"
STAGE_DIR="${STAGE_ROOT}/stage"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${MODULE}" "$level: $*"
}

run_sudo() {
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: sudo $*"
    else
        sudo "$@"
    fi
}

require_stage_dir() {
    if [[ ! -d "${STAGE_DIR}" ]]; then
        log ERROR "Stage directory ${STAGE_DIR} does not exist. Run stage module first."
        exit 1
    fi
}

validate_service_principal() {
    if ! id -u "${SERVICE_USER}" >/dev/null 2>&1; then
        log ERROR "Service user ${SERVICE_USER} does not exist. Set SERVICE_USER to an existing account."
        exit 1
    fi
    if ! getent group "${SERVICE_GROUP}" >/dev/null 2>&1; then
        log ERROR "Service group ${SERVICE_GROUP} does not exist. Set SERVICE_GROUP to a group associated with ${SERVICE_USER}."
        exit 1
    fi
    if ! id -Gn "${SERVICE_USER}" | tr ' ' '\n' | grep -Fxq "${SERVICE_GROUP}"; then
        log ERROR "Service user ${SERVICE_USER} is not a member of ${SERVICE_GROUP}. Adjust SERVICE_GROUP or update the account."
        exit 1
    fi
    log INFO "Using service account ${SERVICE_USER}:${SERVICE_GROUP}"
}

install_tree() {
    run_sudo install -d -m 755 "${INSTALL_ROOT}"
    for dir in bin lib etc docs systemd share var; do
        run_sudo install -d -m 755 "${INSTALL_ROOT}/${dir}"
    done

    run_sudo install -d -m 755 "${INSTALL_ROOT}/share/backgrounds"

    for dir in bin lib etc docs systemd share; do
        if [[ -d "${STAGE_DIR}/${dir}" ]]; then
            run_sudo rsync -a --delete "${STAGE_DIR}/${dir}/" "${INSTALL_ROOT}/${dir}/"
        fi
    done
}

bootstrap_var() {
    run_sudo install -d -m 755 "${INSTALL_ROOT}/var/log"
    run_sudo install -d -m 755 "${INSTALL_ROOT}/var/cache"
    run_sudo install -d -m 775 "${INSTALL_ROOT}/var/photos"
    if [[ -f "${INSTALL_ROOT}/etc/config.yaml" && ! -f "${INSTALL_ROOT}/var/config.yaml" ]]; then
        if [[ "${DRY_RUN}" == "1" ]]; then
            log INFO "DRY_RUN: would initialize writable config at ${INSTALL_ROOT}/var/config.yaml"
        else
            run_sudo install -m 664 "${INSTALL_ROOT}/etc/config.yaml" "${INSTALL_ROOT}/var/config.yaml"
        fi
    fi
    run_sudo chown -R "${SERVICE_USER}:${SERVICE_GROUP}" "${INSTALL_ROOT}/var"
}

require_stage_dir
validate_service_principal
if [[ "${DRY_RUN}" == "1" ]]; then
    log INFO "DRY_RUN: would install staged artifacts into ${INSTALL_ROOT}"
else
    install_tree
fi

if [[ "${DRY_RUN}" == "1" ]]; then
    log INFO "DRY_RUN: would prepare writable directories under ${INSTALL_ROOT}/var"
else
    bootstrap_var
fi

if [[ "${DRY_RUN}" != "1" ]]; then
    run_sudo chmod -R u+rwX,go+rX \
        "${INSTALL_ROOT}/bin" \
        "${INSTALL_ROOT}/lib" \
        "${INSTALL_ROOT}/docs" \
        "${INSTALL_ROOT}/systemd" \
        "${INSTALL_ROOT}/share"
    if [[ -d "${INSTALL_ROOT}/etc" ]]; then
        run_sudo find "${INSTALL_ROOT}/etc" -type f -exec chmod 644 {} +
    fi
fi

log INFO "Installation tree updated at ${INSTALL_ROOT}"
