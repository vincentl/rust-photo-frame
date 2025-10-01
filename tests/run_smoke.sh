#!/usr/bin/env bash
set -Eeuo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$REPO_ROOT/tests/lib/assert.sh"

main() {
  section "Pre-flight"
  require_cmd vcgencmd
  run_cmd "Measure SoC temperature" vcgencmd measure_temp
  if command -v sensors >/dev/null 2>&1; then
    info "Optional: run 'sensors' if additional thermal data needed"
  fi

  section "Service health"
  require_cmd systemctl
  run_cmd "photo-app.service is active" systemctl is-active --quiet photo-app.service
  run_cmd "photo-app.service enabled" systemctl is-enabled --quiet photo-app.service
  run_cmd "Process present" pidof rust-photo-frame
  info "Recent service log tail"
  log_cmd journalctl -u photo-app.service -n 20 --no-pager
  journalctl -u photo-app.service -n 20 --no-pager || warn "Unable to read journal"

  section "Display mode"
  if command -v wlr-randr >/dev/null 2>&1; then
    run_cmd "Query Wayland outputs" wlr-randr
  else
    warn "wlr-randr not found; falling back to DRM sysfs"
    for mode in /sys/class/drm/card*-*/modes; do
      [ -e "$mode" ] || continue
      info "Modes for ${mode%/modes}"
      log_cmd cat "$mode"
      cat "$mode"
    done
  fi

  section "Button short press"
  info "Prepare to short-press the physical power button. Watch the screen for sleep toggle and confirm below."
  if confirm_or_skip "Did the short press toggle slideshow sleep?"; then
    info "Capture recent journal entries for evidence"
    journalctl -u photo-app.service -n 40 --no-pager || warn "Unable to read journal"
  fi

  section "Sleep toggle via signal"
  local pid
  pid=$(pidof rust-photo-frame)
  info "Sending SIGUSR1 to PID $pid"
  run_cmd "Send SIGUSR1" kill -USR1 "$pid"
  sleep 2
  info "Check journal for sleep toggle acknowledgement"
  journalctl -u photo-app.service -n 40 --no-pager || warn "Unable to read journal"

  section "Collect log bundle"
  run_cmd "Run log collector" "$REPO_ROOT/tests/collect_logs.sh"

  pass "Smoke suite complete"
}

main "$@"
