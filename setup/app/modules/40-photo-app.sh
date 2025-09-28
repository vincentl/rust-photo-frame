#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FILES_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)/files"
SYSTEMD_DIR="${FILES_DIR}/systemd"

if [[ $EUID -ne 0 ]]; then
    echo "[40-photo-app] This module must be run as root." >&2
    exit 1
fi

install -Dm0644 "${SYSTEMD_DIR}/photo-app.service" /etc/systemd/system/photo-app.service
install -Dm0644 "${SYSTEMD_DIR}/photo-app.target" /etc/systemd/system/photo-app.target

systemctl daemon-reload

# Ensure the target is enabled so the watcher can start it on demand
systemctl enable photo-app.target

# Explicitly disable direct startup of the service; the watcher will manage it
systemctl disable photo-app.service >/dev/null 2>&1 || true

echo "[40-photo-app] Photo application units installed."
