#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
FILES_DIR="${REPO_ROOT}/setup/files"
STAGE_DIR="${SCRIPT_DIR}/../stage"

BIN_SRC="${REPO_ROOT}/target/release/wifi-manager"
if [[ ! -x "${BIN_SRC}" ]]; then
    echo "[ERROR] Expected built binary at ${BIN_SRC}" >&2
    exit 1
fi

rm -rf "${STAGE_DIR}"
mkdir -p "${STAGE_DIR}/bin" "${STAGE_DIR}/etc" "${STAGE_DIR}/lib/wifi-ui" "${STAGE_DIR}/systemd" "${STAGE_DIR}/share"

install -m 0755 "${BIN_SRC}" "${STAGE_DIR}/bin/wifi-manager"
if [[ -d "${FILES_DIR}/bin" ]]; then
    for helper in "${FILES_DIR}/bin"/*; do
        [[ -f "${helper}" ]] || continue
        install -m 0755 "${helper}" "${STAGE_DIR}/bin/$(basename "${helper}")"
    done
fi
install -m 0644 "${FILES_DIR}/etc/wifi-manager.yaml" "${STAGE_DIR}/etc/wifi-manager.yaml"
install -m 0644 "${FILES_DIR}/wordlist.txt" "${STAGE_DIR}/share/wordlist.txt"
if [[ -d "${FILES_DIR}/lib/wifi-ui" ]]; then
    cp -R "${FILES_DIR}/lib/wifi-ui/." "${STAGE_DIR}/lib/wifi-ui/"
fi
install -m 0644 "${FILES_DIR}/systemd/wifi-manager.service" "${STAGE_DIR}/systemd/wifi-manager.service"

echo "[INFO] Staged wifi-manager artifacts to ${STAGE_DIR}"
