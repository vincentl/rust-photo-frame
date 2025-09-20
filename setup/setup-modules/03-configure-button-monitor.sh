#!/usr/bin/env bash
set -euo pipefail

SERVICE_USER="${SUDO_USER:-pi}"
INSTALL_DIR="/opt/rust-photo-frame/button-monitor"
SCRIPT_SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")"/.. && pwd)/support/button-monitor.py"
SCRIPT_DEST="${INSTALL_DIR}/button-monitor.py"
SERVICE_FILE="/etc/systemd/system/button-monitor.service"

if [[ ! -f "${SCRIPT_SRC}" ]]; then
    echo "[03-configure-button-monitor] Button monitor script not found at ${SCRIPT_SRC}" >&2
    exit 1
fi

echo "[03-configure-button-monitor] Installing Python dependencies..."
apt-get install -y python3 python3-gpiozero python3-pip python3-venv

if [[ "${SERVICE_USER}" != "root" ]]; then
    echo "[03-configure-button-monitor] Ensuring ${SERVICE_USER} has GPIO/video access..."
    usermod -aG gpio,video "${SERVICE_USER}"
fi

mkdir -p "${INSTALL_DIR}"
cp "${SCRIPT_SRC}" "${SCRIPT_DEST}"
chmod 755 "${SCRIPT_DEST}"
chown -R "${SERVICE_USER}:${SERVICE_USER}" "${INSTALL_DIR}"

echo "[03-configure-button-monitor] Creating systemd service..."
cat <<SERVICE > "${SERVICE_FILE}"
[Unit]
Description=Rust Photo Frame Button Monitor
After=network.target

[Service]
Type=simple
ExecStart=/usr/bin/env python3 ${SCRIPT_DEST}
Restart=on-failure
User=${SERVICE_USER}
Group=${SERVICE_USER}
SupplementaryGroups=gpio video
Environment=BUTTON_GPIO=17

[Install]
WantedBy=multi-user.target
SERVICE

systemctl daemon-reload
systemctl enable button-monitor.service
systemctl restart button-monitor.service

echo "[03-configure-button-monitor] Button monitor service installed and started."
