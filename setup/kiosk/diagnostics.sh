#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID} -ne 0 ]]; then
    exec sudo -- "$0" "$@"
fi

FAIL=0

title() {
    printf '[kiosk-diagnostics] %s\n' "$1"
}

ok() {
    printf '[kiosk-diagnostics] OK: %s\n' "$1"
}

err() {
    printf '[kiosk-diagnostics] ERROR: %s\n' "$1" >&2
    FAIL=1
}

warn() {
    printf '[kiosk-diagnostics] WARN: %s\n' "$1"
}

check_greetd_unit() {
    title 'Checking greetd.service'
    if systemctl list-unit-files greetd.service >/dev/null 2>&1; then
        ok 'greetd.service unit installed'
    else
        err 'greetd.service unit not found'
        return
    fi

    if systemctl is-enabled --quiet greetd.service; then
        ok 'greetd.service enabled'
    else
        err 'greetd.service not enabled'
    fi

    if systemctl is-active --quiet greetd.service; then
        ok 'greetd.service active'
    else
        warn 'greetd.service not running; inspect journalctl -u greetd'
    fi
}

check_greetd_config() {
    title 'Validating /etc/greetd/config.toml'
    local config="/etc/greetd/config.toml"
    if [[ ! -f "${config}" ]]; then
        err "${config} missing"
        return
    fi

    ok 'config file present'

    if grep -Fxq 'vt = 1' "${config}"; then
        ok 'virtual terminal set to 1'
    else
        err 'config missing "vt = 1"'
    fi

    if grep -Fxq 'command = "cage -s -- systemd-cat --identifier=rust-photo-frame env RUST_LOG=info /opt/photo-frame/bin/rust-photo-frame /var/lib/photo-frame/config/config.yaml"' "${config}"; then
        ok 'command matches cage launch'
    else
        err 'config missing cage command'
    fi

    if grep -Fxq 'user = "kiosk"' "${config}"; then
        ok 'user set to kiosk'
    else
        err 'config missing kiosk user'
    fi
}

check_kiosk_user() {
    title 'Inspecting kiosk user'
    if ! id -u kiosk >/dev/null 2>&1; then
        err 'kiosk user does not exist'
        return
    fi

    ok 'kiosk user exists'

    local missing=()
    local group
    for group in video render input; do
        if ! id -nG kiosk | tr ' ' '\n' | grep -Fxq "${group}"; then
            missing+=("${group}")
        fi
    done

    if (( ${#missing[@]} == 0 )); then
        ok 'kiosk user has video, render, and input groups'
    else
        err "kiosk user missing groups: ${missing[*]}"
    fi
}

check_greetd_unit
check_greetd_config
check_kiosk_user

exit ${FAIL}
