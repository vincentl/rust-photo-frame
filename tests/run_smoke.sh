#!/usr/bin/env bash
set -Eeuo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$REPO_ROOT/tests/lib/assert.sh"

main() {
  local photo_service="${PHOTO_SERVICE:-greetd.service}"
  local wifi_service="${WIFI_SERVICE:-photoframe-wifi-manager.service}"

  section "Pre-flight"
  require_cmd vcgencmd
  run_cmd "Measure SoC temperature" vcgencmd measure_temp
  if command -v sensors >/dev/null 2>&1; then
    info "Optional: run 'sensors' if additional thermal data needed"
  fi

  section "Service health"
  require_cmd systemctl
  run_cmd "${photo_service} is active" systemctl is-active --quiet "${photo_service}"
  run_cmd "${photo_service} enabled" systemctl is-enabled --quiet "${photo_service}"
  run_cmd "Process present" pidof photoframe
  if systemctl cat "${wifi_service}" >/dev/null 2>&1; then
    run_cmd "${wifi_service} is active" systemctl is-active --quiet "${wifi_service}"
  else
    warn "${wifi_service} not installed on this system"
  fi
  info "Recent service log tail"
  log_cmd journalctl -u "${photo_service}" -n 20 --no-pager
  journalctl -u "${photo_service}" -n 20 --no-pager || warn "Unable to read journal"

  section "Display mode"
  # wlr-randr needs the kiosk session's Wayland socket, and this suite runs as
  # the operator account (no Wayland display of its own). Query DRM sysfs
  # instead — always readable, no compositor connection required. Confirm the
  # live compositor mode separately with `sudo -u kiosk wlr-randr` if needed.
  local mode found_modes=0
  for mode in /sys/class/drm/card*-*/modes; do
    { [ -e "$mode" ] && [ -s "$mode" ]; } || continue
    info "Modes for ${mode%/modes}"
    log_cmd cat "$mode"
    cat "$mode"
    found_modes=1
  done
  if [ "$found_modes" -eq 1 ]; then
    pass "DRM connector reports modes (confirm 4K@60 on screen)"
  else
    warn "No DRM connector modes found"
  fi

  section "Button: sleep and wake (round trip)"
  info "Confirm the frame is awake (slideshow showing), then test BOTH directions of the power button."
  info "Step 1: short-press the power button once — the screen should go to SLEEP (the panel powers off after the configured delay)."
  if confirm_or_skip "Did the short press put the frame to SLEEP?"; then
    info "Step 2: short-press the power button again — the screen should WAKE and the slideshow should resume."
    confirm_or_skip "Did the next short press WAKE the frame?" || true
    info "Capture recent button + photo journal for evidence"
    journalctl -u buttond.service -n 30 --no-pager || warn "Unable to read buttond journal"
    journalctl -t photoframe -n 20 --no-pager || warn "Unable to read photoframe journal"
  fi

  section "Sleep toggle via control socket"
  info "Sending toggle-state command via /run/photoframe/control.sock"
  run_cmd "Send toggle-state" python3 - <<'PY'
import socket

payload = b'{"command":"toggle-state"}'
sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
try:
    sock.connect("/run/photoframe/control.sock")
    sock.sendall(payload)
finally:
    sock.close()
PY
  sleep 2
  info "Check journal for sleep toggle acknowledgement"
  journalctl -t photoframe -n 20 --no-pager || warn "Unable to read photoframe journal"

  section "Collect log bundle"
  run_cmd "Run log collector" "$REPO_ROOT/tests/collect_logs.sh"

  pass "Smoke suite complete"
}

main "$@"
