#!/usr/bin/env bash
set -euo pipefail

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
  needs_root "$@"

  local script_dir
  script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

  log_step "Creating users and setting permissions"
  "${script_dir}/create-users-and-perms.sh"

  log_step "Configuring NetworkManager"
  "${script_dir}/configure-networkmanager.sh"

  log_step "Installing sudoers rules"
  "${script_dir}/install-sudoers.sh"

  log_step "Installing systemd units"
  "${script_dir}/install-systemd-units.sh"

  echo
  echo "[photoframe-setup] System provisioning completed successfully."
}

main "$@"
