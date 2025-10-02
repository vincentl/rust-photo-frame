#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

if [[ $(id -u) -ne 0 ]]; then
    echo "legacy-cleanup.sh must be run as root" >&2
    exit 1
fi

LEGACY_UNITS=(
    sync-photos.service
    sync-photos.timer
    wifi-manager.service
    photo-buttond.service
)

for unit in "${LEGACY_UNITS[@]}"; do
    if systemctl list-unit-files "${unit}" >/dev/null 2>&1; then
        systemctl disable "${unit}" >/dev/null 2>&1 || true
    fi
    if systemctl is-active --quiet "${unit}"; then
        systemctl stop "${unit}" || true
    fi
    rm -f "/etc/systemd/system/${unit}"
    rm -f "/etc/systemd/system/${unit}.d"/*.conf 2>/dev/null || true
    rm -rf "/etc/systemd/system/${unit}.d" 2>/dev/null || true
    rm -f "/etc/systemd/system/timers.target.wants/${unit}" 2>/dev/null || true
    rm -f "/etc/systemd/system/multi-user.target.wants/${unit}" 2>/dev/null || true
    rm -f "/etc/systemd/system/graphical.target.wants/${unit}" 2>/dev/null || true
    echo "Removed legacy unit ${unit}"
done

rm -f /etc/pam.d/cage.local 2>/dev/null || true
rm -f /etc/systemd/system/cage@.service.d/override.conf 2>/dev/null || true
rm -rf /etc/systemd/system/cage@.service.d 2>/dev/null || true

if [[ -d /opt/photo-frame/run ]]; then
    rm -rf /opt/photo-frame/run
    echo "Removed legacy /opt/photo-frame/run directory"
fi

systemctl daemon-reload
printf 'Legacy unit cleanup complete. Review enablement of new photoframe services.\n'
