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

  log_step "Installing OS packages"
  "${script_dir}/install-apt-packages.sh"

  log_step "Installing Rust toolchain"
  "${script_dir}/install-rust.sh"
  
  green=$(tput setaf 2)
  reset=$(tput sgr0) 
  
  echo
  echo "[photoframe-setup]${green} Package provisioning completed successfully.${reset}"
  echo "[photoframe-setup]${green} Exit and reconnect to get a new shell that picks up the updated PATH if this was your first run.${reset}"
}

main "$@"
