#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/systemd.sh
source "${SCRIPT_DIR}/../lib/systemd.sh"

if [[ ${EUID} -ne 0 ]]; then
    exec sudo -- "$0" "$@"
fi

FAIL=0

PREFIX="kiosk-diagnostics"

title() {
    printf '[%s] %s\n' "${PREFIX}" "$1"
}

ok() {
    printf '[%s] OK: %s\n' "${PREFIX}" "$1"
}

err() {
    printf '[%s] ERROR: %s\n' "${PREFIX}" "$1" >&2
    FAIL=1
}

warn() {
    printf '[%s] WARN: %s\n' "${PREFIX}" "$1"
}

check_greetd_unit() {
    title 'Checking greetd.service'
    if ! systemd_available; then
        warn 'systemctl not available; skipping greetd.service checks'
        return
    fi

    if systemd_unit_exists greetd.service; then
        ok 'greetd.service unit installed'
    else
        err 'greetd.service unit not found'
        return
    fi

    if systemd_is_enabled greetd.service; then
        ok 'greetd.service enabled'
    else
        err 'greetd.service not enabled'
    fi

    if systemd_is_active greetd.service; then
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

    if grep -Fxq 'command = "/usr/local/bin/photoframe-session"' "${config}"; then
        ok 'config launches photoframe session wrapper'
    else
        err 'config missing photoframe session command'
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

check_sway_config() {
    title 'Validating sway configuration'
    local config="/usr/local/share/photoframe/sway/config"
    if [[ ! -f "${config}" ]]; then
        err "${config} missing"
        return
    fi

    ok 'sway config present'

    if grep -Fq '/usr/local/bin/photoframe' "${config}"; then
        ok 'sway config launches /usr/local/bin/photoframe'
    else
        warn 'sway config missing /usr/local/bin/photoframe exec line'
    fi
}

check_greetd_unit
check_greetd_config
check_sway_config
check_kiosk_user

exit ${FAIL}
