#!/usr/bin/env bash
set -Eeuo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$REPO_ROOT/tests/lib/assert.sh"

main() {
  local photo_service="${PHOTO_SERVICE:-photo-frame.service}"
  local wifi_service="${WIFI_SERVICE:-wifi-manager.service}"

  section "Service health"
  require_cmd systemctl
  run_cmd "${photo_service} is active" systemctl is-active --quiet "${photo_service}"
  run_cmd "Process present" pidof rust-photo-frame
  if systemctl cat "${wifi_service}" >/dev/null 2>&1; then
    run_cmd "${wifi_service} is active" systemctl is-active --quiet "${wifi_service}"
  else
    warn "${wifi_service} not installed on this system"
  fi

  section "Slideshow status"
  info "Visually confirm that images are advancing and no overlays report errors."
  confirm "Is the slideshow advancing normally?"

  section "Network"
  if command -v nmcli >/dev/null 2>&1; then
    run_cmd "List device status" nmcli dev status
    run_cmd "Show active connections" nmcli connection show --active
  else
    warn "nmcli not available; skipping network detail"
  fi

  if [ -x /opt/photo-frame/bin/print-status.sh ]; then
    section "Fleet status snapshot"
    run_cmd "print-status" /opt/photo-frame/bin/print-status.sh
  fi

  section "Sleep schedule"
  info "Check upcoming sleep/dim schedule hints from service logs."
  log_cmd journalctl -u "${photo_service}" -n 50 --no-pager
  journalctl -u "${photo_service}" -n 50 --no-pager || warn "Unable to read journal"
  confirm "Do the logs show the expected upcoming sleep window?"

  section "Log tail"
  log_cmd journalctl -u "${photo_service}" -n 30 --no-pager
  journalctl -u "${photo_service}" -n 30 --no-pager || warn "Unable to read journal"
  confirm "Is the recent service log tail clean (no errors/warnings of concern)?"

  pass "Daily checks complete"
}

main "$@"
