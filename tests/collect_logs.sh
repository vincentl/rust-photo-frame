#!/usr/bin/env bash
set -Eeuo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$REPO_ROOT/tests/lib/assert.sh"

main() {
  section "Collecting diagnostics"
  require_cmd tar
  mkdir -p "$REPO_ROOT/artifacts"

  local timestamp bundle tmpdir
  timestamp="$(date -u +"%Y%m%dT%H%M%SZ")"
  bundle="$REPO_ROOT/artifacts/FRAME-logs-$timestamp.tar.gz"
  tmpdir="$(mktemp -d)"
  trap 'rm -rf "$tmpdir"' EXIT

  mkdir -p "$tmpdir/system" "$tmpdir/journal" "$tmpdir/services" "$tmpdir/network" "$tmpdir/display" "$tmpdir/runtime"

  info "Gathering system metadata"
  uname -a >"$tmpdir/system/uname.txt"
  if [ -r /etc/os-release ]; then
    cp /etc/os-release "$tmpdir/system/os-release"
  fi

  info "Collecting journals"
  if command -v journalctl >/dev/null 2>&1; then
    journalctl -b -n 2000 >"$tmpdir/journal/journalctl-b.txt" || warn "journalctl -b truncated"
    journalctl -u photo-app.service -b >"$tmpdir/journal/photo-app-service.txt" || warn "Unable to read unit journal"
  else
    warn "journalctl unavailable"
  fi

  info "Recording service definitions"
  if command -v systemctl >/dev/null 2>&1; then
    systemctl status photo-app.service >"$tmpdir/services/photo-app-status.txt" || true
    systemctl cat photo-app.service >"$tmpdir/services/photo-app.service" || warn "Unable to cat service"
  fi

  info "Capturing network state"
  if command -v nmcli >/dev/null 2>&1; then
    nmcli dev status >"$tmpdir/network/dev-status.txt" || true
    nmcli connection show --active >"$tmpdir/network/active-connections.txt" || true
  else
    warn "nmcli unavailable"
  fi

  info "Saving display modes"
  for mode in /sys/class/drm/card*-*/modes; do
    [ -e "$mode" ] || continue
    cp "$mode" "$tmpdir/display/${mode##*/}.txt"
  done
  if command -v edid-decode >/dev/null 2>&1; then
    for edid in /sys/class/drm/card*-*/edid; do
      [ -e "$edid" ] || continue
      edid-decode "$edid" >"$tmpdir/display/$(basename "${edid%/edid}")-edid.txt" || warn "Failed to decode $edid"
    done
  else
    for edid in /sys/class/drm/card*-*/edid; do
      [ -e "$edid" ] || continue
      hexdump -C "$edid" >"$tmpdir/display/$(basename "${edid%/edid}")-edid.hex"
    done
  fi

  info "Recording runtime metrics"
  ps -eo pid,comm,pcpu,pmem --sort=-pcpu | head -20 >"$tmpdir/runtime/top20.txt"
  if command -v vcgencmd >/dev/null 2>&1; then
    vcgencmd measure_temp >"$tmpdir/runtime/temperature.txt" || true
  fi

  if [ -x "$REPO_ROOT/target/release/rust-photo-frame" ]; then
    "$REPO_ROOT/target/release/rust-photo-frame" --version >"$tmpdir/runtime/app-version.txt" || true
  elif [ -x "$REPO_ROOT/target/debug/rust-photo-frame" ]; then
    "$REPO_ROOT/target/debug/rust-photo-frame" --version >"$tmpdir/runtime/app-version.txt" || true
  else
    warn "rust-photo-frame binary not found; skipping --version"
  fi

  info "Copying config"
  if [ -f "$REPO_ROOT/config.yaml" ]; then
    cp "$REPO_ROOT/config.yaml" "$tmpdir/runtime/config.yaml"
  fi

  tar -czf "$bundle" -C "$tmpdir" .
  pass "Created $bundle"
}

main "$@"
