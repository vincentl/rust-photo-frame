#!/usr/bin/env bash
set -euo pipefail

MODULE="app:05-service-user"
SERVICE_USER="${SERVICE_USER:-kiosk}"
SERVICE_GROUP="${SERVICE_GROUP:-}" 
DEFAULT_SHELL="/usr/sbin/nologin"

log() {
    local level="$1"; shift
    printf '[%s] %s\n' "${MODULE}" "$level: $*"
}

run_sudo() {
    sudo "$@"
}

resolve_service_group() {
    if id -u "${SERVICE_USER}" >/dev/null 2>&1; then
        SERVICE_GROUP="${SERVICE_GROUP:-$(id -gn "${SERVICE_USER}")}"
    fi
    SERVICE_GROUP="${SERVICE_GROUP:-${SERVICE_USER}}"
}

ensure_group_exists() {
    if getent group "${SERVICE_GROUP}" >/dev/null 2>&1; then
        return
    fi
    log INFO "Creating service group ${SERVICE_GROUP}"
    run_sudo groupadd --system "${SERVICE_GROUP}"
}

ensure_user_exists() {
    if id -u "${SERVICE_USER}" >/dev/null 2>&1; then
        local current_group
        current_group="$(id -gn "${SERVICE_USER}")"
        if [[ "${current_group}" != "${SERVICE_GROUP}" ]]; then
            log ERROR "Service user ${SERVICE_USER} exists but primary group is ${current_group}; expected ${SERVICE_GROUP}."
            log ERROR "Adjust SERVICE_GROUP or update the account before rerunning."
            exit 1
        fi
        log INFO "Service user ${SERVICE_USER}:${SERVICE_GROUP} already present"
        return
    fi

    log INFO "Creating service user ${SERVICE_USER}"
    local args=(--create-home --shell "${DEFAULT_SHELL}" --gid "${SERVICE_GROUP}")
    run_sudo useradd "${args[@]}" "${SERVICE_USER}"
}

main() {
    resolve_service_group
    ensure_group_exists
    ensure_user_exists
}

main "$@"
