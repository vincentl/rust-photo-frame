#!/usr/bin/env bash
set -Eeuo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$REPO_ROOT/tests/lib/assert.sh"

parse_wifi_interface_from_config() {
  local config_path="${WIFI_CONFIG_PATH:-/opt/photoframe/etc/wifi-manager.yaml}"
  if [[ ! -f "${config_path}" ]]; then
    return 0
  fi
  awk -F: '
    /^[[:space:]]*interface:[[:space:]]*/ {
      value=$2
      sub(/^[[:space:]]+/, "", value)
      sub(/[[:space:]]+#.*/, "", value)
      gsub(/["[:space:]]/, "", value)
      print value
      exit
    }
  ' "${config_path}" 2>/dev/null || true
}

ssh_route_interface() {
  local ssh_client_ip=""
  local route_line=""
  if [[ -z "${SSH_CONNECTION:-}" ]]; then
    return 1
  fi
  if ! command -v ip >/dev/null 2>&1; then
    return 1
  fi

  ssh_client_ip="$(awk '{print $1}' <<< "${SSH_CONNECTION}" 2>/dev/null || true)"
  if [[ -z "${ssh_client_ip}" ]]; then
    return 1
  fi

  route_line="$(ip -o route get "${ssh_client_ip}" 2>/dev/null | head -n1 || true)"
  if [[ -z "${route_line}" ]]; then
    return 1
  fi

  awk '{
    for (i = 1; i <= NF; i++) {
      if ($i == "dev" && (i + 1) <= NF) {
        print $(i + 1)
        exit
      }
    }
  }' <<< "${route_line}"
}

dump_recovery_debug() {
  local wifi_service="$1"
  local start_iso="$2"

  section "Recovery debug snapshot"
  log_cmd nmcli -t -f DEVICE,TYPE,STATE device status
  nmcli -t -f DEVICE,TYPE,STATE device status || true
  log_cmd nmcli connection show --active
  nmcli connection show --active || true

  if [[ -f /var/lib/photoframe/wifi-state.json ]]; then
    log_cmd sudo cat /var/lib/photoframe/wifi-state.json
    sudo cat /var/lib/photoframe/wifi-state.json || true
  fi
  if [[ -f /var/lib/photoframe/wifi-last.json ]]; then
    log_cmd sudo cat /var/lib/photoframe/wifi-last.json
    sudo cat /var/lib/photoframe/wifi-last.json || true
  fi

  log_cmd sudo journalctl -u "$wifi_service" --since "$start_iso" -n 200 --no-pager
  sudo journalctl -u "$wifi_service" --since "$start_iso" -n 200 --no-pager || true
}

wait_for_fault_injection() {
  local helper_log="$1"
  local timeout="${2:-45}"
  local deadline

  deadline=$((SECONDS + timeout))
  while (( SECONDS < deadline )); do
    if [[ -f "${helper_log}" ]]; then
      if grep -Eq 'STATUS: fault-injected' "${helper_log}"; then
        return 0
      fi
      if grep -Eq 'systemd-run is required|This script must run as root|No active Wi-Fi on' "${helper_log}"; then
        return 2
      fi
    fi
    sleep 1
  done

  return 1
}

wait_for_hotspot_transition() {
  local service="$1"
  local since="$2"
  local timeout="${3:-180}"
  local deadline
  local state=""

  deadline=$((SECONDS + timeout))
  while (( SECONDS < deadline )); do
    if sudo journalctl -u "$service" --since "$since" --no-pager 2>/dev/null | grep -E 'to=RecoveryHotspotActive' >/dev/null; then
      return 0
    fi

    if [[ -f /var/lib/photoframe/wifi-state.json ]]; then
      if command -v jq >/dev/null 2>&1; then
        state="$(jq -r '.state // ""' /var/lib/photoframe/wifi-state.json 2>/dev/null || true)"
      else
        state="$(sed -n 's/.*"state"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' /var/lib/photoframe/wifi-state.json | head -n1)"
      fi
      if [[ "${state:-}" == "RecoveryHotspotActive" ]]; then
        return 0
      fi
    fi

    sleep 2
  done

  return 1
}

main() {
  local wifi_service="${WIFI_SERVICE:-photoframe-wifi-manager.service}"
  local photo_service="${PHOTO_SERVICE:-greetd.service}"
  local wifi_interface="${WIFI_INTERFACE:-}"
  local hotspot_id="${HOTSPOT_ID:-pf-hotspot}"
  local hotspot_ssid="${HOTSPOT_SSID:-PhotoFrame-Setup}"
  local wait_hotspot_sec="${WAIT_HOTSPOT_SEC:-150}"
  local wait_online_sec="${WAIT_ONLINE_SEC:-240}"
  local config_interface=""
  local active_connection=""
  local device_state=""
  local ssh_iface=""
  local start_iso
  local helper_log="/tmp/wifi-recovery-test.log"

  section "Pre-flight"
  require_cmd systemctl
  require_cmd journalctl
  require_cmd nmcli
  require_cmd sudo

  run_cmd "${photo_service} is active" systemctl is-active --quiet "${photo_service}"
  run_cmd "${wifi_service} is active" systemctl is-active --quiet "${wifi_service}"

  config_interface="$(parse_wifi_interface_from_config)"
  if [[ -z "${wifi_interface}" ]]; then
    wifi_interface="${config_interface:-wlan0}"
  elif [[ -n "${config_interface}" && "${wifi_interface}" != "${config_interface}" && "${ALLOW_INTERFACE_MISMATCH:-0}" != "1" ]]; then
    fail "WIFI_INTERFACE (${wifi_interface}) does not match wifi-manager config interface (${config_interface}). Set ALLOW_INTERFACE_MISMATCH=1 to override."
  fi
  info "Using Wi-Fi interface: ${wifi_interface}"

  ssh_iface="$(ssh_route_interface || true)"
  if [[ -n "${ssh_iface}" && "${ssh_iface}" == "${wifi_interface}" ]]; then
    if [[ "${ALLOW_WIFI_SSH_DROP:-0}" != "1" ]]; then
      fail "Current SSH session routes over ${wifi_interface}; this test will disconnect SSH. Run from local console/alternate management path, or run inside tmux and rerun with ALLOW_WIFI_SSH_DROP=1."
    fi
    warn "SSH is routed over ${wifi_interface}; disconnect is expected during fault injection."
  fi

  if ! nmcli -t -f DEVICE device status | grep -Fxq "${wifi_interface}"; then
    fail "Interface ${wifi_interface} not found in nmcli output. Confirm /opt/photoframe/etc/wifi-manager.yaml interface value."
  fi

  active_connection="$(nmcli -g GENERAL.CONNECTION device show "${wifi_interface}" 2>/dev/null | head -n1 | tr -d '\r')"
  device_state="$(nmcli -t -f DEVICE,STATE device status | awk -F: -v iface="${wifi_interface}" '$1==iface {print $2; exit}')"

  if [[ -z "${active_connection}" || "${active_connection}" == "--" ]]; then
    fail "No active infrastructure connection on ${wifi_interface}. Connect Wi-Fi first, then rerun this test."
  fi
  if [[ "${active_connection}" == "${hotspot_id}" ]]; then
    fail "Interface ${wifi_interface} is already on hotspot profile ${hotspot_id}. Join your normal Wi-Fi first, then rerun."
  fi
  if ! [[ "${device_state}" =~ ^(connected|activated|full)$ ]]; then
    fail "Interface ${wifi_interface} state is '${device_state:-unknown}', expected connected/activated/full before fault injection."
  fi
  info "Preflight connection on ${wifi_interface}: ${active_connection} (${device_state})"

  if [[ -x /opt/photoframe/bin/print-status.sh ]]; then
    run_cmd "Initial status snapshot" /opt/photoframe/bin/print-status.sh
  else
    warn "/opt/photoframe/bin/print-status.sh not found; skipping snapshot"
  fi

  if [[ ! -x "$REPO_ROOT/developer/suspend-wifi.sh" ]]; then
    fail "Missing helper script: $REPO_ROOT/developer/suspend-wifi.sh"
  fi

  start_iso="$(date --iso-8601=seconds)"
  info "Starting Wi-Fi recovery test from marker: ${start_iso}"

  section "Trigger recovery"
  run_cmd \
    "Inject wrong PSK on ${wifi_interface}" \
    sudo bash -lc "nohup bash '$REPO_ROOT/developer/suspend-wifi.sh' '${wifi_interface}' >'${helper_log}' 2>&1 &"
  info "Helper output: ${helper_log}"

  if wait_for_fault_injection "${helper_log}" 45; then
    pass "Fault injection helper reported success"
  else
    rc=$?
    section "Fault injection helper output"
    if [[ -f "${helper_log}" ]]; then
      log_cmd sudo cat "${helper_log}"
      sudo cat "${helper_log}" || true
    else
      warn "Helper log not found: ${helper_log}"
    fi

    if [[ ${rc} -eq 2 ]]; then
      fail "Fault injection helper reported a fatal setup error"
    fi
    fail "Fault injection helper did not confirm success within 45s"
  fi

  section "Wait for hotspot transition"
  if wait_for_hotspot_transition "$wifi_service" "$start_iso" "$wait_hotspot_sec"; then
    pass "Watcher reached RecoveryHotspotActive"
  else
    dump_recovery_debug "$wifi_service" "$start_iso"
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
    dump_recovery_debug "$wifi_service" "$start_iso"
    fail "Watcher did not report a successful recovery reason within ${wait_online_sec}s"
  fi

  run_cmd "Interface is back on infrastructure Wi-Fi" bash -lc "nmcli -t -f DEVICE,STATE device status | grep -E '^${wifi_interface}:(connected|activated|full)$'"
  run_cmd "Hotspot is no longer active" bash -lc "! nmcli -t -f NAME connection show --active | grep -Fx '${hotspot_id}'"

  if [[ -f /var/lib/photoframe/wifi-request.json ]]; then
    fail "Request file still present: /var/lib/photoframe/wifi-request.json"
  else
    pass "No pending wifi-request.json"
  fi

  if [[ -x /opt/photoframe/bin/print-status.sh ]]; then
    run_cmd "Final status snapshot" /opt/photoframe/bin/print-status.sh
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
