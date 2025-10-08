#!/usr/bin/env bash
#
# Simulate a Wi-Fi authentication failure so the wifi-manager watchdog
# should fall back to hotspot provisioning. The script intentionally
# replaces the active profile with one that uses a wrong PSK and then
# tries to activate it. Once NetworkManager rejects the credentials the
# interface will drop, giving wifi-manager a chance to take over.
#
# Usage: sudo bash scripts/test-bad-psk.sh [interface]
# Defaults to wlan0 when no interface is provided.
#
# The script copies the active connection keyfile to
# ~/wifi-backup.nmconnection and schedules an automatic restore with `at`
# (if available). You can also manually restore the original profile with
# the command that prints at the end of the run.
#
set -euo pipefail

log() {
  printf '%s\n' "$*"
}

if [[ $EUID -ne 0 ]]; then
  log "This script must run as root (try: sudo bash $0 ${1:-})"
  exit 1
fi

IFACE="${1:-wlan0}"
BAD_NAME="wifi-bad"
BAD_PSK="wrong-password"
DELAY_MIN=5

ACTIVE_CONN=$(nmcli -t -f NAME,TYPE,DEVICE connection show --active \
  | awk -F: -v i="$IFACE" '$3==i && ($2=="802-11-wireless" || $2=="wifi"){print $1; exit}')
if [[ -z "$ACTIVE_CONN" ]]; then
  log "No active Wi-Fi on $IFACE"
  exit 1
fi
log "Active: $ACTIVE_CONN on $IFACE"

UUID=$(nmcli -g connection.uuid connection show "$ACTIVE_CONN")
KEYFILE=$(grep -rl "uuid=$UUID" /etc/NetworkManager/system-connections/ | head -1)
if [[ -z "$KEYFILE" ]]; then
  log "Could not find keyfile for $ACTIVE_CONN"
  exit 1
fi
cp "$KEYFILE" ~/wifi-backup.nmconnection
chmod 600 ~/wifi-backup.nmconnection
log "Backup: ~/wifi-backup.nmconnection"

if command -v at >/dev/null 2>&1; then
  echo "nmcli connection import type wifi file ~/wifi-backup.nmconnection >/dev/null 2>&1 || true; nmcli connection up \"$ACTIVE_CONN\" >/dev/null 2>&1 || true" \
    | at now + ${DELAY_MIN} minutes
  log "Auto-restore scheduled in ${DELAY_MIN} min"
else
  log "Warning: 'at' not installed; no auto-restore scheduled."
fi

SSID=$(nmcli -g 802-11-wireless.ssid connection show "$ACTIVE_CONN")
nmcli connection delete "$BAD_NAME" >/dev/null 2>&1 || true
nmcli connection add type wifi ifname "$IFACE" con-name "$BAD_NAME" ssid "$SSID" >/dev/null
nmcli connection modify "$BAD_NAME" wifi-sec.key-mgmt wpa-psk
nmcli connection modify "$BAD_NAME" wifi-sec.psk "$BAD_PSK"
log "Created '$BAD_NAME' for SSID '$SSID' with bad PSK"

nmcli connection down "$ACTIVE_CONN" || true
nmcli connection up "$BAD_NAME" || true
log "Switched to '$BAD_NAME' (auth should fail)."
log "Logs: sudo journalctl -fu photoframe-wifi-manager.service    and    sudo journalctl -fu NetworkManager"
log "Restore: nmcli connection delete \"$BAD_NAME\"; nmcli connection import type wifi file ~/wifi-backup.nmconnection; nmcli connection up \"$ACTIVE_CONN\""
