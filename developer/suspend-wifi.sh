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
BAD_NAME="wifi-bad-${IFACE}"
BAD_PSK="wrong-password"
DELAY_MIN=5
NMCLI="$(command -v nmcli || echo /usr/bin/nmcli)"
BACKUP="/root/wifi-backup.nmconnection"
SCHEDULED_UNIT=""

schedule_restore() {
  local backup="$1"; shift
  local conn_name="$1"; shift
  local delay_min="$1"; shift
  local autoconnect="$1"; shift
  local device_autoconnect="$1"; shift
  local iface="$1"; shift
  local bad_name="$1"; shift
  local when="${delay_min}m"
  # Absolute path for nmcli and backup file; minimal environment in timers.
  local cmd=""

  if [[ -n "${backup}" ]]; then
    cmd+="$NMCLI connection import type wifi file '$backup' >/dev/null 2>&1 || true; "
  fi
  cmd+="$NMCLI device set '$iface' autoconnect '$device_autoconnect' >/dev/null 2>&1 || true; "
  cmd+="$NMCLI connection modify '$conn_name' connection.autoconnect '$autoconnect' >/dev/null 2>&1 || true; "
  cmd+="$NMCLI connection up '$conn_name' >/dev/null 2>&1 || true; "
  cmd+="$NMCLI connection delete '$bad_name' >/dev/null 2>&1 || true"

  if ! command -v systemd-run >/dev/null 2>&1; then
    log "systemd-run is required to schedule auto-restore. Aborting to avoid stranding Wi‑Fi."
    log "Manual restore would be: $NMCLI device set '$iface' autoconnect '$device_autoconnect'; $NMCLI connection modify '$conn_name' connection.autoconnect '$autoconnect'; $NMCLI connection up '$conn_name'; $NMCLI connection delete '$bad_name'"
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

find_keyfile_by_uuid() {
  local uuid="$1"
  local keyfile=""
  local base_dir
  for base_dir in /etc/NetworkManager/system-connections /run/NetworkManager/system-connections; do
    [[ -d "${base_dir}" ]] || continue
    keyfile="$(grep -rl "uuid=${uuid}" "${base_dir}" 2>/dev/null | head -1 || true)"
    if [[ -n "${keyfile}" ]]; then
      printf '%s' "${keyfile}"
      return 0
    fi
  done
  return 1
}

ACTIVE_CONN=$(nmcli -t -f NAME,TYPE,DEVICE connection show --active \
  | awk -F: -v i="$IFACE" '$3==i && ($2=="802-11-wireless" || $2=="wifi"){print $1; exit}')
if [[ -z "$ACTIVE_CONN" ]]; then
  log "No active Wi-Fi on $IFACE"
  exit 1
fi
log "Active: $ACTIVE_CONN on $IFACE"

ORIG_AUTOCONNECT="$($NMCLI -g connection.autoconnect connection show "$ACTIVE_CONN" 2>/dev/null | head -n1 | tr -d '\r')"
if [[ -z "${ORIG_AUTOCONNECT}" ]]; then
  ORIG_AUTOCONNECT="yes"
fi
log "Original autoconnect for '$ACTIVE_CONN': ${ORIG_AUTOCONNECT}"

ORIG_DEVICE_AUTOCONNECT="$($NMCLI -g GENERAL.AUTOCONNECT device show "$IFACE" 2>/dev/null | head -n1 | tr -d '\r' | awk '{print $1}')"
if [[ -z "${ORIG_DEVICE_AUTOCONNECT}" ]]; then
  ORIG_DEVICE_AUTOCONNECT="yes"
fi
log "Original device autoconnect for '$IFACE': ${ORIG_DEVICE_AUTOCONNECT}"

UUID=$(nmcli -g connection.uuid connection show "$ACTIVE_CONN")
KEYFILE="$(find_keyfile_by_uuid "$UUID" || true)"
if [[ -n "$KEYFILE" ]]; then
  cp "$KEYFILE" "$BACKUP"
  chmod 600 "$BACKUP"
  log "Backup: $BACKUP"
else
  BACKUP=""
  log "Warning: could not find keyfile for '$ACTIVE_CONN' (uuid=$UUID); proceeding without keyfile backup"
fi

schedule_restore "$BACKUP" "$ACTIVE_CONN" "$DELAY_MIN" "$ORIG_AUTOCONNECT" "$ORIG_DEVICE_AUTOCONNECT" "$IFACE" "$BAD_NAME"

SSID=$(nmcli -g 802-11-wireless.ssid connection show "$ACTIVE_CONN")
nmcli connection delete "$BAD_NAME" >/dev/null 2>&1 || true
nmcli connection add type wifi ifname "$IFACE" con-name "$BAD_NAME" ssid "$SSID" >/dev/null
nmcli connection modify "$BAD_NAME" wifi-sec.key-mgmt wpa-psk
nmcli connection modify "$BAD_NAME" wifi-sec.psk "$BAD_PSK"
nmcli connection modify "$BAD_NAME" connection.autoconnect no
log "Created '$BAD_NAME' for SSID '$SSID' with bad PSK"

nmcli connection modify "$ACTIVE_CONN" connection.autoconnect no || true
nmcli device set "$IFACE" autoconnect no || true
log "Temporarily disabled autoconnect on '$ACTIVE_CONN' to force offline transition"

nmcli connection down "$ACTIVE_CONN" || true
nmcli device disconnect "$IFACE" >/dev/null 2>&1 || true
nmcli connection up "$BAD_NAME" ifname "$IFACE" || true

ACTIVE_AFTER=$(nmcli -g GENERAL.CONNECTION device show "$IFACE" 2>/dev/null | head -n1 | tr -d '\r')
if [[ "$ACTIVE_AFTER" == "$ACTIVE_CONN" ]]; then
  log "Warning: interface remained on '$ACTIVE_CONN'; forcing disconnect"
  nmcli device disconnect "$IFACE" >/dev/null 2>&1 || true
fi

STATE_AFTER=$(nmcli -t -f DEVICE,STATE device status | awk -F: -v i="$IFACE" '$1==i {print $2; exit}')
log "Injected bad credential profile '$BAD_NAME'; interface state now '${STATE_AFTER:-unknown}'."
if [[ -n "$SCHEDULED_UNIT" ]]; then
  log "Restore timer logs: sudo journalctl -u $SCHEDULED_UNIT"
fi
log "STATUS: fault-injected"
log "Logs: sudo journalctl -fu photoframe-wifi-manager.service    and    sudo journalctl -fu NetworkManager"
log "Restore: nmcli connection delete \"$BAD_NAME\"; nmcli connection import type wifi file $BACKUP; nmcli connection up \"$ACTIVE_CONN\""
