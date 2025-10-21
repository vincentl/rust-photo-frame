#!/usr/bin/env bash
#
# Simulate a Wi-Fi authentication failure so the wifi-manager watchdog
# should fall back to hotspot provisioning. The script intentionally
# replaces the active profile with one that uses a wrong PSK and then
# tries to activate it. Once NetworkManager rejects the credentials the
# interface will drop, giving wifi-manager a chance to take over.
#
# Usage: sudo bash developer/suspend-wifi.sh [interface]
# Defaults to wlan0 when no interface is provided.
#
# The script copies the active connection keyfile to
# /root/wifi-backup.nmconnection and schedules an automatic restore.
#
# Scheduler strategy (Raspberry Pi OS Trixie):
# - Use a transient one-shot systemd unit via systemd-run. This avoids any
#   dependency on at/atd, survives SSH disconnects, and is easy to inspect
#   with journalctl. If systemd-run is not available, the script refuses to
#   proceed (to avoid stranding the device offline).
#
# Note: we use absolute paths and avoid '~' because at(1) runs /bin/sh
# (dash on Debian) which does not expand tildes.
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
NMCLI="$(command -v nmcli || echo /usr/bin/nmcli)"
BACKUP="/root/wifi-backup.nmconnection"
SCHEDULED_UNIT=""

schedule_restore() {
  local backup="$1"; shift
  local conn_name="$1"; shift
  local delay_min="$1"; shift
  local when="${delay_min}m"
  # Absolute path for nmcli and backup file; minimal environment in timers.
  local cmd="$NMCLI connection import type wifi file '$backup' >/dev/null 2>&1 || true; $NMCLI connection up '$conn_name' >/dev/null 2>&1 || true"

  if ! command -v systemd-run >/dev/null 2>&1; then
    log "systemd-run is required to schedule auto-restore. Aborting to avoid stranding Wi‑Fi."
    log "Manual restore would be: $NMCLI connection import type wifi file $backup; $NMCLI connection up '$conn_name'"
    exit 2
  fi

  # One-shot transient unit that runs detached from this SSH session.
  local unit="wifi-restore-$(date +%s)"
  systemd-run --unit "$unit" --on-active="$when" --service-type=oneshot \
    --property=RequiresMountsFor="$backup" \
    /bin/sh -lc "$cmd" >/dev/null 2>&1 || true
  SCHEDULED_UNIT="$unit"
  log "Auto-restore scheduled in ${delay_min} min via systemd-run ($unit)"
  }

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
cp "$KEYFILE" "$BACKUP"
chmod 600 "$BACKUP"
log "Backup: $BACKUP"

schedule_restore "$BACKUP" "$ACTIVE_CONN" "$DELAY_MIN"

SSID=$(nmcli -g 802-11-wireless.ssid connection show "$ACTIVE_CONN")
nmcli connection delete "$BAD_NAME" >/dev/null 2>&1 || true
nmcli connection add type wifi ifname "$IFACE" con-name "$BAD_NAME" ssid "$SSID" >/dev/null
nmcli connection modify "$BAD_NAME" wifi-sec.key-mgmt wpa-psk
nmcli connection modify "$BAD_NAME" wifi-sec.psk "$BAD_PSK"
log "Created '$BAD_NAME' for SSID '$SSID' with bad PSK"

nmcli connection down "$ACTIVE_CONN" || true
nmcli connection up "$BAD_NAME" || true
log "Switched to '$BAD_NAME' (auth should fail)."
if [[ -n "$SCHEDULED_UNIT" ]]; then
  log "Restore timer logs: sudo journalctl -u $SCHEDULED_UNIT"
fi
log "Logs: sudo journalctl -fu photoframe-wifi-manager.service    and    sudo journalctl -fu NetworkManager"
log "Restore: nmcli connection delete \"$BAD_NAME\"; nmcli connection import type wifi file $BACKUP; nmcli connection up \"$ACTIVE_CONN\""
