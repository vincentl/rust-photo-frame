#!/usr/bin/env bash
set -euo pipefail

# One-shot installer: provisions the system and deploys the application.
# - Runs the system stage with sudo to install packages, zram, kiosk user, greetd.
# - Immediately builds and deploys the application as the current (unprivileged) user.
# - Uses app deploy auto-tuning to avoid OOM (override via CARGO_BUILD_JOBS=N).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

log() {
  local level="$1"; shift
  printf '[install-all] %s\n' "$level: $*"
}

trap 'log ERROR "install-all failed on line ${LINENO}"' ERR

log INFO "Starting system provisioning stage"
"${SCRIPT_DIR}/system/install.sh"

log INFO "System provisioning complete; proceeding to application deploy"
# Ensure we run app deploy as the invoking user, not root
if [[ $(id -u) -eq 0 ]]; then
  log ERROR "This script must be invoked as an unprivileged user (it will sudo as needed)"
  exit 1
fi

# Pass through commonly overridden env vars; defaults are handled by deploy.sh
env \
  INSTALL_ROOT="${INSTALL_ROOT:-/opt/photo-frame}" \
  SERVICE_USER="${SERVICE_USER:-kiosk}" \
  SERVICE_GROUP="${SERVICE_GROUP:-}" \
  CARGO_PROFILE="${CARGO_PROFILE:-release}" \
  CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-}" \
  "${SCRIPT_DIR}/application/deploy.sh"

log INFO "All done. Quick verification:"
cat <<'TIP'
- Check session and services:
    systemctl status greetd --no-pager
    systemctl status photoframe-wifi-manager --no-pager
    systemctl status buttond --no-pager
- Tail app logs:
    journalctl -t photo-frame -b -n 100
- Edit configuration (requires sudo):
    /etc/photo-frame/config.yaml

If the build was memory constrained, retry with:
    CARGO_BUILD_JOBS=2 ./setup/install-all.sh
TIP
