#!/usr/bin/env bash
set -euo pipefail

MODULE="system:20-kiosk-user"
DRY_RUN="${DRY_RUN:-0}"
KIOSK_USER="${KIOSK_USER:-kiosk}"
KIOSK_GROUPS=(render video input)

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

user_exists() {
    id -u "$1" >/dev/null 2>&1
}

ensure_user() {
    if user_exists "${KIOSK_USER}"; then
        log INFO "User ${KIOSK_USER} already exists"
        return
    fi
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would create user ${KIOSK_USER}"
        return
    fi
    run_sudo adduser --disabled-password --gecos "" "${KIOSK_USER}"
    log INFO "Created user ${KIOSK_USER}"
}

ensure_group_membership() {
    local group="$1"
    if ! getent group "${group}" >/dev/null 2>&1; then
        log WARN "Expected group ${group} is missing; install GPU/input stack first"
        return
    fi
    if id -nG "${KIOSK_USER}" 2>/dev/null | tr ' ' '\n' | grep -Fxq "${group}"; then
        log INFO "${KIOSK_USER} already a member of ${group}"
        return
    fi
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would add ${KIOSK_USER} to ${group}"
    else
        run_sudo usermod -aG "${group}" "${KIOSK_USER}"
        log INFO "Added ${KIOSK_USER} to ${group}"
    fi
}

ensure_user
for group in "${KIOSK_GROUPS[@]}"; do
    ensure_group_membership "${group}"
done

log INFO "Kiosk user ${KIOSK_USER} ready"
