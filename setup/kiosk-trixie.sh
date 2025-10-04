#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

log() {
    printf '[kiosk-setup] %s\n' "$*"
}

die() {
    printf '[kiosk-setup] ERROR: %s\n' "$*" >&2
    exit 1
}

require_root() {
    if [[ ${EUID} -ne 0 ]]; then
        exec sudo -- "$0" "$@"
    fi
}

require_trixie() {
    if [[ ! -r /etc/os-release ]]; then
        die "/etc/os-release not found; cannot detect OS"
    fi
    # shellcheck disable=SC1091
    . /etc/os-release
    if [[ "${VERSION_CODENAME:-}" != "trixie" ]]; then
        die "Debian 13 (trixie) is required"
    fi
}

require_commands() {
    local missing=()
    local cmd
    for cmd in apt-get install getent groupadd id install mkdir systemctl useradd usermod; do
        if ! command -v "${cmd}" >/dev/null 2>&1; then
            missing+=("${cmd}")
        fi
    done
    if (( ${#missing[@]} > 0 )); then
        die "Missing required commands: ${missing[*]}"
    fi
}

ensure_packages() {
    local packages=(
        cage
        greetd
        mesa-vulkan-drivers
        vulkan-tools
        wayland-protocols
        wlr-randr
    )
    log "Installing packages: ${packages[*]}"
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends "${packages[@]}"
}

ensure_kiosk_user() {
    local user="kiosk"
    if ! id -u "${user}" >/dev/null 2>&1; then
        log "Creating kiosk user ${user}"
        useradd --create-home --shell /usr/sbin/nologin "${user}"
    else
        log "User ${user} already exists"
        usermod --shell /usr/sbin/nologin "${user}" >/dev/null 2>&1 || true
        if [[ ! -d "/home/${user}" ]]; then
            log "Ensuring home directory for ${user}"
            install -d -m 0750 "/home/${user}"
            chown "${user}:${user}" "/home/${user}"
        fi
    fi

    local group
    for group in render video input; do
        if ! getent group "${group}" >/dev/null 2>&1; then
            log "Creating group ${group}"
            groupadd "${group}"
        fi
        if ! id -nG "${user}" | tr ' ' '\n' | grep -Fxq "${group}"; then
            log "Adding ${user} to ${group}"
            usermod -aG "${group}" "${user}"
        fi
    done

    if [[ ! -d "/home/${user}" ]]; then
        log "Creating home directory for ${user}"
        install -d -m 0750 "/home/${user}"
        chown "${user}:${user}" "/home/${user}"
    fi
}

write_greetd_config() {
    local config_dir="/etc/greetd"
    local config_file="${config_dir}/config.toml"
    log "Writing ${config_file}"
    install -d -m 0755 "${config_dir}"
    cat <<'CONFIG' >"${config_file}"
[terminal]
vt = 1

[default_session]
command = "cage -s -- /opt/photo-frame/bin/rust-photo-frame /var/lib/photo-frame/config/config.yaml"
user = "kiosk"
CONFIG
    chmod 0644 "${config_file}"
}

install_auxiliary_units() {
    log "Installing photoframe systemd units"
    local unit
    for unit in "${REPO_ROOT}"/assets/systemd/photoframe-*; do
        [ -f "${unit}" ] || continue
        install -D -m 0644 "${unit}" "/etc/systemd/system/$(basename "${unit}")"
    done
}

enable_systemd_units() {
    log "Enabling kiosk services"
    systemctl daemon-reload

    local dm
    for dm in gdm3.service sddm.service lightdm.service; do
        if systemctl list-unit-files "${dm}" >/dev/null 2>&1; then
            log "Disabling conflicting display manager ${dm}"
            systemctl disable --now "${dm}" >/dev/null 2>&1 || true
        fi
    done

    log "Setting default boot target to graphical.target"
    systemctl set-default graphical.target

    if systemctl list-unit-files getty@tty1.service >/dev/null 2>&1; then
        log "Disabling and masking getty@tty1.service to avoid VT contention"
        systemctl disable --now getty@tty1.service >/dev/null 2>&1 || true
        systemctl mask getty@tty1.service >/dev/null 2>&1 || true
    fi

    log "Setting greetd as the system display manager"
    systemctl enable --now greetd.service >/dev/null 2>&1 || true

    log "Verifying display-manager alias"
    systemctl status display-manager.service --no-pager || true

    local unit
    for unit in photoframe-wifi-manager.service photoframe-buttond.service; do
        if systemctl list-unit-files "${unit}" >/dev/null 2>&1; then
            systemctl enable --now "${unit}"
        fi
    done

    if systemctl list-unit-files photoframe-sync.timer >/dev/null 2>&1; then
        systemctl enable photoframe-sync.timer
        systemctl start photoframe-sync.timer || true
    fi
}

main() {
    require_root "$@"
    require_trixie
    require_commands

    ensure_packages
    ensure_kiosk_user
    write_greetd_config
    install_auxiliary_units
    enable_systemd_units

    log "Kiosk provisioning complete. greetd will launch cage on tty1 as kiosk."
}

main "$@"
