#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STAGE_DIR="${SCRIPT_DIR}/../stage"
INSTALL_ROOT="/opt/photo-frame"
SERVICE_USER="photo-frame"
SERVICE_GROUP="photo-frame"

if [[ ! -d "${STAGE_DIR}" ]]; then
    echo "[ERROR] Stage directory ${STAGE_DIR} missing. Run 20-stage.sh first." >&2
    exit 1
fi

install -d -m 0755 "${INSTALL_ROOT}" "${INSTALL_ROOT}/bin" "${INSTALL_ROOT}/lib" "${INSTALL_ROOT}/etc" "${INSTALL_ROOT}/systemd" "${INSTALL_ROOT}/var" "${INSTALL_ROOT}/share"
install -m 0755 "${STAGE_DIR}/bin/wifi-manager" "${INSTALL_ROOT}/bin/wifi-manager"
for helper in "${STAGE_DIR}/bin"/*; do
    [[ -f "${helper}" ]] || continue
    base="$(basename "${helper}")"
    if [[ "${base}" == "wifi-manager" ]]; then
        continue
    fi
    install -m 0755 "${helper}" "${INSTALL_ROOT}/bin/${base}"
done

CONFIG_TARGET="${INSTALL_ROOT}/etc/wifi-manager.yaml"
if [[ ! -f "${CONFIG_TARGET}" ]]; then
    install -m 0644 "${STAGE_DIR}/etc/wifi-manager.yaml" "${CONFIG_TARGET}"
else
    install -m 0644 "${STAGE_DIR}/etc/wifi-manager.yaml" "${CONFIG_TARGET}.default"
    echo "[INFO] Existing wifi-manager.yaml preserved; updated template at ${CONFIG_TARGET}.default"
fi

install -m 0644 "${STAGE_DIR}/share/wordlist.txt" "${INSTALL_ROOT}/share/wordlist.txt"
if [[ -d "${STAGE_DIR}/lib/wifi-ui" ]]; then
    rm -rf "${INSTALL_ROOT}/lib/wifi-ui"
    mkdir -p "${INSTALL_ROOT}/lib"
    cp -R "${STAGE_DIR}/lib/wifi-ui" "${INSTALL_ROOT}/lib/"
else
    mkdir -p "${INSTALL_ROOT}/lib/wifi-ui"
fi

install -d -m 0755 "${INSTALL_ROOT}/var"
chown -R "${SERVICE_USER}:${SERVICE_GROUP}" "${INSTALL_ROOT}/var"
chmod 0755 "${INSTALL_ROOT}/var"

install -m 0644 "${STAGE_DIR}/systemd/wifi-manager.service" "${INSTALL_ROOT}/systemd/wifi-manager.service"

echo "[INFO] Installed wifi-manager to ${INSTALL_ROOT}"
