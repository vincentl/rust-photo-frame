#!/usr/bin/env bash
#
# Switch Wifi to an invalid SSID/password and restore in 5 minutes

set -euo pipefail

if ! command -v at >/dev/null 2>&1; then
  echo "[wifi-test] Error: 'at' command not found. Please install it with:"
  echo "  sudo apt install at"
  exit 1
fi

IFACE="${1:-wlan0}"
BAD_NAME="wifi-bad"
BAD_PSK="wrong-password"
DELAY_MIN=5

ACTIVE_CONN=$(nmcli -t -f NAME,TYPE,DEVICE connection show --active \
  | awk -F: -v i="$IFACE" '$3==i && ($2=="802-11-wireless" || $2=="wifi"){print $1; exit}')
[[ -n "$ACTIVE_CONN" ]] || { echo "No active Wi-Fi on $IFACE"; exit 1; }
echo "Active: $ACTIVE_CONN on $IFACE"

UUID=$(nmcli -g connection.uuid connection show "$ACTIVE_CONN")
KEYFILE=$(sudo grep -rl "uuid=$UUID" /etc/NetworkManager/system-connections/ | head -1)
[[ -n "$KEYFILE" ]] || { echo "Could not find keyfile for $ACTIVE_CONN"; exit 1; }
sudo cp "$KEYFILE" ~/wifi-backup.nmconnection && sudo chmod 600 ~/wifi-backup.nmconnection
echo "Backup: ~/wifi-backup.nmconnection"

if command -v at >/dev/null 2>&1; then
  echo "nmcli connection import type wifi file ~/wifi-backup.nmconnection >/dev/null 2>&1 || true; nmcli connection up \"$ACTIVE_CONN\" >/dev/null 2>&1 || true" \
    | at now + ${DELAY_MIN} minutes
  echo "Auto-restore scheduled in ${DELAY_MIN} min"
else
  echo "Warning: 'at' not installed; no auto-restore scheduled."
fi

SSID=$(nmcli -g 802-11-wireless.ssid connection show "$ACTIVE_CONN")
sudo nmcli connection delete "$BAD_NAME" >/dev/null 2>&1 || true
sudo nmcli connection add type wifi ifname "$IFACE" con-name "$BAD_NAME" ssid "$SSID" \
  wifi-sec.key-mgmt wpa-psk wifi-sec.psk "$BAD_PSK" >/dev/null
echo "Created '$BAD_NAME' for SSID '$SSID' with bad PSK"

sudo nmcli connection down "$ACTIVE_CONN" || true
sudo nmcli connection up "$BAD_NAME" || true
echo "Switched to '$BAD_NAME' (auth should fail)."
echo "Logs: sudo journalctl -fu wifi-watcher.service    and    sudo journalctl -fu NetworkManager"
echo "Restore: sudo nmcli connection delete \"$BAD_NAME\"; nmcli connection import type wifi file ~/wifi-backup.nmconnection; nmcli connection up \"$ACTIVE_CONN\""
