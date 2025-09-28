#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FILES_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)/files"
SYSTEMD_DIR="${FILES_DIR}/systemd"
CONFIG_PATH="/etc/photo-frame/config.yaml"
SERVICE_DROPIN="/etc/systemd/system/sync-photos.service.d/override.conf"
TIMER_DROPIN="/etc/systemd/system/sync-photos.timer.d/override.conf"

if [[ $EUID -ne 0 ]]; then
    echo "[50-sync] This module must be run as root." >&2
    exit 1
fi

apt-get update
apt-get install -y rclone

install -Dm0644 "${SYSTEMD_DIR}/sync-photos.service" /etc/systemd/system/sync-photos.service
install -Dm0644 "${SYSTEMD_DIR}/sync-photos.timer" /etc/systemd/system/sync-photos.timer

mkdir -p "$(dirname "${SERVICE_DROPIN}")"
mkdir -p "$(dirname "${TIMER_DROPIN}")"

PHOTO_LIBRARY_PATH=""
if [[ -f "${CONFIG_PATH}" ]]; then
    PHOTO_LIBRARY_PATH="$(python - <<'PY'
import os
path = "${CONFIG_PATH}"
result = ""
try:
    with open(path, 'r', encoding='utf-8') as handle:
        for raw in handle:
            line = raw.strip()
            if not line or line.startswith('#'):
                continue
            if ':' not in line:
                continue
            key, value = line.split(':', 1)
            if key.strip() == 'photo-library-path':
                value = value.strip()
                if value.startswith(('"', "'")) and value.endswith(("'", '"')):
                    value = value[1:-1]
                result = value
                break
except FileNotFoundError:
    pass
except Exception as exc:
    result = f"ERROR:{exc}"
print(result)
PY
)"
    if [[ "${PHOTO_LIBRARY_PATH}" == ERROR:* ]]; then
        echo "[50-sync] Failed to parse ${CONFIG_PATH}: ${PHOTO_LIBRARY_PATH}" >&2
        PHOTO_LIBRARY_PATH=""
    fi
fi

SYNC_TOOL="${SYNC_TOOL:-rclone}"
SYNC_SCHEDULE="${SYNC_SCHEDULE:-hourly}"

{
    echo "[Service]"
    echo "Environment=\"PHOTO_LIBRARY_PATH=${PHOTO_LIBRARY_PATH}\""
    echo "Environment=\"SYNC_TOOL=${SYNC_TOOL}\""
    echo "Environment=\"SYNC_SCHEDULE=${SYNC_SCHEDULE}\""
} > "${SERVICE_DROPIN}"

{
    echo "[Timer]"
    echo "OnCalendar=${SYNC_SCHEDULE}"
} > "${TIMER_DROPIN}"

systemctl daemon-reload
systemctl enable --now sync-photos.timer

cat <<'INFO'
[50-sync] Installed sync service and timer.
[50-sync] Configure a remote with `rclone config` and set RCLONE_REMOTE via:
    sudo systemctl edit sync-photos.service
    [Service]
    Environment=RCLONE_REMOTE=myremote:photos
INFO

