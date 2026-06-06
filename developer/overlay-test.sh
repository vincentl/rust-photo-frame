#!/usr/bin/env bash
#
# Overlay takeover smoke-test for Sway
# - Launches the Wi‑Fi overlay in the active kiosk Sway session
# - Prepares realistic overlay assets (password + QR) from wifi-manager config
# - Verifies it appears (focused + fullscreen) via Sway IPC
# - Optional: hide/kill the overlay
#
# Usage:
#   sudo bash developer/overlay-test.sh            # show overlay
#   sudo bash developer/overlay-test.sh hide       # hide overlay
#   sudo bash developer/overlay-test.sh status     # check presence
#
set -euo pipefail

SSID_DEFAULT=${SSID_DEFAULT:-Test-Overlay}
CONFIG_PATH_DEFAULT=${CONFIG_PATH_DEFAULT:-/opt/photoframe/etc/wifi-manager.yaml}
PASSWORD_WORD_COUNT=${PASSWORD_WORD_COUNT:-3}
GENERATE_PASSWORD=${GENERATE_PASSWORD:-1}
GENERATE_QR=${GENERATE_QR:-1}

log() { printf '[overlay-test] %s\n' "$*"; }
err() { printf '[overlay-test] ERROR: %s\n' "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || err "missing command: $1"; }

strip_yaml_value() {
  sed -e 's/[[:space:]]+#.*$//' \
      -e 's/^[[:space:]]*//' \
      -e 's/[[:space:]]*$//' \
      -e 's/^"//' \
      -e 's/"$//'
}

read_yaml_scalar() {
  local key="$1"
  local file="$2"
  awk -v key="${key}" '
    /^[[:space:]]*#/ { next }
    $0 ~ "^[[:space:]]*" key ":[[:space:]]*" {
      value = $0
      sub(/^[[:space:]]*[^:]+:[[:space:]]*/, "", value)
      print value
      exit
    }
  ' "${file}" | strip_yaml_value
}

read_yaml_nested() {
  local section="$1"
  local key="$2"
  local file="$3"
  awk -v section="${section}" -v key="${key}" '
    /^[[:space:]]*#/ { next }
    $0 ~ "^[[:space:]]*" section ":[[:space:]]*$" { in_section = 1; next }
    in_section == 1 {
      if ($0 ~ "^[^[:space:]]") { in_section = 0; next }
      if ($0 ~ "^[[:space:]]+" key ":[[:space:]]*") {
        value = $0
        sub(/^[[:space:]]*[^:]+:[[:space:]]*/, "", value)
        print value
        exit
      }
    }
  ' "${file}" | strip_yaml_value
}

generate_password_from_wordlist() {
  local wordlist_path="$1"
  local count="$2"
  [[ -r "${wordlist_path}" ]] || err "wordlist not readable: ${wordlist_path}"
  local words
  words="$(grep -Ev '^[[:space:]]*(#|$)' "${wordlist_path}" || true)"
  [[ -n "${words}" ]] || err "wordlist has no usable entries: ${wordlist_path}"
  local password=""
  local i word
  for ((i = 0; i < count; i++)); do
    word="$(printf '%s\n' "${words}" | shuf -n1)"
    [[ -n "${word}" ]] || err "failed to choose random word from ${wordlist_path}"
    if [[ -z "${password}" ]]; then
      password="${word}"
    else
      password="${password}-${word}"
    fi
  done
  printf '%s\n' "${password}"
}

main() {
  local action="${1:-show}"
  need id
  need ls
  need grep
  need awk
  need ps
  need shuf
  need timeout || true

  # Resolve kiosk user + Sway IPC
  local kiosk_uid current_uid
  kiosk_uid=$(id -u kiosk 2>/dev/null || true)
  current_uid=$(id -u)
  [[ -n "$kiosk_uid" ]] || err "user 'kiosk' not found"

  # Helper to run swaymsg as kiosk with proper env
  run_as_kiosk() {
    if [[ "$current_uid" == "$kiosk_uid" ]]; then
      env "$@"
    else
      sudo -u kiosk env "$@"
    fi
  }

  run_sway() {
    run_as_kiosk XDG_RUNTIME_DIR="/run/user/${kiosk_uid}" SWAYSOCK="${sway_sock}" swaymsg "$@"
  }

  select_sway_socket() {
    local runtime_dir="/run/user/${kiosk_uid}"
    local candidate
    for candidate in "${runtime_dir}"/sway-ipc.*.sock; do
      [[ -S "${candidate}" ]] || continue
      if run_as_kiosk XDG_RUNTIME_DIR="${runtime_dir}" SWAYSOCK="${candidate}" \
        swaymsg -s "${candidate}" -t get_version >/dev/null 2>&1; then
        printf '%s\n' "${candidate}"
        return 0
      fi
    done
    return 1
  }

  local sway_sock
  sway_sock=$(select_sway_socket || true)
  [[ -n "${sway_sock:-}" ]] || err "failed to locate a live Sway IPC socket for kiosk (is greetd/sway running?)"

  case "$action" in
    hide)
      log "Hiding overlay (if present)"
      run_sway '[app_id="wifi-overlay"] kill' || true
      exit 0
      ;;
    status)
      if run_sway -t get_tree | grep -qi '"app_id"\s*:\s*"wifi-overlay"'; then
        log "Overlay present"
        exit 0
      else
        log "Overlay not present"
        exit 1
      fi
      ;;
    show)
      ;;
    *)
      err "unknown action: $action (expected: show|hide|status)"
      ;;
  esac

  # Inputs (safe defaults; override via env)
  local config_path ssid ui_url pass_file
  local hotspot_ip ui_port var_dir wordlist_path configured_ssid
  config_path="${WIFI_MANAGER_CONFIG:-$CONFIG_PATH_DEFAULT}"
  hotspot_ip="192.168.4.1"
  ui_port="8080"
  var_dir="/var/lib/photoframe"
  wordlist_path="/opt/photoframe/share/wordlist.txt"
  configured_ssid="${SSID_DEFAULT}"

  if [[ -f "${config_path}" ]]; then
    hotspot_ip="$(read_yaml_nested "hotspot" "ipv4-addr" "${config_path}" || true)"
    hotspot_ip="${hotspot_ip:-192.168.4.1}"
    ui_port="$(read_yaml_nested "ui" "port" "${config_path}" || true)"
    ui_port="${ui_port:-8080}"
    var_dir="$(read_yaml_scalar "var-dir" "${config_path}" || true)"
    var_dir="${var_dir:-/var/lib/photoframe}"
    wordlist_path="$(read_yaml_scalar "wordlist-path" "${config_path}" || true)"
    wordlist_path="${wordlist_path:-/opt/photoframe/share/wordlist.txt}"
    configured_ssid="$(read_yaml_nested "hotspot" "ssid" "${config_path}" || true)"
    configured_ssid="${configured_ssid:-$SSID_DEFAULT}"
  fi

  ssid="${SSID:-${configured_ssid}}"
  ui_url="${UI_URL:-http://${hotspot_ip}:${ui_port}/}"
  pass_file="${PASS_FILE:-${var_dir}/hotspot-password.txt}"

  # Ensure the overlay binary exists
  local bin=/opt/photoframe/bin/wifi-manager
  [[ -x "$bin" ]] || err "wifi-manager not found at ${bin} (deploy application stage first)"

  # Prepare a realistic hotspot password from the configured wordlist.
  if [[ "${GENERATE_PASSWORD}" == "1" ]]; then
    local generated_password
    generated_password="$(generate_password_from_wordlist "${wordlist_path}" "${PASSWORD_WORD_COUNT}")"
    log "Generated ${PASSWORD_WORD_COUNT}-word test hotspot password from ${wordlist_path}"
    run_as_kiosk PASS_PATH="${pass_file}" PASS_VALUE="${generated_password}" sh -lc '
      umask 077
      install -d -m 750 "$(dirname -- "$PASS_PATH")"
      printf "%s\n" "$PASS_VALUE" > "$PASS_PATH"
    '
  else
    [[ -f "${pass_file}" ]] || err "password file missing and GENERATE_PASSWORD=0: ${pass_file}"
  fi
  if ! run_as_kiosk test -r "${pass_file}"; then
    err "password file is not readable by kiosk: ${pass_file}"
  fi

  # Generate QR asset used by the recovery portal page.
  if [[ "${GENERATE_QR}" == "1" ]]; then
    if [[ -f "${config_path}" ]]; then
      log "Generating QR asset using ${config_path}"
      run_as_kiosk "${bin}" qr --config "${config_path}" >/dev/null
      log "QR asset refreshed at ${var_dir}/wifi-qr.png"
    else
      log "Skipping QR generation: config not found at ${config_path}"
    fi
  fi

  log "Note: overlay-test does not activate hotspot or change NetworkManager state"

  # Determine Wayland display socket under XDG_RUNTIME_DIR
  local wl_sock wl_display
  wl_sock=$(ls "/run/user/${kiosk_uid}"/wayland-* 2>/dev/null | head -n1 || true)
  if [[ -S "${wl_sock:-/nonexistent}" ]]; then
    wl_display="$(basename "$wl_sock")"
  else
    # Fallback to wayland-0; winit will error if unavailable
    wl_display="wayland-0"
  fi

  # Launch overlay directly with proper Wayland + IPC env
  log "Launching overlay (direct) for SSID='${ssid}' (UI: ${ui_url}); WAYLAND_DISPLAY=${wl_display}"
  run_as_kiosk \
    XDG_RUNTIME_DIR="/run/user/${kiosk_uid}" \
    SWAYSOCK="${sway_sock}" \
    WAYLAND_DISPLAY="${wl_display}" \
    systemd-cat -t wifi-overlay -- \
    "$bin" overlay \
      --ssid "$ssid" \
      --password-file "$pass_file" \
      --ui-url "$ui_url" \
      >/dev/null 2>&1 &

  # Give Sway a moment to map the window, try to focus/fullscreen it, then verify presence
  sleep 0.6
  run_sway '[app_id="wifi-overlay"] focus, fullscreen enable' || true
  sleep 0.2
  if run_sway -t get_tree | grep -qi '"app_id"\s*:\s*"wifi-overlay"'; then
    log "Overlay is visible and mapped (app_id=wifi-overlay)"
    exit 0
  fi

  # Retry once after a short delay
  sleep 1
  run_sway '[app_id="wifi-overlay"] focus, fullscreen enable' || true
  if run_sway -t get_tree | grep -qi '"app_id"\s*:\s*"wifi-overlay"'; then
    log "Overlay is visible after retry"
    exit 0
  else
    err "failed to detect overlay window; check Sway logs and SWAYSOCK; view logs with: journalctl -t wifi-overlay -n 100 --no-pager"
  fi
}

main "$@"
