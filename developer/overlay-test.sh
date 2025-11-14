#!/usr/bin/env bash
#
# Overlay takeover smoke-test for Sway
# - Launches the Wi‑Fi overlay in the active kiosk Sway session
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
UI_URL_DEFAULT=${UI_URL_DEFAULT:-http://127.0.0.1:8080/}
PASS_FILE_DEFAULT=${PASS_FILE_DEFAULT:-/var/tmp/overlay-test-password.txt}

log() { printf '[overlay-test] %s\n' "$*"; }
err() { printf '[overlay-test] ERROR: %s\n' "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || err "missing command: $1"; }

main() {
  local action="${1:-show}"
  need id
  need ls
  need grep
  need awk
  need ps
  need timeout || true

  # Resolve kiosk user + Sway IPC
  local kiosk_uid current_uid
  kiosk_uid=$(id -u kiosk 2>/dev/null || true)
  current_uid=$(id -u)
  [[ -n "$kiosk_uid" ]] || err "user 'kiosk' not found"

  local sway_sock
  sway_sock=$(ls /run/user/${kiosk_uid}/sway-ipc.*.sock 2>/dev/null | head -n1 || true)
  [[ -S "${sway_sock:-/nonexistent}" ]] || err "failed to locate Sway IPC socket for kiosk (is greetd/sway running?)"

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
  local ssid ui_url pass_file
  ssid="${SSID:-$SSID_DEFAULT}"
  ui_url="${UI_URL:-$UI_URL_DEFAULT}"
  pass_file="${PASS_FILE:-$PASS_FILE_DEFAULT}"

  # Ensure a password file exists (do not overwrite hotspot password)
  if [[ ! -f "$pass_file" ]]; then
    log "Creating test password file at ${pass_file}"
    echo 'Test-Overlay-1234' > "$pass_file"
    chmod 600 "$pass_file" || true
  fi

  # Ensure the overlay binary exists
  local bin=/opt/photo-frame/bin/wifi-manager
  [[ -x "$bin" ]] || err "wifi-manager not found at ${bin} (deploy application stage first)"

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
    WINIT_APP_ID="wifi-overlay" \
    systemd-cat -t wifi-overlay -- \
    "$bin" overlay \
      --ssid "$ssid" \
      --password-file "$pass_file" \
      --ui-url "$ui_url" \
      >/dev/null 2>&1 &

  # Give Sway a moment to map the window, then verify presence
  sleep 0.8
  if run_sway -t get_tree | grep -qi '"app_id"\s*:\s*"wifi-overlay"'; then
    log "Overlay is visible and mapped (app_id=wifi-overlay)"
    exit 0
  fi

  # Retry once after a short delay
  sleep 1
  if run_sway -t get_tree | grep -qi '"app_id"\s*:\s*"wifi-overlay"'; then
    log "Overlay is visible after retry"
    exit 0
  else
    err "failed to detect overlay window; check Sway logs and SWAYSOCK; view logs with: journalctl -t wifi-overlay -n 100 --no-pager"
  fi
}

main "$@"
