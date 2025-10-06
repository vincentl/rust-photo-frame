#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT_NAME="$(basename "$0")"

log() {
    printf '[%s] %s\n' "${SCRIPT_NAME}" "$*"
}

die() {
    printf '[%s] ERROR: %s\n' "${SCRIPT_NAME}" "$*" >&2
    exit 1
}

require_root() {
    if [[ ${EUID} -ne 0 ]]; then
        exec sudo -- "$0" "$@"
    fi
}

require_commands() {
    local missing=()
    local cmd
    for cmd in apt-get dpkg-query install rm systemctl swapoff swapon; do
        if ! command -v "${cmd}" >/dev/null 2>&1; then
            missing+=("${cmd}")
        fi
    done
    if (( ${#missing[@]} > 0 )); then
        die "Missing required commands: ${missing[*]}"
    fi
}

disable_default_swap() {
    local swap_entries
    swap_entries="$(swapon --noheadings --show=NAME 2>/dev/null || true)"
    if grep -Fxq '/var/swap' <<<"${swap_entries}"; then
        log "Disabling active /var/swap"
        swapoff /var/swap || true
    fi

    if systemctl list-unit-files dphys-swapfile.service >/dev/null 2>&1; then
        log "Disabling dphys-swapfile.service"
        systemctl disable --now dphys-swapfile.service >/dev/null 2>&1 || true
    fi

    if dpkg-query -W -f='${Status}' dphys-swapfile 2>/dev/null | grep -q 'install ok installed'; then
        log "Purging dphys-swapfile package"
        export DEBIAN_FRONTEND=noninteractive
        apt-get purge -y dphys-swapfile
    fi

    if [[ -f /etc/dphys-swapfile ]]; then
        log "Removing /etc/dphys-swapfile"
        rm -f /etc/dphys-swapfile
    fi

    if [[ -f /var/swap ]]; then
        log "Removing /var/swap"
        rm -f /var/swap
    fi
}

ensure_zram_packages() {
    local packages=(systemd-zram-generator)
    local install_list=()
    local pkg
    for pkg in "${packages[@]}"; do
        if ! dpkg-query -W -f='${Status}' "${pkg}" 2>/dev/null | grep -q 'install ok installed'; then
            install_list+=("${pkg}")
        fi
    done

    if (( ${#install_list[@]} > 0 )); then
        log "Installing packages: ${install_list[*]}"
        export DEBIAN_FRONTEND=noninteractive
        apt-get update
        apt-get install -y --no-install-recommends "${install_list[@]}"
    else
        log "Required zram packages already installed"
    fi
}

configure_zram_generator() {
    local config_dir="/etc/systemd/zram-generator.conf.d"
    local config_file="${config_dir}/photoframe.conf"

    log "Configuring systemd zram generator"
    install -d -m 0755 "${config_dir}"
    cat <<'CONF' >"${config_file}"
# Managed by setup/install-zram.sh
# Configure a compressed swap device sized to half of physical memory (up to 2 GiB).
# Higher priority ensures the kernel prefers zram swap over any other device if reintroduced.
[zram0]
compression-algorithm = zstd
zram-size = min(ram / 2, 2G)
swap-priority = 100
CONF
    chmod 0644 "${config_file}"
}

activate_zram() {
    if systemctl list-unit-files systemd-zram-setup@zram0.service >/dev/null 2>&1; then
        if systemctl is-active --quiet systemd-zram-setup@zram0.service; then
            log "Restarting systemd-zram-setup@zram0.service"
            systemctl restart systemd-zram-setup@zram0.service
        else
            log "Starting systemd-zram-setup@zram0.service"
            systemctl start systemd-zram-setup@zram0.service
        fi
    else
        log "systemd-zram-setup@zram0.service unavailable; run 'systemctl daemon-reload' and reboot to activate zram"
        return
    fi

    log "Active swap devices after zram setup:"
    swapon --show
}

main() {
    require_root "$@"
    require_commands

    disable_default_swap
    ensure_zram_packages
    configure_zram_generator
    systemctl daemon-reload
    activate_zram

    log "zram swap provisioning complete."
}

main "$@"
