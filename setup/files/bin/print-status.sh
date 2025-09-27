#!/usr/bin/env bash
set -euo pipefail

WIFI_IFNAME="${WIFI_IFNAME:-wlan0}"

printf '\n=== Wi-Fi Connectivity ===\n'
if nmcli -t -f CONNECTIVITY general status >/tmp/connectivity 2>&1; then
    CONNECTIVITY="$(cat /tmp/connectivity | tr -d '\n')"
    printf 'Connectivity: %s\n' "${CONNECTIVITY:-unknown}"
else
    printf 'Connectivity: error (%s)\n' "$(cat /tmp/connectivity)"
fi
rm -f /tmp/connectivity

ACTIVE_WIFI="$(nmcli -t -f NAME,DEVICE,TYPE connection show --active 2>/dev/null | awk -F: '$3=="wifi" {print $1 " on " $2}')"
if [[ -n "${ACTIVE_WIFI}" ]]; then
    printf 'Active connection: %s\n' "${ACTIVE_WIFI}"
else
    printf 'Active connection: none\n'
fi

printf '\n=== Hotspot & Provisioning ===\n'
HOTSPOT_STATE="$(systemctl is-active "wifi-hotspot@${WIFI_IFNAME}.service" 2>/dev/null || true)"
printf 'Hotspot (%s): %s\n' "${WIFI_IFNAME}" "${HOTSPOT_STATE:-unknown}"
SETTER_STATE="$(systemctl is-active wifi-setter.service 2>/dev/null || true)"
printf 'WiFi setter service: %s\n' "${SETTER_STATE:-unknown}"
WATCHER_STATE="$(systemctl is-active wifi-watcher.service 2>/dev/null || true)"
printf 'WiFi watcher service: %s\n' "${WATCHER_STATE:-unknown}"

printf '\n=== Photo App ===\n'
PHOTO_STATE="$(systemctl is-active photo-app.service 2>/dev/null || true)"
printf 'photo-app.service: %s\n' "${PHOTO_STATE:-unknown}"
TARGET_STATE="$(systemctl is-active photo-app.target 2>/dev/null || true)"
printf 'photo-app.target: %s\n' "${TARGET_STATE:-unknown}"

printf '\n=== Sync ===\n'
SYNC_STATE="$(systemctl is-active sync-photos.service 2>/dev/null || true)"
printf 'sync-photos.service: %s\n' "${SYNC_STATE:-unknown}"
NEXT_TIMER="$(systemctl list-timers --all 2>/dev/null | awk '/sync-photos.timer/ {print $5, "(left: "$6")"; exit}')"
if [[ -n "${NEXT_TIMER}" ]]; then
    printf 'Next sync: %s\n' "${NEXT_TIMER}"
else
    printf 'Next sync: timer inactive\n'
fi
LAST_TRIGGER="$(systemctl show -p LastTriggerUSec sync-photos.timer 2>/dev/null | cut -d= -f2)"
if [[ -n "${LAST_TRIGGER}" && "${LAST_TRIGGER}" != "n/a" ]]; then
    printf 'Last sync trigger: %s\n' "${LAST_TRIGGER}"
else
    printf 'Last sync trigger: never\n'
fi

printf '\nStatus summary complete.\n'
