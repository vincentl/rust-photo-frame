#!/usr/bin/env bash
set -euo pipefail

MODULE="app:35-fonts"

FONT_NAME="Macondo"
FONT_URL="https://github.com/google/fonts/raw/main/ofl/macondo/Macondo-Regular.ttf"
FONT_DIR="/usr/local/share/fonts/google/${FONT_NAME}"
FONT_BASENAME="$(basename "${FONT_URL}")"
FONT_PATH="${FONT_DIR}/${FONT_BASENAME}"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${MODULE}" "$level: $*"
}

run_sudo() {
    sudo "$@"
}

install_font() {
    run_sudo mkdir -p "${FONT_DIR}"

    tmpfile="$(mktemp)"
    cleanup() {
        rm -f "${tmpfile}"
    }
    trap cleanup EXIT

    log INFO "Downloading ${FONT_NAME} font"
    curl -fL -o "${tmpfile}" "${FONT_URL}"

    run_sudo install -m 644 "${tmpfile}" "${FONT_PATH}"
    trap - EXIT
    cleanup

    log INFO "Refreshing font cache"
    run_sudo fc-cache -fv >/dev/null

    log INFO "Installed ${FONT_NAME} font at ${FONT_PATH}"
}

if [[ -f "${FONT_PATH}" ]]; then
    log INFO "${FONT_NAME} font already present at ${FONT_PATH}; skipping download"
    log INFO "Refreshing font cache"
    run_sudo fc-cache -fv >/dev/null
    exit 0
fi

install_font
