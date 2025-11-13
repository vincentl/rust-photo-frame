#!/usr/bin/env bash
set -euo pipefail

MODULE="app:20-stage"
CARGO_PROFILE="${CARGO_PROFILE:-release}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "${SCRIPT_DIR}/../../.." && pwd)}"
STAGE_ROOT="${STAGE_ROOT:-${SCRIPT_DIR}/../build}"
STAGE_DIR="${STAGE_ROOT}/stage"

ASSETS_APP_ROOT="${REPO_ROOT}/setup/assets/app"
SYSTEM_UNITS_DIR="${REPO_ROOT}/assets/systemd"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${MODULE}" "$level: $*"
}

cleanup_stage() {
    rm -rf "${STAGE_DIR}"
    mkdir -p "${STAGE_DIR}"
}

create_stage_layout() {
    local dir
    cleanup_stage
    for dir in bin etc docs share systemd; do
        mkdir -p "${STAGE_DIR}/${dir}"
    done
    mkdir -p "${STAGE_DIR}/share/backgrounds"
}

get_target_dir() {
    if [[ "${CARGO_PROFILE}" == "release" ]]; then
        printf '%s' "${REPO_ROOT}/target/release"
    else
        printf '%s' "${REPO_ROOT}/target/${CARGO_PROFILE}"
    fi
}

stage_binary() {
    local src="$1" dest="$2" label="$3"
    if [[ ! -f "${src}" ]]; then
        log ERROR "Expected ${label} binary not found at ${src}. Run the build module first."
        exit 1
    fi
    install -Dm755 "${src}" "${dest}"
}

copy_tree() {
    local src="$1" dest="$2" mode="$3"
    [[ -d "${src}" ]] || return 0
    while IFS= read -r -d '' file; do
        local rel="${file#${src}/}"
        install -Dm"${mode}" "${file}" "${dest}/${rel}"
    done < <(find "${src}" -type f -print0)
}

create_stage_layout

TARGET_DIR="$(get_target_dir)"

stage_binary "${TARGET_DIR}/photo-frame" "${STAGE_DIR}/bin/photo-frame" "photo-frame"
stage_binary "${TARGET_DIR}/wifi-manager" "${STAGE_DIR}/bin/wifi-manager" "wifi-manager"
if [[ -f "${TARGET_DIR}/buttond" ]]; then
    stage_binary "${TARGET_DIR}/buttond" "${STAGE_DIR}/bin/buttond" "buttond"
else
    log WARN "buttond binary not built; button service will not be installed"
fi

copy_tree "${ASSETS_APP_ROOT}/bin" "${STAGE_DIR}/bin" 755
copy_tree "${ASSETS_APP_ROOT}/etc" "${STAGE_DIR}/etc" 644
copy_tree "${ASSETS_APP_ROOT}/share" "${STAGE_DIR}/share" 644

if [[ -f "${ASSETS_APP_ROOT}/share/wordlist.txt" ]]; then
    install -Dm644 "${ASSETS_APP_ROOT}/share/wordlist.txt" "${STAGE_DIR}/share/wordlist.txt"
fi

if [[ -f "${REPO_ROOT}/config.yaml" ]]; then
    install -Dm644 "${REPO_ROOT}/config.yaml" "${STAGE_DIR}/etc/photo-frame/config.yaml"
else
    log WARN "Default config.yaml not found at repo root"
fi

if [[ -d "${REPO_ROOT}/docs" ]]; then
    rsync -a --delete "${REPO_ROOT}/docs/" "${STAGE_DIR}/docs/"
fi

if [[ -f "${REPO_ROOT}/LICENSE" ]]; then
    install -Dm644 "${REPO_ROOT}/LICENSE" "${STAGE_DIR}/docs/LICENSE"
fi

if [[ -d "${SYSTEM_UNITS_DIR}" ]]; then
    rsync -a --delete "${SYSTEM_UNITS_DIR}/" "${STAGE_DIR}/systemd/"
fi

log INFO "Stage artifacts prepared at ${STAGE_DIR}"
