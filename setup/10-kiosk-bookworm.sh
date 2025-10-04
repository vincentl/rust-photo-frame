#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
LIB_DIR="${SCRIPT_DIR}/lib"

log() {
    printf '[kiosk-setup] %s\n' "$*"
}

usage() {
    cat <<USAGE
Usage: sudo ./setup/10-kiosk-bookworm.sh [--user NAME] [--app PATH]

Options:
  --user NAME   Kiosk service account to run Cage (default: kiosk)
  --app PATH    Wayland kiosk binary executed by Cage (default: /usr/local/bin/photo-app)
  --help        Show this message and exit
USAGE
}

reexec_as_root() {
    if [[ ${EUID} -ne 0 ]]; then
        exec sudo -- "$0" "$@"
    fi
}

parse_args() {
    KIOSK_USER="kiosk"
    APP_PATH="/usr/local/bin/photo-app"

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --user)
                KIOSK_USER="$2"
                shift 2
                ;;
            --app)
                APP_PATH="$2"
                shift 2
                ;;
            --help)
                usage
                exit 0
                ;;
            *)
                echo "Unknown argument: $1" >&2
                usage >&2
                exit 1
                ;;
        esac
    done
}

require_bookworm() {
    if [[ ! -f /etc/os-release ]]; then
        echo "/etc/os-release not found; unable to detect OS" >&2
        exit 1
    fi
    # shellcheck disable=SC1091
    . /etc/os-release
    if [[ "${VERSION_CODENAME:-}" != "bookworm" ]]; then
        echo "This script supports Raspberry Pi OS Bookworm only." >&2
        exit 1
    fi
}

require_commands() {
    local cmd
    for cmd in install getent usermod useradd groupadd id apt-get python3 systemctl; do
        if ! command -v "${cmd}" >/dev/null 2>&1; then
            echo "Missing required command: ${cmd}" >&2
            exit 1
        fi
    done
}

ensure_packages() {
    local packages=(cage seatd plymouth)
    log "Installing packages: ${packages[*]}"
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends "${packages[@]}"
}

ensure_seatd_active() {
    if ! systemctl is-enabled seatd.service >/dev/null 2>&1; then
        log "Enabling seatd.service"
        systemctl enable seatd.service
    fi
    if ! systemctl is-active seatd.service >/dev/null 2>&1; then
        log "Starting seatd.service"
        systemctl start seatd.service
    fi
}

ensure_user_exists() {
    local user="$1"
    if id -u "${user}" >/dev/null 2>&1; then
        return
    fi
    log "Creating kiosk user ${user}"
    useradd --create-home --shell /usr/sbin/nologin "${user}"
}

ensure_group_membership() {
    local user="$1" group="$2"
    if ! getent group "${group}" >/dev/null 2>&1; then
        log "Creating missing group ${group}"
        groupadd "${group}"
    fi
    if id -nG "${user}" | tr ' ' '\n' | grep -Fxq "${group}"; then
        return
    fi
    log "Adding ${user} to ${group}"
    usermod -aG "${group}" "${user}"
}

render_template() {
    local template="$1" dest="$2"
    local tmp
    tmp="$(mktemp)"
    python3 - "$template" "$tmp" "$KIOSK_USER" "$APP_PATH" <<'PY'
import pathlib
import sys

src = pathlib.Path(sys.argv[1]).read_text()
dest = pathlib.Path(sys.argv[2])
user = sys.argv[3]
app = sys.argv[4]
rendered = src.replace('{{KIOSK_USER}}', user).replace('{{APP_PATH}}', app)
dest.write_text(rendered)
PY
    install -D -m 0644 "${tmp}" "${dest}"
    rm -f "${tmp}"
}

install_cage_unit() {
    local template="${REPO_ROOT}/assets/systemd/cage@.service"
    local dest="/etc/systemd/system/cage@.service"
    render_template "${template}" "${dest}"
}

install_pam_stack() {
    local src="${REPO_ROOT}/assets/pam/cage"
    install -D -m 0644 "${src}" /etc/pam.d/cage
}

install_auxiliary_units() {
    local unit
    shopt -s nullglob
    for unit in "${REPO_ROOT}"/assets/systemd/photoframe-*; do
        install -D -m 0644 "${unit}" "/etc/systemd/system/$(basename "${unit}")"
    done
    shopt -u nullglob
}

enable_units() {
    local unit
    systemd_daemon_reload

    systemd_disable_unit getty@tty1.service
    systemd_stop_unit getty@tty1.service

    local enable_list=(cage@tty1.service photoframe-wifi-manager.service photoframe-buttond.service photoframe-sync.timer)
    for unit in "${enable_list[@]}"; do
        if systemd_unit_exists "${unit}"; then
            log "Enabling ${unit}"
            systemd_enable_unit "${unit}"
            if [[ "${unit}" == *.timer ]]; then
                systemd_start_unit "${unit}" || true
            else
                systemd_restart_unit "${unit}" || true
            fi
        fi
    done

    systemctl set-default graphical.target
}

cleanup_display_managers() {
    local dm
    local managers=(lightdm.service gdm.service gdm3.service sddm.service lxdm.service slim.service)
    for dm in "${managers[@]}"; do
        if systemd_unit_exists "${dm}"; then
            systemd_disable_unit "${dm}"
            systemd_stop_unit "${dm}"
        fi
    done
}

update_cmdline() {
    # shellcheck source=/dev/null
    . "${LIB_DIR}/raspi_boot.sh"
    local changed=0
    ensure_cmdline_without_console_tty1 "${RASPI_CMDLINE:-/boot/firmware/cmdline.txt}" changed
    if [[ "${changed}" -eq 1 ]]; then
        log "Removed console=tty1 from cmdline.txt"
        update_initramfs_if_available
    fi
}

main() {
    reexec_as_root "$@"
    parse_args "$@"
    require_bookworm
    require_commands
    # shellcheck source=/dev/null
    . "${LIB_DIR}/systemd.sh"

    ensure_packages
    ensure_seatd_active
    ensure_user_exists "${KIOSK_USER}"
    local group
    for group in render video input; do
        ensure_group_membership "${KIOSK_USER}" "${group}"
    done

    cleanup_display_managers
    install_cage_unit
    install_pam_stack
    install_auxiliary_units

    update_cmdline
    enable_units

    log "Kiosk environment configured for user ${KIOSK_USER} running ${APP_PATH}"
    log "Reboot to launch Cage on tty1."
}

main "$@"
