#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: setup/system/run.sh [options]

Options:
  --with-legacy-cleanup  Run the optional legacy cleanup script after provisioning.
  -h, --help             Show this help message and exit.
USAGE
}

needs_root() {
  if [[ $EUID -ne 0 ]]; then
    echo "[photoframe-setup] This script must be run as root. Re-running with sudo..." >&2
    exec sudo "$0" "$@"
  fi
}

log_step() {
  echo
  echo "[photoframe-setup] === $1 ==="
}

main() {
  local with_legacy_cleanup=0
  local -a passthrough=("$@")

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --with-legacy-cleanup)
        with_legacy_cleanup=1
        shift
        ;;
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

  needs_root "${passthrough[@]}"

  local script_dir
  script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

  log_step "Installing packages"
  "${script_dir}/install-packages.sh"

  log_step "Creating users and setting permissions"
  "${script_dir}/create-users-and-perms.sh"

  log_step "Configuring NetworkManager"
  "${script_dir}/configure-networkmanager.sh"

  log_step "Installing sudoers rules"
  "${script_dir}/install-sudoers.sh"

  log_step "Installing systemd units"
  "${script_dir}/install-systemd-units.sh"

  if [[ ${with_legacy_cleanup} -eq 1 ]]; then
    local migrate_dir
    migrate_dir=$(cd "${script_dir}/.." && pwd)/migrate
    if [[ -x "${migrate_dir}/legacy-cleanup.sh" ]]; then
      log_step "Running legacy cleanup"
      "${migrate_dir}/legacy-cleanup.sh"
    else
      echo "[photoframe-setup] Skipping legacy cleanup: script not found or not executable" >&2
    fi
  fi

  echo
  echo "[photoframe-setup] System provisioning completed successfully."
}

main "$@"
