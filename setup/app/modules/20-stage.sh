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
WIFI_MANAGER_SRC="${TARGET_DIR}/wifi-manager"
WIFI_MANAGER_DEST="${STAGE_DIR}/bin/wifi-manager"
FILES_ROOT="${REPO_ROOT}/setup/files"

create_stage_layout

if [[ ! -f "${BINARY_SRC}" ]]; then
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would take binary from ${BINARY_SRC}"
    else
        log ERROR "Expected binary not found at ${BINARY_SRC}. Run build module first."
        exit 1
    fi
fi

stage_binary() {
    local src="$1"
    local dest="$2"
    local name="$3"
    if [[ ! -f "${src}" ]]; then
        if [[ "${DRY_RUN}" == "1" ]]; then
            log INFO "DRY_RUN: would stage ${name} from ${src}"
            return
        fi
        log ERROR "Expected ${name} binary not found at ${src}. Run build module first."
        exit 1
    fi
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would install ${src} -> ${dest}"
    else
        install -Dm755 "${src}" "${dest}"
    fi
}

copy_tree() {
    local src_dir="$1"
    local dest_root="$2"
    local mode="$3"
    local description="$4"
    [[ -d "${src_dir}" ]] || return
    while IFS= read -r -d '' file; do
        local rel_path="${file#${src_dir}/}"
        local dest_path="${dest_root}/${rel_path}"
        if [[ "${DRY_RUN}" == "1" ]]; then
            log INFO "DRY_RUN: would install ${description} ${file} -> ${dest_path}"
        else
            install -Dm"${mode}" "${file}" "${dest_path}"
        fi
    done < <(find "${src_dir}" -type f -print0)
}

stage_binary "${BINARY_SRC}" "${BINARY_DEST}" "photo-frame"
stage_binary "${WIFI_MANAGER_SRC}" "${WIFI_MANAGER_DEST}" "wifi-manager"

POWERCTL_SRC="${REPO_ROOT}/setup/app/powerctl"
if [[ -f "${POWERCTL_SRC}" ]]; then
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would install powerctl helper to ${STAGE_DIR}/bin/powerctl"
    else
        install -Dm755 "${POWERCTL_SRC}" "${STAGE_DIR}/bin/powerctl"
    fi
fi

copy_tree "${FILES_ROOT}/bin" "${STAGE_DIR}/bin" 755 "helper script"
copy_tree "${FILES_ROOT}/etc" "${STAGE_DIR}/etc" 644 "config template"
copy_tree "${FILES_ROOT}/share" "${STAGE_DIR}/share" 644 "shared asset"

WORDLIST_SRC="${FILES_ROOT}/wordlist.txt"
if [[ -f "${WORDLIST_SRC}" ]]; then
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would install hotspot wordlist to ${STAGE_DIR}/share/wordlist.txt"
    else
        install -Dm644 "${WORDLIST_SRC}" "${STAGE_DIR}/share/wordlist.txt"
    fi
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

BACKGROUND_SRC_DIR="${REPO_ROOT}/assets/background"
if [[ -d "${BACKGROUND_SRC_DIR}" ]]; then
    copy_tree "${BACKGROUND_SRC_DIR}" "${STAGE_DIR}/share/backgrounds" 644 "background asset"
else
    log INFO "No background assets found under ${BACKGROUND_SRC_DIR}"
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
EXTRA_SYSTEMD_SRC="${FILES_ROOT}/systemd"

stage_systemd_units() {
    local source_dir="$1"
    [[ -d "${source_dir}" ]] || return
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would stage systemd units from ${source_dir} with INSTALL_ROOT=${INSTALL_ROOT} and SERVICE_USER=${SERVICE_USER}"
        return
    fi
    rsync -a "${source_dir}/" "${STAGE_DIR}/systemd/"
}

stage_systemd_units "${SYSTEMD_SRC}"
stage_systemd_units "${EXTRA_SYSTEMD_SRC}"

if [[ "${DRY_RUN}" != "1" ]]; then
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

log INFO "Stage artifacts prepared at ${STAGE_DIR}"
