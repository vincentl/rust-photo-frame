#!/usr/bin/env bash
set -euo pipefail

MODULE="app:20-stage"
DRY_RUN="${DRY_RUN:-0}"
CARGO_PROFILE="${CARGO_PROFILE:-release}"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/photo-frame}"
SERVICE_USER="${SERVICE_USER:-$(id -un)}"
SERVICE_GROUP="${SERVICE_GROUP:-$(id -gn)}"
INSTALL_ROOT_ESC="$(printf '%s' "${INSTALL_ROOT}" | sed 's/[&/]/\\&/g')"
SERVICE_USER_ESC="$(printf '%s' "${SERVICE_USER}" | sed 's/[&/]/\\&/g')"
SERVICE_GROUP_ESC="$(printf '%s' "${SERVICE_GROUP}" | sed 's/[&/]/\\&/g')"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "${SCRIPT_DIR}/../../.." && pwd)}"
STAGE_ROOT="${STAGE_ROOT:-${SCRIPT_DIR}/../build}"
STAGE_DIR="${STAGE_ROOT}/stage"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${MODULE}" "$level: $*"
}

ensure_dir() {
    local dir="$1"
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would create directory ${dir}"
    else
        mkdir -p "${dir}"
    fi
}

cleanup_stage() {
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would remove existing stage directory ${STAGE_DIR}"
    else
        rm -rf "${STAGE_DIR}"
    fi
}

create_stage_layout() {
    cleanup_stage
    for dir in \
        "${STAGE_DIR}" \
        "${STAGE_DIR}/bin" \
        "${STAGE_DIR}/lib" \
        "${STAGE_DIR}/etc" \
        "${STAGE_DIR}/docs" \
        "${STAGE_DIR}/systemd" \
        "${STAGE_DIR}/share" \
        "${STAGE_DIR}/share/backgrounds"; do
        ensure_dir "${dir}"
    done
}

get_target_dir() {
    if [[ "${CARGO_PROFILE}" == "release" ]]; then
        printf '%s' "${REPO_ROOT}/target/release"
    else
        printf '%s' "${REPO_ROOT}/target/${CARGO_PROFILE}"
    fi
}

TARGET_DIR="$(get_target_dir)"
BINARY_SRC="${TARGET_DIR}/rust-photo-frame"
BINARY_DEST="${STAGE_DIR}/bin/rust-photo-frame"

create_stage_layout

if [[ ! -f "${BINARY_SRC}" ]]; then
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would take binary from ${BINARY_SRC}"
    else
        log ERROR "Expected binary not found at ${BINARY_SRC}. Run build module first."
        exit 1
    fi
fi

if [[ "${DRY_RUN}" == "1" ]]; then
    log INFO "DRY_RUN: would install ${BINARY_SRC} -> ${BINARY_DEST}"
else
    install -Dm755 "${BINARY_SRC}" "${BINARY_DEST}"
fi

CONFIG_SRC="${REPO_ROOT}/config.yaml"
if [[ -f "${CONFIG_SRC}" ]]; then
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would copy default config to ${STAGE_DIR}/etc/config.yaml"
    else
        install -Dm644 "${CONFIG_SRC}" "${STAGE_DIR}/etc/config.yaml"
    fi
else
    log WARN "Default config.yaml not found at repo root"
fi

B64_BACKGROUND_SRC="${REPO_ROOT}/assets/backgrounds/default-fixed.jpg.b64"
BACKGROUND_DEST="${STAGE_DIR}/share/backgrounds/default-fixed.jpg"
if [[ -f "${B64_BACKGROUND_SRC}" ]]; then
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would decode fixed background asset to ${BACKGROUND_DEST}"
    else
        if ! base64 --decode "${B64_BACKGROUND_SRC}" | install -Dm644 /dev/stdin "${BACKGROUND_DEST}"; then
            log ERROR "Failed to decode fixed background asset from ${B64_BACKGROUND_SRC}"
            exit 1
        fi
    fi
else
    log WARN "Fixed background asset not found at ${B64_BACKGROUND_SRC}"
fi

DOCS_SRC="${REPO_ROOT}/docs"
if [[ -d "${DOCS_SRC}" ]]; then
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would sync documentation into ${STAGE_DIR}/docs"
    else
        rsync -a --delete "${DOCS_SRC}/" "${STAGE_DIR}/docs/"
    fi
fi

if [[ -f "${REPO_ROOT}/LICENSE" ]]; then
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would copy LICENSE into docs directory"
    else
        install -Dm644 "${REPO_ROOT}/LICENSE" "${STAGE_DIR}/docs/LICENSE"
    fi
fi

SYSTEMD_SRC="${SCRIPT_DIR}/../systemd"
if [[ -d "${SYSTEMD_SRC}" ]]; then
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would stage systemd units from ${SYSTEMD_SRC} with INSTALL_ROOT=${INSTALL_ROOT} and SERVICE_USER=${SERVICE_USER}"
    else
        rsync -a --delete "${SYSTEMD_SRC}/" "${STAGE_DIR}/systemd/"
        shopt -s nullglob
        for unit in "${STAGE_DIR}/systemd"/*.service "${STAGE_DIR}/systemd"/*.timer; do
            [[ -f "${unit}" ]] || continue
            sed -i \
                -e "s|@INSTALL_ROOT@|${INSTALL_ROOT_ESC}|g" \
                -e "s|@SERVICE_USER@|${SERVICE_USER_ESC}|g" \
                -e "s|@SERVICE_GROUP@|${SERVICE_GROUP_ESC}|g" \
                "${unit}"
        done
        shopt -u nullglob
    fi
fi

log INFO "Stage artifacts prepared at ${STAGE_DIR}"
