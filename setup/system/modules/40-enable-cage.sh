#!/usr/bin/env bash
set -euo pipefail

MODULE="system:40-enable-cage"
DRY_RUN="${DRY_RUN:-0}"
TTY_DEVICE="${CAGE_TTY:-tty1}"
SERVICE_NAME="cage@${TTY_DEVICE}.service"

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

log INFO "Setting default boot target to graphical"
if [[ "${DRY_RUN}" == "1" ]]; then
    log INFO "DRY_RUN: would run systemctl set-default graphical.target"
else
    run_sudo systemctl set-default graphical.target
fi

log INFO "Enabling ${SERVICE_NAME}"
if [[ "${DRY_RUN}" == "1" ]]; then
    log INFO "DRY_RUN: would enable ${SERVICE_NAME}"
else
    run_sudo systemctl enable "${SERVICE_NAME}"
fi

log INFO "Starting ${SERVICE_NAME}"
if [[ "${DRY_RUN}" == "1" ]]; then
    log INFO "DRY_RUN: would start ${SERVICE_NAME}"
else
    run_sudo systemctl start "${SERVICE_NAME}"
fi

log INFO "Cage compositor service enabled"
