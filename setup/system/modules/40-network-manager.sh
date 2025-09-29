#!/usr/bin/env bash
set -euo pipefail

MODULE="system:40-network-manager"
DRY_RUN="${DRY_RUN:-0}"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${MODULE}" "$level: $*"
}

run_sudo() {
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: sudo $*"
    else
        sudo "$@"
    fi
}

CONFIG_PATH="/etc/NetworkManager/conf.d/photo-frame.conf"
CONFIG_CONTENT='[ifupdown]
managed=true

[device]
wifi.scan-rand-mac-address=no

[connection]
wifi.powersave=2
'

ensure_config() {
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would install NetworkManager config at ${CONFIG_PATH}"
        return
    fi

    local tmpfile
    tmpfile="$(mktemp)"
    trap 'rm -f "${tmpfile}"' RETURN
    printf '%s' "${CONFIG_CONTENT}" >"${tmpfile}"

    if [[ -f "${CONFIG_PATH}" ]] && sudo cmp -s "${tmpfile}" "${CONFIG_PATH}"; then
        log INFO "NetworkManager config already up to date"
        return
    fi

    run_sudo install -Dm644 "${tmpfile}" "${CONFIG_PATH}"
    log INFO "Installed NetworkManager config at ${CONFIG_PATH}"
}

disable_conflicting_services() {
    local services=(dhcpcd.service wpa_supplicant.service)
    for svc in "${services[@]}"; do
        if [[ "${DRY_RUN}" == "1" ]]; then
            log INFO "DRY_RUN: would disable and stop ${svc}"
            continue
        fi
        if systemctl list-unit-files | grep -Fq "${svc}"; then
            run_sudo systemctl disable --now "${svc}" >/dev/null 2>&1 || true
            log INFO "Disabled ${svc}"
        fi
    done

    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would mask dhcpcd.service"
    else
        run_sudo systemctl mask dhcpcd.service >/dev/null 2>&1 || true
        log INFO "Masked dhcpcd.service"
    fi
}

enable_network_manager() {
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would enable and restart NetworkManager"
        return
    fi

    run_sudo systemctl enable NetworkManager.service >/dev/null 2>&1 || true
    run_sudo systemctl restart NetworkManager.service
    log INFO "NetworkManager enabled and restarted"
}

ensure_config
disable_conflicting_services
enable_network_manager

log INFO "NetworkManager ready for Wi-Fi provisioning"
