#!/usr/bin/env bash
# Deactivate showcase mode: restore the config activate.sh backed up and
# restart the kiosk so it returns to your normal slideshow.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
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

log "Restarting greetd to return to your normal configuration"
sudo systemctl restart greetd

log "Showcase deactivated; normal slideshow restored."
