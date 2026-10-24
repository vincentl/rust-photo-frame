#!/usr/bin/env bash
set -euo pipefail

MODULE="app:45-activate-services"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/photoframe}"
SYNC_ENV_PATH="${SYNC_ENV_PATH:-/etc/photoframe/sync.env}"
SYNC_TIMER="${SYNC_TIMER:-photoframe-sync.timer}"
SYNC_SERVICE="${SYNC_SERVICE:-photoframe-sync.service}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../../lib/systemd.sh
source "${SCRIPT_DIR}/../../lib/systemd.sh"
# shellcheck source=../../lib/display-stack.sh
source "${SCRIPT_DIR}/../../lib/display-stack.sh"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${MODULE}" "$level: $*"
}

run_sudo() {
    sudo "$@"
}

# Re-create a unit's enable symlinks from a clean slate. `systemctl enable` only
# *adds* the symlink for the unit's current [Install] WantedBy=; it never removes a
# stale want left behind when a unit changes target between releases. A leftover
# want in a now-wrong target (e.g. buttond's old multi-user.target.wants/ after it
# moved to graphical.target) tangles boot-time ordering and can make systemd
# silently drop the unit's start job — so it never starts at boot, with no error in
# its own journal. Strip every existing *.wants symlink for the unit, then enable
# fresh so only the current target wants it.
reenable_clean() {
    local unit="$1"
    run_sudo find /etc/systemd/system -type l -name "${unit}" -path '*/*.wants/*' -delete 2>/dev/null || true
    if ! run_sudo systemctl enable "${unit}" >/dev/null 2>&1; then
        log WARN "Failed to enable ${unit}"
    fi
}

# Restart a unit, surfacing failures instead of swallowing them. The old
# `restart ... || true` hid a failed (re)start during deploy — exactly how buttond
# could end up stopped after a deploy with nobody noticing.
restart_unit() {
    local unit="$1"
    log INFO "Restarting ${unit}"
    if ! run_sudo systemctl restart "${unit}" >/dev/null 2>&1; then
        log WARN "Failed to restart ${unit}; check 'systemctl status ${unit}' and 'journalctl -u ${unit} -b'"
    fi
}

configure_sync_timer() {
    if ! run_sudo systemctl list-unit-files "${SYNC_TIMER}" >/dev/null 2>&1; then
        log INFO "${SYNC_TIMER} not installed; skipping sync timer activation"
        return
    fi

    if sync_is_configured "${SYNC_ENV_PATH}"; then
        log INFO "Sync source configured; enabling ${SYNC_TIMER}"
        run_sudo systemctl enable "${SYNC_TIMER}" >/dev/null 2>&1 || true
        run_sudo systemctl start "${SYNC_TIMER}" >/dev/null 2>&1 || true
        return
    fi

    log INFO "Sync source not configured in ${SYNC_ENV_PATH}; keeping ${SYNC_TIMER} disabled"
    run_sudo systemctl disable "${SYNC_TIMER}" >/dev/null 2>&1 || true
    run_sudo systemctl stop "${SYNC_TIMER}" >/dev/null 2>&1 || true
    run_sudo systemctl stop "${SYNC_SERVICE}" >/dev/null 2>&1 || true
    run_sudo systemctl reset-failed "${SYNC_SERVICE}" "${SYNC_TIMER}" >/dev/null 2>&1 || true
}

if ! systemd_available; then
    log WARN "systemctl not available; skipping service activation"
    exit 0
fi

# Install or refresh app unit files from the installed tree
UNIT_SRC_DIR="${INSTALL_ROOT}/systemd"
if [[ -d "${UNIT_SRC_DIR}" ]]; then
    log INFO "Installing systemd unit files from ${UNIT_SRC_DIR}"
    while IFS= read -r -d '' unit_file; do
        dest="/etc/systemd/system/$(basename "${unit_file}")"
        run_sudo install -D -m 0644 "${unit_file}" "${dest}"
    done < <(find "${UNIT_SRC_DIR}" -type f \( -name '*.service' -o -name '*.timer' \) -print0)
    run_sudo systemctl daemon-reload
else
    log WARN "Unit source directory not found: ${UNIT_SRC_DIR}"
fi

# An explicit wayland choice that cannot be applied here (no greetd on a frame
# set up on KMS) is dropped, so everything below names the running stack.
display_stack_refuse_unavailable
display_choice="$(display_stack_resolve)"

# The wayland fallback's sway session takes its seat from seatd; the KMS
# stack (the default) does not use it.
if [[ "${display_choice}" == "wayland" ]]; then
    for seatd_unit in seatd.socket seatd.service; do
        if run_sudo systemctl list-unit-files "${seatd_unit}" >/dev/null 2>&1; then
            log INFO "Enabling ${seatd_unit}"
            run_sudo systemctl enable "${seatd_unit}" >/dev/null 2>&1 || true
            log INFO "Starting ${seatd_unit}"
            run_sudo systemctl start "${seatd_unit}" >/dev/null 2>&1 || true
        fi
    done
fi

# Enable and (re)start the chosen display stack now that binaries are present:
# photoframe-kms.service (default) or greetd/sway (PHOTOFRAME_DISPLAY=wayland,
# persisted in /etc/photoframe/display.env). A clean restart picks up the
# freshly installed binaries and recreates the control socket without a reboot.
# A frame set up on KMS has no greetd: display_stack_apply says how to switch.
log INFO "Display stack: $(display_stack_describe)"
if ! display_stack_apply "${display_choice}" start; then
    log WARN "Display stack '${display_choice}' did not start cleanly; see the messages above"
fi

# Enable and start app-specific services if present
for unit in photoframe-wifi-manager.service buttond.service; do
    if run_sudo systemctl list-unit-files "${unit}" >/dev/null 2>&1; then
        log INFO "Enabling ${unit}"
        reenable_clean "${unit}"
        restart_unit "${unit}"
    fi
done

# Re-assert the daemon's view after stripping stale enable symlinks above, so the
# next boot evaluates target dependencies against the corrected wants.
run_sudo systemctl daemon-reload

configure_sync_timer

log INFO "Kiosk services activated"
