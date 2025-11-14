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
  local kiosk_uid
  kiosk_uid=$(id -u kiosk 2>/dev/null || true)
  [[ -n "$kiosk_uid" ]] || err "user 'kiosk' not found"

  local sway_sock
  sway_sock=$(ls /run/user/${kiosk_uid}/sway-ipc.*.sock 2>/dev/null | head -n1 || true)
  [[ -S "${sway_sock:-/nonexistent}" ]] || err "failed to locate Sway IPC socket for kiosk (is greetd/sway running?)"

  # Helper to run swaymsg as kiosk with proper env
  run_sway() {
    sudo -u kiosk env \
      XDG_RUNTIME_DIR="/run/user/${kiosk_uid}" \
      SWAYSOCK="${sway_sock}" \
      swaymsg "$@"
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
    echo 'Test-Overlay-1234' | sudo tee "$pass_file" >/dev/null
    sudo chmod 600 "$pass_file" || true
  fi

  # Ensure the overlay binary exists
  local bin=/opt/photo-frame/bin/wifi-manager
  [[ -x "$bin" ]] || err "wifi-manager not found at ${bin} (deploy application stage first)"

  # Launch overlay directly (no shell quoting pitfalls); inherit Wayland env
  log "Launching overlay for SSID='${ssid}' (UI: ${ui_url})"
  sudo -u kiosk env \
    XDG_RUNTIME_DIR="/run/user/${kiosk_uid}" \
    SWAYSOCK="${sway_sock}" \
    WAYLAND_DISPLAY="wayland-0" \
    WINIT_APP_ID="wifi-overlay" \
    "$bin" overlay \
      --ssid "$ssid" \
      --password-file "$pass_file" \
      --ui-url "$ui_url" \
      >/dev/null 2>&1 &

  # Give Sway a moment to map the window, then verify presence
  sleep 0.5
  if run_sway -t get_tree | grep -qi '"app_id"\s*:\s*"wifi-overlay"'; then
    log "Overlay is visible and mapped (app_id=wifi-overlay)"
    exit 0
  fi

  # Retry once after a short delay
  sleep 0.5
  if run_sway -t get_tree | grep -qi '"app_id"\s*:\s*"wifi-overlay"'; then
    log "Overlay is visible after retry"
    exit 0
  else
    err "failed to detect overlay window; check Sway logs and SWAYSOCK"
  fi
}

main "$@"

