#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
FILES_DIR="${REPO_ROOT}/files"
SYSTEMD_DIR="${FILES_DIR}/systemd"
BIN_DIR="/opt/photo-frame/bin"
RUN_DIR="/run/photo-frame"

if [[ $EUID -ne 0 ]]; then
    echo "[30-wifi-watcher] This module must be run as root." >&2
    exit 1
fi

if [[ ! -d "${SYSTEMD_DIR}" ]]; then
    echo "[30-wifi-watcher] Systemd directory missing: ${SYSTEMD_DIR}" >&2
    exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "[30-wifi-watcher] cargo not found. Run the Rust toolchain module first." >&2
    exit 1
fi

pushd "${REPO_ROOT}/.." >/dev/null

# Build wifi-watcher binary
cargo build --release --manifest-path "${REPO_ROOT}/../crates/wifi-watcher/Cargo.toml"
# Build wifi-setter binary
cargo build --release --manifest-path "${REPO_ROOT}/../crates/wifi-setter/Cargo.toml"

popd >/dev/null

install -d -m 0755 "${BIN_DIR}"
install -m 0755 "${REPO_ROOT}/../target/release/wifi-watcher" "${BIN_DIR}/wifi-watcher"
install -m 0755 "${REPO_ROOT}/../target/release/wifi-setter" "${BIN_DIR}/wifi-setter"

install -Dm0644 "${SYSTEMD_DIR}/wifi-watcher.service" /etc/systemd/system/wifi-watcher.service
install -Dm0644 "${SYSTEMD_DIR}/wifi-hotspot@.service" /etc/systemd/system/wifi-hotspot@.service
install -Dm0644 "${SYSTEMD_DIR}/wifi-setter.service" /etc/systemd/system/wifi-setter.service
install -Dm0755 "${FILES_DIR}/bin/print-status.sh" "${BIN_DIR}/print-status.sh"

install -d -m 0755 "${RUN_DIR}"

systemctl daemon-reload
systemctl enable wifi-watcher.service

echo "[30-wifi-watcher] Wi-Fi watcher and provisioning services installed."
