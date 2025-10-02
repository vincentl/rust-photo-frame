#!/usr/bin/env bash
set -euo pipefail

MODULE="system:40-logging"
DRY_RUN="${DRY_RUN:-0}"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/photo-frame}"
SERVICE_USER="${SERVICE_USER:-kiosk}"
if id -u "${SERVICE_USER}" >/dev/null 2>&1; then
    SERVICE_GROUP="${SERVICE_GROUP:-$(id -gn "${SERVICE_USER}")}"
else
    SERVICE_GROUP="${SERVICE_GROUP:-${SERVICE_USER}}"
fi
PHOTO_SERVICE="${PHOTO_SERVICE:-cage@tty1.service}"

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

ensure_directory() {
    local path="$1" mode="$2" owner="$3" group="$4"
    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would ensure directory ${path} (${mode} ${owner}:${group})"
        return
    fi
    run_sudo install -d -m "${mode}" -o "${owner}" -g "${group}" "${path}"
}

write_config_if_changed() {
    local path="$1" mode="$2" owner="$3" group="$4" content="$5"

    if [[ "${DRY_RUN}" == "1" ]]; then
        log INFO "DRY_RUN: would install ${path} (${mode} ${owner}:${group}) with contents:" 
        printf '%s\n' "${content}"
        return 2
    fi

    local tmp
    tmp="$(mktemp)"
    printf '%s\n' "${content}" > "${tmp}"

    if run_sudo test -f "${path}"; then
        if run_sudo cmp -s "${tmp}" "${path}"; then
            rm -f "${tmp}"
            return 1
        fi
    fi

    run_sudo install -m "${mode}" -o "${owner}" -g "${group}" "${tmp}" "${path}"
    rm -f "${tmp}"
    return 0
}

LOG_DIR="${INSTALL_ROOT}/var/log"
LOGROTATE_CONF="/etc/logrotate.d/photo-frame"
JOURNAL_CONF_DIR="/etc/systemd/journald.conf.d"
JOURNAL_DROP_IN="${JOURNAL_CONF_DIR}/photo-frame.conf"

log INFO "Ensuring photo frame log directory exists at ${LOG_DIR}"
ensure_directory "${LOG_DIR}" 755 "${SERVICE_USER}" "${SERVICE_GROUP}"

log INFO "Configuring logrotate policy at ${LOGROTATE_CONF}"
logrotate_content=$(cat <<EOF
${LOG_DIR}/*.log {
    daily
    rotate 14
    maxsize 50M
    compress
    delaycompress
    missingok
    notifempty
    sharedscripts
    su ${SERVICE_USER} ${SERVICE_GROUP}
    create 0640 ${SERVICE_USER} ${SERVICE_GROUP}
    postrotate
        systemctl kill -s SIGUSR1 ${PHOTO_SERVICE} >/dev/null 2>&1 || true
    endscript
}
EOF
)

logrotate_status=$(write_config_if_changed "${LOGROTATE_CONF}" 644 root root "${logrotate_content}") || true
case "${logrotate_status}" in
    0)
        log INFO "Logrotate policy updated"
        ;;
    1)
        log INFO "Logrotate policy already up to date"
        ;;
    2)
        log INFO "DRY_RUN: skipped writing ${LOGROTATE_CONF}"
        ;;
    *)
        log WARN "Unexpected status ${logrotate_status} while writing ${LOGROTATE_CONF}"
        ;;
esac

log INFO "Configuring systemd-journald retention"
ensure_directory "${JOURNAL_CONF_DIR}" 755 root root
journal_content=$(cat <<'EOF'
[Journal]
SystemMaxUse=200M
RuntimeMaxUse=100M
SystemMaxFileSize=50M
MaxRetentionSec=1month
EOF
)

journal_status=$(write_config_if_changed "${JOURNAL_DROP_IN}" 644 root root "${journal_content}") || true
case "${journal_status}" in
    0)
        log INFO "systemd-journald drop-in updated"
        if [[ "${DRY_RUN}" != "1" ]]; then
            log INFO "Restarting systemd-journald to apply retention settings"
            run_sudo systemctl restart systemd-journald
        fi
        ;;
    1)
        log INFO "systemd-journald drop-in already up to date"
        ;;
    2)
        log INFO "DRY_RUN: skipped writing ${JOURNAL_DROP_IN}"
        ;;
    *)
        log WARN "Unexpected status ${journal_status} while writing ${JOURNAL_DROP_IN}"
        ;;
esac

log INFO "Log rotation and journald retention configured"
