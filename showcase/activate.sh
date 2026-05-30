#!/usr/bin/env bash
# Activate showcase mode on the Pi: back up the live config, swap in
# showcase.yaml, and restart the kiosk so it relaunches into the tour.
# Reverse it with ./deactivate.sh. Run as your normal (sudo-capable) user.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SHOWCASE_YAML="${SCRIPT_DIR}/showcase.yaml"

CONFIG="/etc/photoframe/config.yaml"
BACKUP="/etc/photoframe/config.yaml.preshowcase.bak"
SERVICE_USER="${SERVICE_USER:-kiosk}"
MEDIA_ROOT="/var/lib/photoframe/showcase"
PHOTO_DIR="${MEDIA_ROOT}/photos"
BG_DIR="${MEDIA_ROOT}/backgrounds"

log() { printf '[showcase] %s\n' "$*"; }
die() { printf '[showcase] ERROR: %s\n' "$*" >&2; exit 1; }

[[ -f "${SHOWCASE_YAML}" ]] || die "showcase.yaml not found at ${SHOWCASE_YAML}"
sudo test -f "${CONFIG}" || die "no existing ${CONFIG}; deploy photoframe first"

# Ensure the service user can read the media directories.
log "Ensuring ${PHOTO_DIR} and ${BG_DIR} exist (owner ${SERVICE_USER})"
sudo install -d -o "${SERVICE_USER}" -g "${SERVICE_USER}" -m 0755 \
    "${MEDIA_ROOT}" "${PHOTO_DIR}" "${BG_DIR}"

# Warn if the operator hasn't staged any photos yet.
if ! sudo find "${PHOTO_DIR}" -maxdepth 1 -type f \
        \( -iname '*.jpg' -o -iname '*.jpeg' -o -iname '*.png' \) -print -quit \
        | grep -q .; then
    log "WARNING: no photos in ${PHOTO_DIR}. Add images there (see README.md);"
    log "         the slideshow will be empty until you do."
fi
if ! sudo test -f "${BG_DIR}/background.jpg"; then
    log "Note: ${BG_DIR}/background.jpg missing; the fixed-image mat will be skipped."
fi

# Back up the live config exactly once, so re-running activate never clobbers
# the real config with the showcase config.
if sudo test -f "${BACKUP}"; then
    log "Backup already present at ${BACKUP}; leaving it untouched (re-activating)."
else
    log "Backing up ${CONFIG} -> ${BACKUP}"
    sudo cp -p "${CONFIG}" "${BACKUP}"
fi

log "Installing showcase config to ${CONFIG}"
sudo install -m 0644 "${SHOWCASE_YAML}" "${CONFIG}"

log "Restarting greetd to launch the showcase"
sudo systemctl restart greetd

log "Showcase active. Watch it with:  journalctl -t photoframe -f"
log "Restore your normal config with: ${SCRIPT_DIR}/deactivate.sh"
