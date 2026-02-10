#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STAMP="$(date +%Y%m%d-%H%M%S)"
OUT_DIR="${1:-${ROOT_DIR}/artifacts/upgrade-baseline-${STAMP}}"

mkdir -p "${OUT_DIR}"

log() {
  printf '[baseline] %s\n' "$*"
}

capture_cmd() {
  local file="$1"
  shift
  if "$@" >"${OUT_DIR}/${file}" 2>&1; then
    return 0
  fi
  printf 'command failed: %s\n' "$*" >"${OUT_DIR}/${file}"
}

log "Writing baseline to ${OUT_DIR}"

capture_cmd os-release.txt cat /etc/os-release
capture_cmd uname.txt uname -a
capture_cmd rustc.txt rustc --version
capture_cmd cargo.txt cargo --version
capture_cmd nmcli.txt nmcli --version
capture_cmd apt-policy-network-manager.txt apt-cache policy network-manager
capture_cmd apt-policy-sway.txt apt-cache policy sway
capture_cmd apt-policy-greetd.txt apt-cache policy greetd
capture_cmd apt-policy-rustc.txt apt-cache policy rustc
capture_cmd apt-upgradable.txt apt list --upgradable
capture_cmd git-head.txt git -C "${ROOT_DIR}" rev-parse HEAD

log "Baseline capture complete"
