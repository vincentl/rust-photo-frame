#!/usr/bin/env bash
set -Eeuo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$REPO_ROOT/tests/lib/assert.sh"

main() {
  local wifi_service="${WIFI_SERVICE:-photoframe-wifi-manager.service}"
  local photo_service="${PHOTO_SERVICE:-greetd.service}"
  local wifi_interface="${WIFI_INTERFACE:-wlan0}"
  local hotspot_id="${HOTSPOT_ID:-pf-hotspot}"
  local hotspot_ssid="${HOTSPOT_SSID:-PhotoFrame-Setup}"
  local wait_hotspot_sec="${WAIT_HOTSPOT_SEC:-150}"
  local wait_online_sec="${WAIT_ONLINE_SEC:-240}"
  local start_iso

  section "Pre-flight"
  require_cmd systemctl
  require_cmd journalctl
  require_cmd nmcli
  require_cmd sudo

  run_cmd "${photo_service} is active" systemctl is-active --quiet "${photo_service}"
  run_cmd "${wifi_service} is active" systemctl is-active --quiet "${wifi_service}"

  if [[ -x /opt/photo-frame/bin/print-status.sh ]]; then
    run_cmd "Initial status snapshot" /opt/photo-frame/bin/print-status.sh
  else
    warn "/opt/photo-frame/bin/print-status.sh not found; skipping snapshot"
  fi

  if [[ ! -x "$REPO_ROOT/developer/suspend-wifi.sh" ]]; then
    fail "Missing helper script: $REPO_ROOT/developer/suspend-wifi.sh"
  fi

  start_iso="$(date --iso-8601=seconds)"
  info "Starting Wi-Fi recovery test from marker: ${start_iso}"

  section "Trigger recovery"
  run_cmd \
    "Inject wrong PSK on ${wifi_interface}" \
    sudo bash -lc "nohup bash '$REPO_ROOT/developer/suspend-wifi.sh' '${wifi_interface}' >/tmp/wifi-recovery-test.log 2>&1 &"
  info "Helper output: /tmp/wifi-recovery-test.log"

  section "Wait for hotspot transition"
  if wait_for_journal_pattern "$wifi_service" "$start_iso" 'to=RecoveryHotspotActive' "$wait_hotspot_sec"; then
    pass "Watcher reached RecoveryHotspotActive"
  else
    fail "Watcher did not reach RecoveryHotspotActive within ${wait_hotspot_sec}s"
  fi

  run_cmd "Hotspot is active (${hotspot_id})" bash -lc "nmcli -t -f NAME connection show --active | grep -Fx '${hotspot_id}'"

  section "Operator action"
  info "Join hotspot SSID '${hotspot_ssid}' from a phone/laptop, open the QR/setup URL, and submit valid home Wi-Fi credentials."
  confirm "Did the portal accept credentials submission?"

  section "Wait for online transition"
  if wait_for_journal_pattern "$wifi_service" "$start_iso" 'reason=(provision-success|probe-success|link-restored)' "$wait_online_sec"; then
    pass "Watcher reported successful recovery reason"
  else
    fail "Watcher did not report a successful recovery reason within ${wait_online_sec}s"
  fi

  run_cmd "Interface is back on infrastructure Wi-Fi" bash -lc "nmcli -t -f DEVICE,STATE device status | grep -E '^${wifi_interface}:(connected|activated|full)$'"
  run_cmd "Hotspot is no longer active" bash -lc "! nmcli -t -f NAME connection show --active | grep -Fx '${hotspot_id}'"

  if [[ -f /var/lib/photo-frame/wifi-request.json ]]; then
    fail "Request file still present: /var/lib/photo-frame/wifi-request.json"
  else
    pass "No pending wifi-request.json"
  fi

  if [[ -x /opt/photo-frame/bin/print-status.sh ]]; then
    run_cmd "Final status snapshot" /opt/photo-frame/bin/print-status.sh
  fi

  section "Logs"
  log_cmd sudo journalctl -u "$wifi_service" --since "$start_iso" --no-pager
  sudo journalctl -u "$wifi_service" --since "$start_iso" --no-pager || true

  pass "Wi-Fi recovery acceptance completed"
}

wait_for_journal_pattern() {
  local service="$1"
  local since="$2"
  local regex="$3"
  local timeout="${4:-180}"
  local deadline

  deadline=$((SECONDS + timeout))
  while (( SECONDS < deadline )); do
    if sudo journalctl -u "$service" --since "$since" --no-pager 2>/dev/null | grep -E "$regex" >/dev/null; then
      return 0
    fi
    sleep 2
  done
  return 1
}

main "$@"
