#!/usr/bin/env bash
# Deactivate showcase mode: restore the config activate.sh backed up and
# restart the kiosk so it returns to your normal slideshow.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=../setup/lib/systemd.sh
source "${REPO_ROOT}/setup/lib/systemd.sh"
# shellcheck source=../setup/lib/display-stack.sh
source "${REPO_ROOT}/setup/lib/display-stack.sh"
CONFIG="/etc/photoframe/config.yaml"
BACKUP="/etc/photoframe/config.yaml.preshowcase.bak"

log() { printf '[showcase] %s\n' "$*"; }
die() { printf '[showcase] ERROR: %s\n' "$*" >&2; exit 1; }

if ! sudo test -f "${BACKUP}"; then
    die "no backup at ${BACKUP}; nothing to restore (was activate.sh run?)."
fi

log "Restoring ${CONFIG} from ${BACKUP}"
sudo cp -p "${BACKUP}" "${CONFIG}"
sudo rm -f "${BACKUP}"

display_unit="$(display_stack_unit "$(display_stack_current)")"
log "Restarting ${display_unit} to return to your normal configuration"
if [[ "${display_unit}" == "${DISPLAY_WAYLAND_UNIT}" ]]; then
    # Never `restart` greetd (see docs/operate.md): stop, sleep, start.
    sudo systemctl stop greetd
    sleep 1
    sudo systemctl start greetd
else
    sudo systemctl restart "${display_unit}"
fi

log "Showcase deactivated; normal slideshow restored."
