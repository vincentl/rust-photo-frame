#!/usr/bin/env bash
set -euo pipefail

SCRIPT_NAME="$(basename "$0")"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/systemd.sh
source "${SCRIPT_DIR}/../lib/systemd.sh"
# shellcheck source=../lib/display-stack.sh
source "${SCRIPT_DIR}/../lib/display-stack.sh"

log() {
    local level="$1"; shift
    printf '[%s] %s: %s\n' "${SCRIPT_NAME}" "${level}" "$*"
}

run_systemctl() {
    if [[ $(id -u) -eq 0 ]]; then
        systemctl "$@"
    else
        sudo systemctl "$@"
    fi
}

if ! command -v systemctl >/dev/null 2>&1; then
    log WARN "systemctl not available; skipping VT1 ownership checks"
    exit 0
fi

fail=false

if run_systemctl is-enabled getty@tty1.service 2>/dev/null | grep -q 'enabled'; then
    echo "[postcheck] ERROR: getty@tty1.service is enabled; this steals VT1 from the display stack"
    fail=true
fi

if run_systemctl is-active getty@tty1.service 2>/dev/null | grep -q 'active'; then
    echo "[postcheck] ERROR: getty@tty1.service is running on tty1"
    fail=true
fi

display_choice="$(display_stack_current)"
if [[ "${display_choice}" == "kms" ]]; then
    if ! run_systemctl is-active --quiet "${DISPLAY_KMS_UNIT}"; then
        echo "[postcheck] ERROR: ${DISPLAY_KMS_UNIT} is not active"
        fail=true
    fi
else
    if ! run_systemctl is-active --quiet greetd.service; then
        echo "[postcheck] ERROR: greetd.service is not active"
        fail=true
    else
        if ! run_systemctl status greetd --no-pager -l | grep -q '/usr/local/bin/photoframe-session'; then
            echo "[postcheck] ERROR: greetd is not launching /usr/local/bin/photoframe-session"
            fail=true
        fi
    fi
fi

if [[ "${fail}" == true ]]; then
    log ERROR "System postcheck failed"
    exit 1
fi

log INFO "VT1 ownership verified; display stack $(display_stack_describe)"
