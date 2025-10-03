#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: setup/packages/run.sh

Installs operating system dependencies required by the photo frame build and runtime.
USAGE
}

needs_root() {
  if [[ $EUID -ne 0 ]]; then
    echo "[photoframe-packages] This script must be run as root. Re-running with sudo..." >&2
    exec sudo "$0" "$@"
  fi
}

log_step() {
  echo
  echo "[photoframe-packages] === $1 ==="
}

main() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      -h|--help)
        usage
        exit 0
        ;;
      *)
        echo "Unknown option: $1" >&2
        usage
        exit 1
        ;;
    esac
  done

  needs_root "$@"

  local script_dir
  script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

  log_step "Installing operating system packages"
  "${script_dir}/install-packages.sh"

  echo
  echo "[photoframe-packages] Package installation completed successfully."
}

main "$@"
